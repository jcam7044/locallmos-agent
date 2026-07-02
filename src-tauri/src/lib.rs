//! LocalLMOS agent library entry point: wires state, background loops, and the
//! Tauri command surface consumed by the tray UI.

mod config;
mod monitor;
pub mod runtime;
mod settings;
mod status;
mod supabase;
mod worker;

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
}

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let settings = Settings::from_env();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
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

    let state = Arc::new(AppState {
        settings,
        supabase,
        config: Mutex::new(cfg),
        status: Mutex::new(status),
        ollama,
        monitor: Mutex::new(Monitor::new()),
    });

    let loop_state = state.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None::<Vec<&str>>,
        ))
        .manage(state)
        .setup(move |_app| {
            worker::spawn_loops(loop_state.clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_status, enroll])
        .run(tauri::generate_context!())
        .expect("error while running LocalLMOS agent");
}
