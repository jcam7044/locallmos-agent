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
mod coding;
mod coding_store;
mod config;
mod local_coding;
mod hardware;
mod hub;
mod local_chat;
mod llamacpp_updater;
mod mcp;
mod monitor;
mod peers;
mod realtime;
mod relay_inference;
pub mod runtime;
mod settings;
mod status;
mod subagent;
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
use runtime::{ModelLoadSettings, Runtime};
use serde_json::{json, Value};
use settings::Settings;
use status::AgentStatus;
use supabase::Supabase;
use tauri::{Emitter, State};
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
    /// True only while a signed llama.cpp update owns the runtime lifecycle.
    pub llamacpp_update_running: AtomicBool,
    /// Serializes model process lifecycle changes with install-tree swaps.
    pub runtime_lifecycle: Mutex<()>,
    /// Pending local coding-tool approvals → decision sender. Resolved by the
    /// `coding_local_approve` command (in-process; no cloud round-trip).
    pub coding_approvals: Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    /// Serializes chat-session file writes (save vs rename vs delete).
    pub chat_lock: Mutex<()>,
    /// Shared HTTP client, reused for the web_fetch tool (direct GET from the rig).
    pub http: reqwest::Client,
    /// Desktop-only localhost preview windows and managed workspace dev servers.
    pub preview: Arc<coding::preview::PreviewManager>,
    /// Configured MCP servers and their live tool snapshot. Empty until the user
    /// configures servers (phase 3); inert for turns that don't enable MCP.
    pub mcp: Arc<mcp::McpManager>,
    /// Last-applied version per cloud-authored MCP server (serverId → version),
    /// so the reconcile loop re-applies a web-configured server only when its
    /// config or secret actually changed. In-memory: an empty map on restart just
    /// means the first reconcile re-applies everything (idempotent).
    pub mcp_cloud_applied: Mutex<HashMap<String, String>>,
    pub hub: Arc<hub::HubState>,
    /// Serving peers in this rig's group that can run relayed sub-agent
    /// inference, and the round-robin cursor for load-balancing across them.
    pub peers: Arc<peers::PeerPool>,
    /// Whether *this* rig currently accepts relayed inference jobs (owner-set in
    /// the cloud; mirrored here each reconcile so the serving loop can gate).
    pub serves_inference: AtomicBool,
    /// Caps concurrent inbound inference jobs so a group fan-out can't swamp one
    /// machine. Sized from `LOCALLMOS_INFERENCE_SLOTS` (default 2).
    pub serving_slots: Arc<tokio::sync::Semaphore>,
    /// The Tauri app handle, set once at GUI startup. Absent in headless service
    /// mode. Used to mirror coding-session stream events to the local webview.
    pub app: std::sync::OnceLock<tauri::AppHandle>,
}

impl AppState {
    pub(crate) async fn model_settings(
        &self,
        model: &str,
    ) -> anyhow::Result<(String, ModelLoadSettings)> {
        let key = self.runtime.canonical_model_id(model)?;
        let settings = self
            .config
            .lock()
            .await
            .model_load_settings
            .get(&key)
            .cloned()
            .unwrap_or_default();
        Ok((key, settings))
    }

    pub(crate) async fn load_model_configured(
        &self,
        model: &str,
        force_reload: bool,
    ) -> anyhow::Result<()> {
        let _lifecycle = self.runtime_lifecycle.lock().await;
        if self
            .llamacpp_update_running
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            anyhow::bail!("llama.cpp is being updated; try again when the update finishes");
        }
        let (_, settings) = self.model_settings(model).await?;
        if force_reload && self.runtime.is_model_loaded(model).await {
            self.runtime.unload_model(model).await?;
        }
        self.runtime.load_model_configured(model, &settings).await
    }
}

fn build_state() -> Arc<AppState> {
    llamacpp_updater::recover_interrupted_update();
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
    // One-shot hardware sanity check: warn if the provisioned llama.cpp backend
    // looks wrong for the GPU we can see (the Unsloth detect_hardware() analog).
    if runtime_kind == "llamacpp" {
        if let Some(backend) = runtime::llama_server::active_backend() {
            hardware::warn_on_mismatch(&backend);
        }
    }
    let hub = Arc::new(hub::HubState::new(
        http.clone(),
        runtime::llamacpp_models_dir(),
    ));

    let preview = coding::preview::PreviewManager::new(http.clone());
    let initial_mcp_limit = cfg
        .mcp_tool_limit
        .map(usize::from)
        .unwrap_or(mcp::DEFAULT_MCP_TOOL_LIMIT);
    let mcp = mcp::McpManager::new_with_tool_limit(env!("CARGO_PKG_VERSION"), initial_mcp_limit);
    Arc::new(AppState {
        settings,
        supabase,
        config: Mutex::new(cfg),
        status: Mutex::new(status),
        runtime,
        monitor: Mutex::new(Monitor::new()),
        realtime: Arc::new(realtime::RealtimeHandle::new()),
        cancels: Mutex::new(HashMap::new()),
        llamacpp_update_running: AtomicBool::new(false),
        runtime_lifecycle: Mutex::new(()),
        coding_approvals: Mutex::new(HashMap::new()),
        chat_lock: Mutex::new(()),
        http,
        preview,
        mcp,
        mcp_cloud_applied: Mutex::new(HashMap::new()),
        hub,
        peers: Arc::new(peers::PeerPool::new()),
        serves_inference: AtomicBool::new(false),
        serving_slots: Arc::new(tokio::sync::Semaphore::new(serving_slot_count())),
        app: std::sync::OnceLock::new(),
    })
}

/// Concurrent inbound inference jobs this rig will run at once.
fn serving_slot_count() -> usize {
    std::env::var("LOCALLMOS_INFERENCE_SLOTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(2)
        .clamp(1, 32)
}

// ---------------------------------------------------------------------------
// Tauri commands (GUI)
// ---------------------------------------------------------------------------
#[tauri::command]
async fn get_status(state: State<'_, Arc<AppState>>) -> Result<AgentStatus, String> {
    Ok(state.status.lock().await.clone())
}

/// Lightweight, local-only snapshot for the one-second Resources graphs.
#[tauri::command]
async fn system_metrics_snapshot(
    state: State<'_, Arc<AppState>>,
) -> Result<monitor::SystemMetricsSnapshot, String> {
    let telemetry = {
        let mut monitor = state.monitor.lock().await;
        monitor.sample().await
    };
    Ok(monitor::SystemMetricsSnapshot::from(&telemetry))
}

/// Build the Resources window hidden. Creating it once at startup (rather than
/// lazily on the tray click) sidesteps a WebView2 race that can leave a
/// freshly-created window blank, and lets it live alongside the main window:
/// closing it only hides it (see the window-close handler), so reopening is an
/// instant `show()`. The webview pauses its metric polling while hidden.
fn build_resources_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    use tauri::WebviewUrl;

    tauri::WebviewWindowBuilder::new(
        app,
        "resources",
        WebviewUrl::App("index.html?view=resources".into()),
    )
    .title("System Resources — LocalLMOS")
    .inner_size(980.0, 700.0)
    .min_inner_size(760.0, 520.0)
    .resizable(true)
    .visible(false)
    .build()
    .map_err(|e| e.to_string())
}

fn show_resources_window(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;

    // Normally the window was pre-created at startup; rebuild it if it is somehow
    // missing (e.g. it failed to build then).
    let window = match app.get_webview_window("resources") {
        Some(window) => window,
        None => build_resources_window(app)?,
    };
    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn open_resources_window(app: tauri::AppHandle) -> Result<(), String> {
    show_resources_window(&app)
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
    let context_size = state.runtime.context_size().await;
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
            "backend": snap.backend,
            "state": snap.state,
            "endpoint": snap.endpoint,
            "modelsDir": state.runtime.models_dir().or_else(|| Some(llama_models_dir.clone())),
            "contextSize": context_size,
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
    state.load_model_configured(&model, false).await.map_err(|e| e.to_string())?;
    let mut config = state.config.lock().await;
    if config.locally_ejected_model.is_some() {
        config.locally_ejected_model = None;
        config.save().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Eject a resident model from memory while retaining its local files.
#[tauri::command]
async fn unload_model(state: State<'_, Arc<AppState>>, model: String) -> Result<(), String> {
    let _lifecycle = state.runtime_lifecycle.lock().await;
    state.runtime.unload_model(&model).await.map_err(|e| e.to_string())?;
    let mut config = state.config.lock().await;
    config.locally_ejected_model = Some(model);
    config.save().map_err(|e| e.to_string())?;
    Ok(())
}

/// Delete a locally discovered model. Loaded models must be ejected first.
#[tauri::command]
async fn delete_local_model(state: State<'_, Arc<AppState>>, model_id: String) -> Result<(), String> {
    if state.runtime.snapshot().await.models.iter().any(|model| model.id == model_id && model.loaded) {
        return Err("eject this model before removing its files".into());
    }
    let key = state.runtime.canonical_model_id(&model_id).map_err(|error| error.to_string())?;
    if state.runtime.kind() == "ollama" {
        state
            .runtime
            .delete_model(&model_id)
            .await
            .map_err(|error| error.to_string())?;
    } else {
        runtime::llama_server::delete_local_model(&runtime::llamacpp_models_dir(), &model_id)
            .map_err(|error| error.to_string())?;
    }
    let mut config = state.config.lock().await;
    if config.model_load_settings.remove(&key).is_some() {
        config.save().map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Pull a registry model into Ollama's managed model store.
#[tauri::command]
async fn ollama_pull_model(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    model: String,
) -> Result<(), String> {
    state
        .runtime
        .pull_ollama_model(&model, |progress| {
            let _ = app.emit("ollama-pull", progress);
        })
        .await
        .map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// MCP server management
// ---------------------------------------------------------------------------

/// One server plus the secret env var names it has set (values never leave the
/// backend). Combines runtime status with the persisted config for the UI.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct McpServerView {
    #[serde(flatten)]
    status: mcp::McpServerStatus,
    transport: mcp::McpTransport,
    disabled_tools: Vec<String>,
    catalog_id: Option<String>,
    secret_keys: Vec<String>,
    tools: Vec<McpToolView>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct McpToolView {
    name: String,
    qualified: String,
    description: String,
    enabled: bool,
    available: bool,
    mutating: bool,
    schema_tokens: u32,
}

/// A catalog entry plus the exact base command (with `{placeholder}` tokens
/// intact) so the install dialog can show what will run before it runs.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogEntryView {
    #[serde(flatten)]
    entry: &'static mcp::catalog::CatalogEntry,
    command: String,
}

/// Full picture for the Tools tab: configured servers with live status + tools,
/// the snapshot's truncation count, and the catalog with runtime detection.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct McpOverview {
    servers: Vec<McpServerView>,
    truncated: usize,
    tool_limit: usize,
    available_tools: usize,
    active_schema_tokens: u32,
    available_schema_tokens: u32,
    runtimes: mcp::catalog::RuntimeAvailability,
    catalog: Vec<CatalogEntryView>,
}

async fn build_overview(state: &Arc<AppState>) -> McpOverview {
    let configs = state.config.lock().await.mcp_servers.clone();
    let statuses = state.mcp.statuses().await;
    let snapshot = state.mcp.snapshot();
    let inventory = state.mcp.inventory().await;
    let enabled_inventory: Vec<&mcp::McpToolDef> = inventory
        .iter()
        .filter(|tool| {
            configs
                .iter()
                .find(|server| server.id == tool.server_id)
                .map(|server| server.is_tool_enabled(&tool.tool_name))
                .unwrap_or(false)
        })
        .collect();
    let available_tools = enabled_inventory.len();
    let available_schema_tokens = enabled_inventory
        .iter()
        .map(|tool| mcp::estimated_schema_tokens(tool))
        .sum();
    let active_schema_tokens = snapshot
        .tools
        .iter()
        .map(mcp::estimated_schema_tokens)
        .sum();

    let servers = configs
        .into_iter()
        .map(|c| {
            let status = statuses
                .iter()
                .find(|s| s.id == c.id)
                .cloned()
                .unwrap_or_else(|| mcp::McpServerStatus {
                    id: c.id.clone(),
                    label: c.label.clone(),
                    enabled: c.enabled,
                    trust: c.trust,
                    status: mcp::McpStatus::Stopped,
                    tool_count: 0,
                    last_error: None,
                });
            let tools = inventory
                .iter()
                .filter(|t| t.server_id == c.id)
                .map(|t| McpToolView {
                    name: t.tool_name.clone(),
                    qualified: t.qualified.clone(),
                    description: t.description.clone(),
                    enabled: c.is_tool_enabled(&t.tool_name),
                    available: snapshot.find(&t.qualified).is_some(),
                    mutating: t.mutating,
                    schema_tokens: mcp::estimated_schema_tokens(t),
                })
                .collect();
            McpServerView {
                status,
                transport: c.transport.clone(),
                disabled_tools: c.disabled_tools.clone(),
                catalog_id: c.catalog_id.clone(),
                secret_keys: mcp::secret_keys_for(&c.id),
                tools,
            }
        })
        .collect();

    let empty = std::collections::BTreeMap::new();
    let catalog = mcp::catalog::CATALOG
        .iter()
        .map(|entry| CatalogEntryView { entry, command: entry.preview_command(&empty) })
        .collect();

    McpOverview {
        servers,
        truncated: snapshot.truncated,
        tool_limit: state.mcp.tool_limit(),
        available_tools,
        active_schema_tokens,
        available_schema_tokens,
        runtimes: mcp::catalog::detect_runtimes(),
        catalog,
    }
}

/// Persist the server list, then re-register it with the manager. Callers save
/// under the config lock and reconcile the manager after.
async fn persist_and_reconcile(
    state: &Arc<AppState>,
    servers: Vec<mcp::McpServerConfig>,
) -> Result<(), String> {
    {
        let mut config = state.config.lock().await;
        config.mcp_servers = servers.clone();
        config.save().map_err(|e| e.to_string())?;
    }
    state.mcp.set_configs(servers).await;
    // Reflect the new server set to the cloud (best-effort, non-blocking).
    let bg = Arc::clone(state);
    tauri::async_runtime::spawn(async move { local_coding::sync_mcp_to_cloud(&bg).await });
    Ok(())
}

#[tauri::command]
async fn mcp_overview(state: State<'_, Arc<AppState>>) -> Result<McpOverview, String> {
    Ok(build_overview(&state).await)
}

#[tauri::command]
async fn mcp_set_tool_limit(
    state: State<'_, Arc<AppState>>,
    limit: u16,
) -> Result<McpOverview, String> {
    if !(1..=mcp::MAX_MCP_TOOL_LIMIT as u16).contains(&limit) {
        return Err(format!("MCP tool limit must be between 1 and {}", mcp::MAX_MCP_TOOL_LIMIT));
    }
    {
        let mut config = state.config.lock().await;
        config.mcp_tool_limit = Some(limit);
        config.save().map_err(|error| error.to_string())?;
    }
    state.mcp.set_tool_limit(usize::from(limit)).await;
    let background = state.inner().clone();
    tauri::async_runtime::spawn(async move { local_coding::sync_mcp_to_cloud(&background).await });
    Ok(build_overview(&state).await)
}

#[tauri::command]
async fn mcp_add_server(
    state: State<'_, Arc<AppState>>,
    config: mcp::McpServerConfig,
) -> Result<McpOverview, String> {
    config.validate()?;
    let mut servers = state.config.lock().await.mcp_servers.clone();
    if servers.iter().any(|s| s.id == config.id) {
        return Err(format!("a server with id '{}' already exists", config.id));
    }
    servers.push(config);
    persist_and_reconcile(&state, servers).await?;
    Ok(build_overview(&state).await)
}

#[tauri::command]
async fn mcp_install_catalog_entry(
    state: State<'_, Arc<AppState>>,
    catalog_id: String,
    server_id: String,
    inputs: std::collections::BTreeMap<String, String>,
) -> Result<McpOverview, String> {
    let entry = mcp::catalog::find(&catalog_id).ok_or_else(|| format!("unknown catalog entry: {catalog_id}"))?;
    let (config, secrets) = entry.to_config(&server_id, &inputs).map_err(|e| e.to_string())?;
    config.validate()?;
    {
        let servers = state.config.lock().await.mcp_servers.clone();
        if servers.iter().any(|s| s.id == config.id) {
            return Err(format!("a server with id '{}' already exists", config.id));
        }
    }
    // Write secrets before persisting the config so a crash never leaves a
    // running server referencing a missing token.
    for (key, value) in &secrets {
        mcp::set_secret(&server_id, key, value).map_err(|e| e.to_string())?;
    }
    let mut servers = state.config.lock().await.mcp_servers.clone();
    servers.push(config);
    persist_and_reconcile(&state, servers).await?;
    Ok(build_overview(&state).await)
}

#[tauri::command]
async fn mcp_update_server(
    state: State<'_, Arc<AppState>>,
    id: String,
    enabled: Option<bool>,
    trust: Option<mcp::McpTrust>,
    disabled_tools: Option<Vec<String>>,
    label: Option<String>,
) -> Result<McpOverview, String> {
    let mut servers = state.config.lock().await.mcp_servers.clone();
    let server = servers.iter_mut().find(|s| s.id == id).ok_or_else(|| format!("no such server: {id}"))?;
    if let Some(v) = enabled {
        server.enabled = v;
    }
    if let Some(v) = trust {
        server.trust = v;
    }
    if let Some(v) = disabled_tools {
        server.disabled_tools = v;
    }
    if let Some(v) = label {
        server.label = v;
    }
    server.validate()?;
    persist_and_reconcile(&state, servers).await?;
    // Rebuild the snapshot so a trust/enablement change takes effect immediately
    // for running servers.
    if state.mcp.statuses().await.iter().any(|s| s.id == id && s.status == mcp::McpStatus::Running) {
        state.mcp.start(&id).await.ok();
    }
    Ok(build_overview(&state).await)
}

#[tauri::command]
async fn mcp_set_secret(
    state: State<'_, Arc<AppState>>,
    id: String,
    key: String,
    value: String,
) -> Result<(), String> {
    // Only accept a secret for a configured server.
    let known = state.config.lock().await.mcp_servers.iter().any(|s| s.id == id);
    if !known {
        return Err(format!("no such server: {id}"));
    }
    mcp::set_secret(&id, &key, &value).map_err(|e| e.to_string())
}

#[tauri::command]
async fn mcp_delete_server(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<McpOverview, String> {
    let servers: Vec<mcp::McpServerConfig> = state
        .config
        .lock()
        .await
        .mcp_servers
        .iter()
        .filter(|s| s.id != id)
        .cloned()
        .collect();
    state.mcp.stop(&id).await.ok();
    mcp::remove_secrets(&id).map_err(|e| e.to_string())?;
    persist_and_reconcile(&state, servers).await?;
    Ok(build_overview(&state).await)
}

#[tauri::command]
async fn mcp_start_server(state: State<'_, Arc<AppState>>, id: String) -> Result<McpOverview, String> {
    state.mcp.start(&id).await.map_err(|e| e.to_string())?;
    // A server's tools become known only after it starts — re-advertise.
    let bg = state.inner().clone();
    tauri::async_runtime::spawn(async move { local_coding::sync_mcp_to_cloud(&bg).await });
    Ok(build_overview(&state).await)
}

#[tauri::command]
async fn mcp_stop_server(state: State<'_, Arc<AppState>>, id: String) -> Result<McpOverview, String> {
    state.mcp.stop(&id).await.map_err(|e| e.to_string())?;
    let bg = state.inner().clone();
    tauri::async_runtime::spawn(async move { local_coding::sync_mcp_to_cloud(&bg).await });
    Ok(build_overview(&state).await)
}

#[tauri::command]
async fn mcp_server_logs(state: State<'_, Arc<AppState>>, id: String) -> Result<String, String> {
    state.mcp.server_logs(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_model_load_settings(
    state: State<'_, Arc<AppState>>,
    model_id: String,
) -> Result<ModelLoadSettings, String> {
    if state.runtime.kind() != "llamacpp" {
        return Err("model load settings are only available for llama.cpp".into());
    }
    state
        .model_settings(&model_id)
        .await
        .map(|(_, settings)| settings)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn save_model_load_settings(
    state: State<'_, Arc<AppState>>,
    model_id: String,
    settings: ModelLoadSettings,
    load_now: bool,
) -> Result<(), String> {
    if state.runtime.kind() != "llamacpp" {
        return Err("model load settings are only available for llama.cpp".into());
    }
    settings.validate().map_err(|error| error.to_string())?;
    let key = state.runtime.canonical_model_id(&model_id).map_err(|error| error.to_string())?;
    {
        let mut config = state.config.lock().await;
        if settings.is_recommended() {
            config.model_load_settings.remove(&key);
        } else {
            config.model_load_settings.insert(key, settings);
        }
        config.save().map_err(|error| error.to_string())?;
    }
    if load_now {
        state
            .load_model_configured(&model_id, true)
            .await
            .map_err(|error| format!("settings were saved, but the model failed to load: {error}"))?;
        let mut config = state.config.lock().await;
        if config.locally_ejected_model.take().is_some() {
            config.save().map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Restart the local runtime service.
#[tauri::command]
async fn restart_runtime(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let _lifecycle = state.runtime_lifecycle.lock().await;
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

#[tauri::command]
async fn chat_context(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<local_chat::ChatContextInfo, String> {
    local_chat::context_info(state.inner(), &session_id).await
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

// --- Coding sessions (cloud-backed, continuable from the web) ---------------
// These bridge the local webview to Supabase using the device JWT. Sessions are
// chat_conversations of kind='coding'; the same agent process claims and runs
// the pending turns (chat.rs), streaming to the webview via `coding` events.

/// Validate a folder on this rig and register it as a coding workspace. Returns
/// `{ workspaceId }`.
#[tauri::command]
async fn coding_register_workspace(
    state: State<'_, Arc<AppState>>,
    name: String,
    path: String,
    approval_policy: Option<String>,
) -> Result<Value, String> {
    // Fail fast with a clear message if the folder is missing/inaccessible here.
    coding::Workspace::new(&path).map_err(|e| e.to_string())?;
    let token = worker::ensure_token(state.inner()).await.map_err(|e| e.to_string())?;
    let body = json!({
        "action": "register_workspace",
        "name": name,
        "rootPath": path,
        "approvalPolicy": approval_policy.unwrap_or_else(|| "approve_writes".into()),
    });
    state.supabase.coding_turn(&token, body).await.map_err(|e| e.to_string())
}

/// Registered coding workspaces on this rig.
#[tauri::command]
async fn coding_list_workspaces(state: State<'_, Arc<AppState>>) -> Result<Value, String> {
    let token = worker::ensure_token(state.inner()).await.map_err(|e| e.to_string())?;
    let rig = worker::rig_id(state.inner()).await.ok_or("not enrolled")?;
    state.supabase.list_coding_workspaces(&token, &rig).await.map_err(|e| e.to_string())
}

/// Start a new coding session against a workspace. Returns `{ conversationId, assistantId }`.
#[tauri::command]
async fn coding_start_session(
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    model: String,
    prompt: String,
) -> Result<Value, String> {
    let token = worker::ensure_token(state.inner()).await.map_err(|e| e.to_string())?;
    let body = json!({ "action": "start", "workspaceId": workspace_id, "model": model, "prompt": prompt });
    state.supabase.coding_turn(&token, body).await.map_err(|e| e.to_string())
}

/// Send another turn in an existing coding session. Returns `{ assistantId }`.
#[tauri::command]
async fn coding_send(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    prompt: String,
    model: Option<String>,
) -> Result<Value, String> {
    let token = worker::ensure_token(state.inner()).await.map_err(|e| e.to_string())?;
    let mut body = json!({ "action": "continue", "conversationId": conversation_id, "prompt": prompt });
    if let Some(m) = model {
        body["model"] = json!(m);
    }
    state.supabase.coding_turn(&token, body).await.map_err(|e| e.to_string())
}

/// Coding sessions on this rig (for the sidebar).
#[tauri::command]
async fn coding_list_sessions(state: State<'_, Arc<AppState>>) -> Result<Value, String> {
    let token = worker::ensure_token(state.inner()).await.map_err(|e| e.to_string())?;
    let rig = worker::rig_id(state.inner()).await.ok_or("not enrolled")?;
    state.supabase.list_coding_sessions(&token, &rig).await.map_err(|e| e.to_string())
}

/// All messages in a coding session (for rendering on open / resume).
#[tauri::command]
async fn coding_get_session(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<Value, String> {
    let token = worker::ensure_token(state.inner()).await.map_err(|e| e.to_string())?;
    state.supabase.get_coding_messages(&token, &conversation_id).await.map_err(|e| e.to_string())
}

/// Approve or deny a paused coding tool invocation from the local app.
#[tauri::command]
async fn coding_approve(
    state: State<'_, Arc<AppState>>,
    invocation_id: String,
    decision: String,
) -> Result<(), String> {
    if decision != "approved" && decision != "denied" {
        return Err("decision must be 'approved' or 'denied'".into());
    }
    let token = worker::ensure_token(state.inner()).await.map_err(|e| e.to_string())?;
    state.supabase.set_tool_decision(&token, &invocation_id, &decision).await.map_err(|e| e.to_string())
}

/// Stop an in-flight coding turn (also releases a pending approval wait).
#[tauri::command]
async fn coding_cancel(state: State<'_, Arc<AppState>>, message_id: String) -> Result<(), String> {
    if let Some(flag) = state.cancels.lock().await.get(&message_id) {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

// --- Local coding sessions (offline, on-disk; no cloud required) ------------

/// Create an on-disk coding session bound to a workspace folder on this machine.
/// The path is validated + canonicalized here (rejects a missing folder).
#[tauri::command]
async fn coding_local_create_session(
    state: State<'_, Arc<AppState>>,
    workspace_path: String,
    model: String,
    approval_policy: Option<String>,
) -> Result<coding_store::CodingSession, String> {
    let workspace = coding::Workspace::new(&workspace_path).map_err(|e| e.to_string())?;
    let session = {
        let _guard = state.chat_lock.lock().await;
        let session = coding_store::CodingSession::new(
            model,
            workspace.root_str(),
            approval_policy.unwrap_or_else(|| "approve_writes".into()),
        );
        coding_store::save(&session).map_err(|e| e.to_string())?;
        session
    };
    // Mirror the empty session up front so it shows on the web Code page before
    // its first turn; `send` pushes again with the transcript. Best-effort, and
    // only outside the guard above — `push_to_cloud` takes `chat_lock` itself to
    // persist the remote ids, and the lock is not reentrant.
    local_coding::push_to_cloud(state.inner(), &session).await;
    Ok(session)
}

/// Change a session's approval mode mid-conversation. The next turn picks it up
/// when `local_coding::send` rebuilds the context, so it also governs which
/// tools the model is offered.
#[tauri::command]
async fn coding_local_set_policy(
    state: State<'_, Arc<AppState>>,
    id: String,
    policy: String,
) -> Result<coding_store::CodingSession, String> {
    // `parse` falls back to approve_writes for anything unknown, which would
    // silently *widen* the mode on a typo — round-trip to reject that instead.
    let parsed = coding::ApprovalPolicy::parse(&policy);
    if parsed.as_str() != policy {
        return Err(format!("unknown approval policy: {policy}"));
    }
    let session = {
        let _guard = state.chat_lock.lock().await;
        let mut s = coding_store::load(&id).map_err(|e| e.to_string())?;
        s.approval_policy = parsed.as_str().to_string();
        s.updated_at = chrono::Utc::now();
        coding_store::save(&s).map_err(|e| e.to_string())?;
        s
    };
    local_coding::push_to_cloud(state.inner(), &session).await;
    Ok(session)
}

/// Toggle whether a coding session offers the configured MCP servers' tools.
#[tauri::command]
async fn coding_local_set_mcp_enabled(
    state: State<'_, Arc<AppState>>,
    id: String,
    enabled: bool,
) -> Result<coding_store::CodingSession, String> {
    let session = {
        let _guard = state.chat_lock.lock().await;
        let mut s = coding_store::load(&id).map_err(|e| e.to_string())?;
        s.mcp_enabled = enabled;
        s.updated_at = chrono::Utc::now();
        coding_store::save(&s).map_err(|e| e.to_string())?;
        s
    };
    Ok(session)
}

/// Set the reasoning effort for a coding session. `None` disables reasoning.
#[tauri::command]
async fn coding_local_set_reasoning_effort(
    state: State<'_, Arc<AppState>>,
    id: String,
    effort: Option<crate::runtime::ReasoningEffort>,
) -> Result<coding_store::CodingSession, String> {
    let session = {
        let _guard = state.chat_lock.lock().await;
        let mut s = coding_store::load(&id).map_err(|e| e.to_string())?;
        s.reasoning_effort = effort;
        s.updated_at = chrono::Utc::now();
        coding_store::save(&s).map_err(|e| e.to_string())?;
        s
    };
    Ok(session)
}

/// Validate paths picked from the native dialog against the session's workspace,
/// returning them workspace-relative. Anything outside the root is rejected —
/// the agent's tools could not read it anyway, so silently accepting it would
/// produce attachments the model can never open.
#[tauri::command]
async fn coding_local_attach(
    state: State<'_, Arc<AppState>>,
    id: String,
    paths: Vec<String>,
) -> Result<Vec<String>, String> {
    let session = {
        let _guard = state.chat_lock.lock().await;
        coding_store::load(&id).map_err(|e| e.to_string())?
    };
    let workspace = coding::Workspace::new(&session.workspace_root).map_err(|e| e.to_string())?;
    paths
        .iter()
        .map(|p| workspace.relativize(p).map_err(|e| e.to_string()))
        .collect()
}

#[tauri::command]
async fn coding_local_list_sessions(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<coding_store::CodingSessionMeta>, String> {
    let _guard = state.chat_lock.lock().await;
    coding_store::list().map_err(|e| e.to_string())
}

#[tauri::command]
async fn coding_local_get_session(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<coding_store::CodingSession, String> {
    let _guard = state.chat_lock.lock().await;
    coding_store::load(&id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn coding_local_context(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<local_coding::CodingContextInfo, String> {
    local_coding::get_context(&app, state.inner(), &session_id).await
}

#[tauri::command]
async fn coding_local_compact(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<local_coding::CodingContextInfo, String> {
    local_coding::compact(&app, state.inner(), &session_id).await
}

#[tauri::command]
async fn coding_local_set_context_settings(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
    auto_compact: bool,
    auto_threshold: u8,
) -> Result<local_coding::CodingContextInfo, String> {
    local_coding::set_context_settings(
        &app,
        state.inner(),
        &session_id,
        auto_compact,
        auto_threshold,
    ).await
}

#[tauri::command]
async fn coding_local_delete_session(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    {
        let _guard = state.chat_lock.lock().await;
        coding_store::delete(&id).map_err(|e| e.to_string())?;
    }
    if let Some(app) = state.app.get() {
        state.preview.close_session(app, &id, true).await.map_err(|e| e.to_string())?;
    }
    // Drop the cloud mirror too, or the session lingers on the web Code page.
    // Best-effort, and outside the guard so a slow or offline round-trip does
    // not hold the store lock; an unenrolled rig skips it entirely.
    local_coding::delete_from_cloud(state.inner(), &id).await;
    Ok(())
}

/// A sub-agent as shown in the Code tab's Agents panel.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentView {
    name: String,
    description: String,
    tools: Vec<String>,
    prompt: String,
    /// "builtin" | "project" | "global".
    scope: String,
    editable: bool,
    /// Per-agent exploration round budget, or null to use the default.
    max_rounds: Option<usize>,
}

/// List the sub-agents available to a session's workspace: built-in `explore`
/// plus project (`.agents/`) and global (`<config>/agents/`) files.
#[tauri::command]
async fn coding_list_agents(workspace_root: String) -> Result<Vec<AgentView>, String> {
    let ws = coding::Workspace::new(&workspace_root).map_err(|e| e.to_string())?;
    Ok(coding::list_agents(&ws)
        .into_iter()
        .map(|a| AgentView {
            name: a.def.name,
            description: a.def.description,
            tools: a.def.tools,
            prompt: a.def.system_prompt,
            scope: a.scope,
            editable: a.editable,
            max_rounds: a.def.max_rounds,
        })
        .collect())
}

/// Create or overwrite a custom agent file (project or global). `tools` is
/// normalized to the read-only set; the built-in `explore` name is rejected.
#[tauri::command]
async fn coding_save_agent(
    workspace_root: String,
    scope: String,
    name: String,
    description: String,
    prompt: String,
    tools: Vec<String>,
    max_rounds: Option<usize>,
) -> Result<(), String> {
    coding::save_agent(
        coding::AgentScope::parse(&scope),
        std::path::Path::new(&workspace_root),
        &name,
        &description,
        &prompt,
        &tools,
        max_rounds,
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Delete a custom agent file (project or global).
#[tauri::command]
async fn coding_delete_agent(workspace_root: String, scope: String, name: String) -> Result<(), String> {
    coding::delete_agent(
        coding::AgentScope::parse(&scope),
        std::path::Path::new(&workspace_root),
        &name,
    )
    .map_err(|e| e.to_string())
}

/// Distributed sub-agent status for the coding UI: whether this rig offloads
/// sub-agents to the group, whether it serves inference to peers (owner-set in
/// the dashboard), and the serving peers it can currently reach.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupSubagentStatus {
    /// Consumer-side opt-in: dispatch this rig's sub-agents to group peers.
    enabled: bool,
    /// Producer-side: this rig accepts relayed jobs (owner-set, cloud-mirrored).
    serving: bool,
    peers: Vec<peers::PeerInfo>,
}

#[tauri::command]
async fn coding_peer_status(
    state: State<'_, Arc<AppState>>,
) -> Result<GroupSubagentStatus, String> {
    let state = state.inner().clone();
    let enabled = state.config.lock().await.use_group_subagents;
    let serving = state.serves_inference.load(std::sync::atomic::Ordering::Relaxed);
    let peers = state.peers.snapshot(&state).await;
    Ok(GroupSubagentStatus { enabled, serving, peers })
}

/// Toggle whether this rig offloads its coding sub-agents to serving group
/// peers. Consumer-side preference, stored locally (serving is owner-set in the
/// web dashboard). Off by default — sub-agent prompts carry workspace code.
#[tauri::command]
async fn coding_set_use_group_subagents(
    state: State<'_, Arc<AppState>>,
    enabled: bool,
) -> Result<(), String> {
    let mut cfg = state.config.lock().await;
    cfg.use_group_subagents = enabled;
    cfg.save().map_err(|e| e.to_string())
}

#[tauri::command]
async fn coding_preview_status(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<coding::preview::PreviewStatus, String> {
    Ok(state.preview.status(&app, &session_id).await)
}

#[tauri::command]
async fn coding_preview_focus(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    state.preview.focus(&app, &session_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn coding_preview_reload(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    state.preview.reload(&app, &session_id).await.map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn coding_preview_close(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    state.preview.close_session(&app, &session_id, true).await.map_err(|e| e.to_string())
}

/// Run one local coding turn. Deltas stream as `local-coding` events (carrying
/// `sessionId`/`messageId`); the assistant message is returned once persisted.
#[tauri::command]
async fn coding_local_send(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
    request_id: String,
    content: String,
) -> Result<coding_store::CodingStoredMessage, String> {
    local_coding::send(app, state.inner().clone(), session_id, request_id, content).await
}

/// Approve or deny a paused local coding tool call.
#[tauri::command]
async fn coding_local_approve(
    state: State<'_, Arc<AppState>>,
    invocation_id: String,
    approved: bool,
) -> Result<(), String> {
    if let Some(tx) = state.coding_approvals.lock().await.remove(&invocation_id) {
        let _ = tx.send(approved);
    }
    Ok(())
}

/// Stop an in-flight local coding turn (also releases a pending approval wait).
#[tauri::command]
async fn coding_local_cancel(state: State<'_, Arc<AppState>>, request_id: String) -> Result<(), String> {
    if let Some(flag) = state.cancels.lock().await.get(&request_id) {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

/// Check GitHub Releases directly (no account, no download) for a newer agent
/// version. Returns the version delta plus the OS-appropriate install command so
/// the desktop app can show a copy-command update toast; `None` when current.
/// The GUI can't overwrite its own privileged binary, so it points the user at
/// the installer command rather than self-updating in place.
#[tauri::command]
async fn agent_check_update() -> Result<Option<updater::AgentUpdateInfo>, String> {
    crate::updater::check_for_update()
        .await
        .map_err(|e| e.to_string())
}

/// Check the signed LocalLMOS llama.cpp stable channel without downloading a
/// runtime artifact. Returns `None` when the managed install is current.
#[tauri::command]
async fn llamacpp_check_update(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<llamacpp_updater::LlamaCppUpdateInfo>, String> {
    llamacpp_updater::check(state.inner())
        .await
        .map_err(|error| error.to_string())
}

/// Download and install the exact signed release the user accepted. The
/// backend re-fetches the channel catalog so stale/tampered UI input cannot
/// choose an arbitrary artifact URL.
#[tauri::command]
async fn llamacpp_install_update(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    tag: String,
) -> Result<(), String> {
    llamacpp_updater::install(app, state.inner().clone(), tag)
        .await
        .map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Entry point + mode dispatch
// ---------------------------------------------------------------------------
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Load apps/agent/.env if present (searches CWD upward); real env wins.
    let _ = dotenvy::dotenv();
    init_tracing();
    prefer_x11_backend();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("service") | Some("--service") | Some("--headless") => run_service(),
        Some("enroll") => run_enroll(&args[1..]),
        Some("--help") | Some("-h") => print_help(),
        // Includes the no-args GUI launch and the autostart `--minimized` case.
        _ => run_gui(),
    }
}

/// On Linux, run GTK on X11 (via XWayland) rather than the native Wayland
/// backend. tao 0.35's Wayland client-side decorations have a bug where the
/// titlebar minimize/maximize/close buttons go unresponsive — most visibly with
/// more than one window open (see tauri-apps/tauri#13440, fixed upstream in tao
/// 0.36, which no released Tauri ships yet). The X11 decoration path is
/// unaffected. Must run before any GTK init (i.e. before the Tauri builder).
///
/// We only set the backend when the user hasn't chosen one, so anyone who
/// deliberately forces `GDK_BACKEND=wayland` keeps control. Remove this once we
/// upgrade to a Tauri that bundles tao >= 0.36.
fn prefer_x11_backend() {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("GDK_BACKEND").is_none() {
            std::env::set_var("GDK_BACKEND", "x11");
        }
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
    let window_state = state.clone();
    let exit_state = state.clone();

    let app = tauri::Builder::default()
        // Native file/folder pickers for attaching workspace context.
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            // Autostart launches minimized to tray.
            Some(vec!["--minimized"]),
        ))
        .manage(state.clone())
        .setup(move |app| {
            // Give background loops (coding turns) a handle to mirror stream
            // events to the local webview.
            loop_state.app.set(app.handle().clone()).ok();
            worker::spawn_loops(loop_state.clone());

            // Heal coding sessions that never reached the cloud (created while
            // offline or unenrolled) so they show up on the web Code page.
            {
                let state = loop_state.clone();
                tauri::async_runtime::spawn(local_coding::backfill_unsynced(state));
            }

            // Register configured MCP servers with the manager (lazy-started on
            // first turn that enables them; nothing is spawned here), then
            // advertise them to the cloud so the dashboard and web-initiated turns
            // see them. Advertising on startup is what makes the mirror self-heal
            // — the other sync triggers only fire on a config or lifecycle change,
            // so without this a rig that was configured before the mcp-sync
            // function existed would never appear on the web.
            {
                let state = loop_state.clone();
                tauri::async_runtime::spawn(async move {
                    let configs = state.config.lock().await.mcp_servers.clone();
                    state.mcp.set_configs(configs).await;
                    local_coding::sync_mcp_to_cloud(&state).await;
                });
            }

            // Best-effort: enable launch-on-login so the tray survives reboots
            // on interactive machines. (Headless rigs use the systemd service.)
            #[cfg(desktop)]
            {
                use tauri_plugin_autostart::ManagerExt;
                let _ = app.autolaunch().enable();
            }

            // Tray icon + menu built in code so we can wire menu events.
            let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
            let resources =
                MenuItem::with_id(app, "resources", "Resources", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &resources, &quit])?;
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
                    "resources" => {
                        if let Err(error) = show_resources_window(app) {
                            tracing::warn!("failed to open Resources window: {error}");
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

            // Pre-create the Resources window (hidden) so the tray just shows it
            // and it can sit open next to the main window. Best-effort: if it
            // fails, `show_resources_window` will build it on demand instead.
            if let Err(error) = build_resources_window(app.handle()) {
                tracing::warn!("failed to pre-create Resources window: {error}");
            }
            Ok(())
        })
        .on_window_event(move |window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // The main and Resources windows hide to the tray so they persist
                // and reopen instantly; preview windows really close, tearing down
                // their managed development server.
                if matches!(window.label(), "main" | "resources") {
                    let _ = window.hide();
                    api.prevent_close();
                } else {
                    let state = window_state.clone();
                    let app = window.app_handle().clone();
                    let label = window.label().to_string();
                    tauri::async_runtime::spawn(async move {
                        if let Some(session_id) = state.preview.session_for_window(&label).await {
                            state.preview.close_session(&app, &session_id, false).await.ok();
                        }
                    });
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            system_metrics_snapshot,
            open_resources_window,
            enroll,
            local_status,
            load_model,
            get_model_load_settings,
            save_model_load_settings,
            mcp_overview,
            mcp_set_tool_limit,
            mcp_add_server,
            mcp_install_catalog_entry,
            mcp_update_server,
            mcp_set_secret,
            mcp_delete_server,
            mcp_start_server,
            mcp_stop_server,
            mcp_server_logs,
            unload_model,
            delete_local_model,
            ollama_pull_model,
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
            chat_context,
            agent_check_update,
            llamacpp_check_update,
            llamacpp_install_update,
            chat_list_sessions,
            chat_create_session,
            chat_get_session,
            chat_rename_session,
            chat_delete_session,
            chat_update_settings,
            read_dropped_file,
            coding_register_workspace,
            coding_list_workspaces,
            coding_start_session,
            coding_send,
            coding_list_sessions,
            coding_get_session,
            coding_approve,
            coding_cancel,
            coding_local_create_session,
            coding_local_set_policy,
            coding_local_set_mcp_enabled,
            coding_local_set_reasoning_effort,
            coding_local_attach,
            coding_local_list_sessions,
            coding_local_get_session,
            coding_local_context,
            coding_local_compact,
            coding_local_set_context_settings,
            coding_local_delete_session,
            coding_local_send,
            coding_local_approve,
            coding_local_cancel,
            coding_list_agents,
            coding_save_agent,
            coding_delete_agent,
            coding_peer_status,
            coding_set_use_group_subagents,
            coding_preview_status,
            coding_preview_focus,
            coding_preview_reload,
            coding_preview_close
        ])
        .build(tauri::generate_context!())
        .expect("error while running LocalLMOS agent");
    app.run(move |handle, event| {
        if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
            tauri::async_runtime::block_on(async {
                exit_state.preview.stop_all(handle).await;
                // Terminate every MCP server process tree so none are orphaned.
                exit_state.mcp.stop_all().await;
            });
        }
    });
}
