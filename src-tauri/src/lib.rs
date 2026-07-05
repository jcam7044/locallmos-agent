//! LocalLMOS agent library entry point.
//!
//! One binary, three modes (chosen by argv):
//!   * (no args)                     → GUI tray app (enrollment + status)
//!   * `service` / `--headless`      → run worker loops headless (for systemd)
//!   * `enroll --code X --name Y`     → enroll headlessly, then exit
//!
//! The worker loops (telemetry, commands, reconcile) are identical across GUI
//! and service modes; only the shell differs.

mod chat;
mod config;
mod monitor;
mod realtime;
pub mod runtime;
mod settings;
mod status;
mod supabase;
mod updater;
mod worker;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use config::AgentConfig;
use monitor::Monitor;
use runtime::ollama::OllamaAdapter;
use settings::Settings;
use status::AgentStatus;
use supabase::Supabase;
use tauri::State;
use tauri_plugin_autostart::MacosLauncher;
use tokio::sync::Mutex;

/// Shared application state. `Arc<AppState>` is both managed by Tauri (for
/// commands) and cloned into the background loops.
pub struct AppState {
    pub settings: Settings,
    pub supabase: Supabase,
    pub config: Mutex<AgentConfig>,
    pub status: Mutex<AgentStatus>,
    pub ollama: OllamaAdapter,
    pub monitor: Mutex<Monitor>,
    pub realtime: Arc<realtime::RealtimeHandle>,
    /// In-flight chat turns → cancel flag, for stop-generation.
    pub cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

fn build_state() -> Arc<AppState> {
    let settings = Settings::from_env();
    // Use connect + idle-read timeouts rather than a single total-request
    // deadline: fast-fail the short polling/Supabase calls when a host is down,
    // but don't cap streaming chat, where the model can take a while to load
    // before the first token and a long generation can outlast any fixed total.
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(300))
        .build()
        .expect("failed to build HTTP client");
    let supabase = Supabase::new(http.clone(), &settings.supabase_url, &settings.anon_key);

    let cfg = AgentConfig::load();
    let status = AgentStatus {
        enrolled: cfg.is_enrolled(),
        rig_id: cfg.rig_id.clone(),
        rig_name: cfg.rig_name.clone(),
        ..Default::default()
    };
    let ollama = OllamaAdapter::new(http.clone());

    Arc::new(AppState {
        settings,
        supabase,
        config: Mutex::new(cfg),
        status: Mutex::new(status),
        ollama,
        monitor: Mutex::new(Monitor::new()),
        realtime: Arc::new(realtime::RealtimeHandle::new()),
        cancels: Mutex::new(HashMap::new()),
    })
}

// ---------------------------------------------------------------------------
// Tauri commands (GUI)
// ---------------------------------------------------------------------------
#[tauri::command]
async fn get_status(state: State<'_, Arc<AppState>>) -> Result<AgentStatus, String> {
    Ok(state.status.lock().await.clone())
}

#[tauri::command]
async fn enroll(
    state: State<'_, Arc<AppState>>,
    code: String,
    name: String,
) -> Result<(), String> {
    worker::enroll(state.inner(), &code, &name)
        .await
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Entry point + mode dispatch
// ---------------------------------------------------------------------------
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Load apps/agent/.env if present (searches CWD upward); real env wins.
    let _ = dotenvy::dotenv();
    init_tracing();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("service") | Some("--service") | Some("--headless") => run_service(),
        Some("enroll") => run_enroll(&args[1..]),
        Some("--help") | Some("-h") => print_help(),
        // Includes the no-args GUI launch and the autostart `--minimized` case.
        _ => run_gui(),
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
}

fn print_help() {
    println!(
        "LocaLLMOS agent\n\n\
         USAGE:\n  \
         locallmos-agent                       launch the tray GUI\n  \
         locallmos-agent service               run headless (for systemd/launchd)\n  \
         locallmos-agent enroll --code <CODE> --name <NAME>\n\n\
         Config dir override: LOCALLMOS_CONFIG_DIR\n\
         Supabase: LOCALLMOS_SUPABASE_URL, LOCALLMOS_SUPABASE_ANON_KEY"
    );
}

/// Headless service mode: run the worker loops forever. Requires prior enrollment.
fn run_service() {
    let state = build_state();
    let enrolled =
        tauri::async_runtime::block_on(async { state.config.lock().await.is_enrolled() });
    if !enrolled {
        eprintln!(
            "agent is not enrolled. Run:\n  locallmos-agent enroll --code <CODE> --name <NAME>"
        );
        std::process::exit(1);
    }
    tracing::info!("starting LocalLMOS agent (service mode)");
    worker::spawn_loops(state);
    // Park the main thread; spawned loops run on the async runtime's workers.
    tauri::async_runtime::block_on(std::future::pending::<()>());
}

/// Headless enrollment: `enroll --code <CODE> --name <NAME>`.
fn run_enroll(args: &[String]) {
    let mut code = None;
    let mut name = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--code" => {
                code = args.get(i + 1).cloned();
                i += 2;
            }
            "--name" => {
                name = args.get(i + 1).cloned();
                i += 2;
            }
            _ => i += 1,
        }
    }
    let (Some(code), Some(name)) = (code, name) else {
        eprintln!("usage: locallmos-agent enroll --code <CODE> --name <NAME>");
        std::process::exit(2);
    };

    let state = build_state();
    match tauri::async_runtime::block_on(worker::enroll(&state, &code, &name)) {
        Ok(()) => println!("enrolled successfully as '{name}'"),
        Err(e) => {
            eprintln!("enroll failed: {e}");
            std::process::exit(1);
        }
    }
}

/// GUI tray app. Started with `--minimized` (by autostart) it launches hidden.
fn run_gui() {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;
    use tauri::{Manager, WindowEvent};

    let start_hidden = std::env::args().any(|a| a == "--minimized");
    let state = build_state();
    let loop_state = state.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            // Autostart launches minimized to tray.
            Some(vec!["--minimized"]),
        ))
        .manage(state)
        .setup(move |app| {
            worker::spawn_loops(loop_state.clone());

            // Best-effort: enable launch-on-login so the tray survives reboots
            // on interactive machines. (Headless rigs use the systemd service.)
            #[cfg(desktop)]
            {
                use tauri_plugin_autostart::ManagerExt;
                let _ = app.autolaunch().enable();
            }

            // Tray icon + menu built in code so we can wire menu events.
            let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("LocalLMOS Agent")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            if start_hidden {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            Ok(())
        })
        // Closing the window hides to tray instead of quitting the agent.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![get_status, enroll])
        .run(tauri::generate_context!())
        .expect("error while running LocalLMOS agent");
}
