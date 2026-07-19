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
mod chat_store;
mod config;
mod hub;
mod local_chat;
mod monitor;
mod realtime;
pub mod runtime;
mod settings;
mod status;
mod supabase;
mod updater;
mod worker;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use config::AgentConfig;
use monitor::Monitor;
use runtime::Runtime;
use serde_json::{json, Value};
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
    /// The active local LLM runtime (Ollama or llama.cpp), chosen at startup.
    pub runtime: Runtime,
    pub monitor: Mutex<Monitor>,
    pub realtime: Arc<realtime::RealtimeHandle>,
    /// In-flight chat turns → cancel flag, for stop-generation.
    pub cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Serializes chat-session file writes (save vs rename vs delete).
    pub chat_lock: Mutex<()>,
    /// Shared HTTP client, reused for the web_fetch tool (direct GET from the rig).
    pub http: reqwest::Client,
    pub hub: Arc<hub::HubState>,
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
    // Runtime precedence: env `LOCALLMOS_RUNTIME` (installer/service-managed) wins,
    // else the tray-GUI choice persisted in config, else the default.
    let runtime_kind = std::env::var("LOCALLMOS_RUNTIME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| cfg.runtime.clone())
        .unwrap_or_else(|| "ollama".into());
    let runtime = Runtime::from_kind(http.clone(), &runtime_kind);
    let hub = Arc::new(hub::HubState::new(
        http.clone(),
        runtime::llamacpp_models_dir(),
    ));

    Arc::new(AppState {
        settings,
        supabase,
        config: Mutex::new(cfg),
        status: Mutex::new(status),
        runtime,
        monitor: Mutex::new(Monitor::new()),
        realtime: Arc::new(realtime::RealtimeHandle::new()),
        cancels: Mutex::new(HashMap::new()),
        chat_lock: Mutex::new(()),
        http,
        hub,
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
// Local mode (no account): the tray drives these directly, so the app is a
// useful local LLM control panel without ever enrolling to the cloud.
// ---------------------------------------------------------------------------

/// Live local snapshot: runtime state, available models, and system telemetry.
/// Does not touch Supabase — works fully offline / unenrolled.
#[tauri::command]
async fn local_status(state: State<'_, Arc<AppState>>) -> Result<Value, String> {
    let snap = state.runtime.snapshot().await;
    let telemetry = {
        let mut mon = state.monitor.lock().await;
        mon.sample().await
    };
    let configured_runtime = state.config.lock().await.runtime.clone();
    let llama_models_dir = runtime::llamacpp_models_dir();
    let (models_disk_total, disk_available) = models_disk_space(&llama_models_dir);
    Ok(json!({
        "runtime": {
            "kind": snap.kind,
            "version": snap.version,
            "state": snap.state,
            "endpoint": snap.endpoint,
            "modelsDir": state.runtime.models_dir().or_else(|| Some(llama_models_dir.clone())),
            "contextSize": state.runtime.context_size(),
        },
        "configuredRuntime": configured_runtime,
        "models": snap.models.iter().map(|m| json!({
            "id": m.id,
            "name": m.name,
            "sizeBytes": m.size_bytes,
            "quantization": m.quantization,
            "loaded": m.loaded,
            "capabilities": m.capabilities,
            "sourceRepo": m.source_repo,
            "revision": m.revision,
            "variantId": m.variant_id,
            "files": m.files,
        })).collect::<Vec<_>>(),
        "modelsStorage": {
            "dir": llama_models_dir,
            "availableBytes": disk_available,
            "totalBytes": models_disk_total,
        },
        "telemetry": {
            "cpuPct": telemetry.cpu_utilization_pct,
            "memoryUsedBytes": telemetry.memory_used_bytes,
            "memoryTotalBytes": telemetry.memory_total_bytes,
            "gpus": telemetry.gpus,
        },
    }))
}

fn models_disk_space(models_dir: &str) -> (Option<u64>, Option<u64>) {
    let path = Path::new(models_dir);
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .iter()
        .filter(|disk| path.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().components().count())
        .map(|disk| (Some(disk.total_space()), Some(disk.available_space())))
        .unwrap_or((None, None))
}

#[tauri::command]
async fn hub_search_models(
    state: State<'_, Arc<AppState>>,
    query: String,
    capability: String,
    sort: String,
    cursor: Option<String>,
) -> Result<hub::HubModelPage, String> {
    state.hub.search(&query, &capability, &sort, cursor.as_deref()).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn hub_get_model(
    state: State<'_, Arc<AppState>>,
    repo_id: String,
) -> Result<hub::HubModelDetail, String> {
    state.hub.detail(&repo_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn hub_get_author_avatars(
    state: State<'_, Arc<AppState>>,
    authors: Vec<String>,
) -> Result<HashMap<String, String>, String> {
    let authors: std::collections::HashSet<_> = authors.into_iter().take(50).collect();
    let pairs = futures_util::future::join_all(authors.into_iter().map(|author| {
        let hub = state.hub.clone();
        async move {
            let avatar = hub.author_avatar(&author).await.ok().flatten();
            (author, avatar)
        }
    }))
    .await;
    Ok(pairs
        .into_iter()
        .filter_map(|(author, avatar)| avatar.map(|url| (author, url)))
        .collect())
}

#[tauri::command]
async fn hub_start_download(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    repo_id: String,
    revision: String,
    variant_id: String,
) -> Result<hub::DownloadState, String> {
    state.hub.start_download(app, repo_id, revision, variant_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn hub_list_downloads(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<hub::DownloadState>, String> {
    Ok(state.hub.list_downloads().await)
}

#[tauri::command]
async fn hub_cancel_download(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<hub::DownloadState, String> {
    state.hub.cancel_download(&app, &id).await.map_err(|e| e.to_string())
}

/// Load/keep a model resident in the runtime.
#[tauri::command]
async fn load_model(state: State<'_, Arc<AppState>>, model: String) -> Result<(), String> {
    state.runtime.load_model(&model).await.map_err(|e| e.to_string())
}

/// Eject a resident model from memory while retaining its local files.
#[tauri::command]
async fn unload_model(state: State<'_, Arc<AppState>>, model: String) -> Result<(), String> {
    state.runtime.unload_model(&model).await.map_err(|e| e.to_string())
}

/// Delete a completed Hub download. Loaded models must be ejected first.
#[tauri::command]
async fn delete_local_model(state: State<'_, Arc<AppState>>, model_id: String) -> Result<(), String> {
    if state.runtime.snapshot().await.models.iter().any(|model| model.id == model_id && model.loaded) {
        return Err("eject this model before removing its files".into());
    }
    runtime::llama_server::delete_hub_model(&runtime::llamacpp_models_dir(), &model_id)
        .map_err(|error| error.to_string())
}

/// Restart the local runtime service.
#[tauri::command]
async fn restart_runtime(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.runtime.restart().await.map_err(|e| e.to_string())
}

/// Persist the user's local-runtime choice ("ollama" | "llamacpp"). Takes effect
/// on the next launch (the active `Runtime` is built at startup); the GUI prompts
/// for a restart. No-op vs. env: if `LOCALLMOS_RUNTIME` is set it still wins.
#[tauri::command]
async fn set_runtime(state: State<'_, Arc<AppState>>, kind: String) -> Result<(), String> {
    if kind != "ollama" && kind != "llamacpp" {
        return Err(format!("unknown runtime: {kind}"));
    }
    let mut cfg = state.config.lock().await;
    cfg.runtime = Some(kind);
    cfg.save().map_err(|e| e.to_string())
}

/// Open the current runtime's models directory in the OS file manager (llama.cpp
/// only — Ollama manages its own store).
#[tauri::command]
async fn open_models_dir(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let dir = state
        .runtime
        .models_dir()
        .ok_or("this runtime has no models directory")?;
    std::fs::create_dir_all(&dir).ok();
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Run one persisted local chat turn. Deltas stream as `local-chat` events
/// (payloads carry `requestId`); the final assistant message is returned and
/// already saved to the session file.
#[tauri::command]
async fn local_chat_send(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
    request_id: String,
    content: String,
    attachments: Option<Vec<chat_store::Attachment>>,
    regenerate: Option<bool>,
) -> Result<chat_store::StoredMessage, String> {
    local_chat::send(
        app,
        state.inner().clone(),
        session_id,
        request_id,
        content,
        attachments.unwrap_or_default(),
        regenerate.unwrap_or(false),
    )
    .await
}

/// Stop an in-flight local chat turn; the partial reply is still persisted.
#[tauri::command]
async fn local_chat_cancel(
    state: State<'_, Arc<AppState>>,
    request_id: String,
) -> Result<(), String> {
    if let Some(flag) = state.cancels.lock().await.get(&request_id) {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

// --- Persistent chat sessions (local, on-disk) ------------------------------

#[tauri::command]
async fn chat_list_sessions(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<chat_store::SessionMeta>, String> {
    let _guard = state.chat_lock.lock().await;
    chat_store::list().map_err(|e| e.to_string())
}

#[tauri::command]
async fn chat_create_session(
    state: State<'_, Arc<AppState>>,
    model: String,
) -> Result<chat_store::ChatSession, String> {
    let _guard = state.chat_lock.lock().await;
    let session = chat_store::ChatSession::new(model);
    chat_store::save(&session).map_err(|e| e.to_string())?;
    Ok(session)
}

#[tauri::command]
async fn chat_get_session(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<chat_store::ChatSession, String> {
    let _guard = state.chat_lock.lock().await;
    chat_store::load(&id).map_err(|e| e.to_string())
}

/// Rename keeps `updated_at` untouched so the sidebar order doesn't jump.
#[tauri::command]
async fn chat_rename_session(
    state: State<'_, Arc<AppState>>,
    id: String,
    title: String,
) -> Result<(), String> {
    let _guard = state.chat_lock.lock().await;
    let mut session = chat_store::load(&id).map_err(|e| e.to_string())?;
    session.title = title;
    chat_store::save(&session).map_err(|e| e.to_string())
}

#[tauri::command]
async fn chat_delete_session(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let _guard = state.chat_lock.lock().await;
    chat_store::delete(&id).map_err(|e| e.to_string())
}

/// Patch a session's model + generation settings (toggles, system prompt, …).
#[tauri::command]
async fn chat_update_settings(
    state: State<'_, Arc<AppState>>,
    id: String,
    model: String,
    settings: chat_store::SessionSettings,
) -> Result<(), String> {
    let _guard = state.chat_lock.lock().await;
    let mut session = chat_store::load(&id).map_err(|e| e.to_string())?;
    session.model = model;
    session.settings = settings;
    chat_store::save(&session).map_err(|e| e.to_string())
}

/// Read a locally dropped file (drag-drop delivers paths, not contents) into a
/// chat attachment: images inline as base64, UTF-8 files as capped text.
#[tauri::command]
async fn read_dropped_file(path: String) -> Result<chat_store::Attachment, String> {
    chat_store::attachment_from_path(&path).map_err(|e| e.to_string())
}

/// Check GitHub Releases directly (no account) and self-update if a newer version
/// exists. Returns the new version when it updated, `None` when already current.
#[tauri::command]
async fn local_update() -> Result<Option<String>, String> {
    crate::updater::self_update_from_github()
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
        .invoke_handler(tauri::generate_handler![
            get_status,
            enroll,
            local_status,
            load_model,
            unload_model,
            delete_local_model,
            restart_runtime,
            set_runtime,
            open_models_dir,
            hub_search_models,
            hub_get_model,
            hub_get_author_avatars,
            hub_start_download,
            hub_list_downloads,
            hub_cancel_download,
            local_chat_send,
            local_chat_cancel,
            local_update,
            chat_list_sessions,
            chat_create_session,
            chat_get_session,
            chat_rename_session,
            chat_delete_session,
            chat_update_settings,
            read_dropped_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running LocalLMOS agent");
}
