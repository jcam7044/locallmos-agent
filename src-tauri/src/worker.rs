//! Background orchestration: telemetry push, command polling/execution, and
//! desired-state reconciliation. Also hosts OS-level actions (service restart,
//! reboot) shared with the runtime adapters.

use crate::runtime::RuntimeAdapter;
use crate::AppState;
use anyhow::{anyhow, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub fn os_name() -> &'static str {
    std::env::consts::OS
}
pub fn arch_name() -> &'static str {
    std::env::consts::ARCH
}
pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Enroll this machine with a pairing code and persist the credentials.
pub async fn enroll(state: &Arc<AppState>, code: &str, name: &str) -> Result<()> {
    let resp = state
        .supabase
        .enroll(code, name, os_name(), arch_name(), AGENT_VERSION)
        .await?;
    let now = Utc::now().timestamp();
    {
        let mut cfg = state.config.lock().await;
        cfg.rig_id = Some(resp.rig_id.clone());
        cfg.token = Some(resp.token);
        cfg.refresh_secret = Some(resp.refresh_secret);
        cfg.token_expires_at = Some(now + resp.expires_in);
        cfg.rig_name = Some(name.to_string());
        cfg.save()?;
    }
    {
        let mut s = state.status.lock().await;
        s.enrolled = true;
        s.rig_id = Some(resp.rig_id);
        s.rig_name = Some(name.to_string());
        s.last_error = None;
    }
    Ok(())
}

/// Return a valid device JWT, refreshing it when missing or near expiry.
async fn ensure_token(state: &Arc<AppState>) -> Result<String> {
    let mut cfg = state.config.lock().await;
    let rig_id = cfg.rig_id.clone().ok_or_else(|| anyhow!("not enrolled"))?;
    let secret = cfg
        .refresh_secret
        .clone()
        .ok_or_else(|| anyhow!("no refresh secret"))?;
    let now = Utc::now().timestamp();
    let stale = cfg.token.is_none()
        || cfg.token_expires_at.map_or(true, |exp| exp - now < 120);
    if stale {
        let tr = state.supabase.refresh_token(&rig_id, &secret).await?;
        cfg.token = Some(tr.token);
        cfg.token_expires_at = Some(now + tr.expires_in);
        cfg.save().ok();
    }
    Ok(cfg.token.clone().unwrap())
}

async fn rig_id(state: &Arc<AppState>) -> Option<String> {
    state.config.lock().await.rig_id.clone()
}

async fn set_error(state: &Arc<AppState>, err: impl std::fmt::Display) {
    let mut s = state.status.lock().await;
    s.connected = false;
    s.last_error = Some(err.to_string());
}

/// Spawn all background loops. Called once from the Tauri setup hook. Uses
/// Tauri's async runtime so the tasks run inside its Tokio context.
pub fn spawn_loops(state: Arc<AppState>) {
    tauri::async_runtime::spawn(telemetry_loop(state.clone()));
    tauri::async_runtime::spawn(command_loop(state.clone()));
    tauri::async_runtime::spawn(reconcile_loop(state));
}

async fn telemetry_loop(state: Arc<AppState>) {
    let period = Duration::from_secs(state.settings.telemetry_interval_secs);
    loop {
        if rig_id(&state).await.is_some() {
            if let Err(e) = telemetry_tick(&state).await {
                set_error(&state, e).await;
            }
        }
        tokio::time::sleep(period).await;
    }
}

async fn telemetry_tick(state: &Arc<AppState>) -> Result<()> {
    let token = ensure_token(state).await?;
    let rid = rig_id(state).await.ok_or_else(|| anyhow!("not enrolled"))?;

    // System telemetry.
    let telemetry = {
        let mut mon = state.monitor.lock().await;
        mon.sample().await
    };
    let ts = Utc::now().to_rfc3339();
    state
        .supabase
        .post_metrics(&token, telemetry.to_insert(&rid, &ts))
        .await?;

    // Runtime + model state.
    let snap = state.ollama.snapshot().await;
    state.supabase.upsert_runtime(&token, &rid, &snap).await?;
    state.supabase.upsert_models(&token, &rid, &snap).await?;
    state
        .supabase
        .heartbeat(&token, &rid, os_name(), arch_name(), AGENT_VERSION)
        .await?;

    // Reflect into the tray status.
    {
        let mut s = state.status.lock().await;
        s.connected = true;
        s.last_error = None;
        s.runtime_kind = Some(snap.kind.clone());
        s.runtime_state = Some(snap.state.clone());
        s.loaded_model = snap.models.iter().find(|m| m.loaded).map(|m| m.name.clone());
        s.cpu_pct = telemetry.cpu_utilization_pct;
        s.gpus = telemetry.gpus.clone();
    }
    Ok(())
}

async fn command_loop(state: Arc<AppState>) {
    let period = Duration::from_secs(state.settings.command_poll_secs);
    loop {
        if rig_id(&state).await.is_some() {
            if let Err(e) = command_tick(&state).await {
                set_error(&state, e).await;
            }
        }
        tokio::time::sleep(period).await;
    }
}

async fn command_tick(state: &Arc<AppState>) -> Result<()> {
    let token = ensure_token(state).await?;
    let rid = rig_id(state).await.ok_or_else(|| anyhow!("not enrolled"))?;
    let pending = state.supabase.fetch_pending_commands(&token, &rid).await?;

    for cmd in pending {
        state
            .supabase
            .update_command(&token, &cmd.id, "running", None)
            .await
            .ok();

        let (ok, result) = execute(state, &cmd.kind, &cmd.payload).await;
        let status = if ok { "done" } else { "error" };
        state
            .supabase
            .update_command(&token, &cmd.id, status, Some(result))
            .await
            .ok();
    }
    Ok(())
}

/// Execute a single command; returns (ok, result-json).
async fn execute(state: &Arc<AppState>, kind: &str, payload: &Value) -> (bool, Value) {
    let outcome: Result<Value> = match kind {
        "set_model" => {
            let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model.is_empty() {
                Err(anyhow!("set_model: missing model"))
            } else {
                state.ollama.load_model(model).await.map(|_| json!({ "model": model }))
            }
        }
        "restart_runtime" => state.ollama.restart().await.map(|_| json!({})),
        "reboot_machine" => {
            let delay = payload.get("delaySeconds").and_then(|v| v.as_u64()).unwrap_or(0);
            reboot(delay).map(|_| json!({ "scheduled": true }))
        }
        "run_shell" => run_shell(payload),
        other => Err(anyhow!("unknown command type: {other}")),
    };

    match outcome {
        Ok(data) => (true, json!({ "ok": true, "data": data })),
        Err(e) => (false, json!({ "ok": false, "message": e.to_string() })),
    }
}

async fn reconcile_loop(state: Arc<AppState>) {
    let period = Duration::from_secs(state.settings.reconcile_secs);
    loop {
        if rig_id(&state).await.is_some() {
            if let Err(e) = reconcile_tick(&state).await {
                tracing::warn!("reconcile: {e}");
            }
        }
        tokio::time::sleep(period).await;
    }
}

/// Drive the runtime toward desired state: keep it running and the desired
/// model loaded. This is the "always running the model" guarantee.
async fn reconcile_tick(state: &Arc<AppState>) -> Result<()> {
    let token = ensure_token(state).await?;
    let rid = rig_id(state).await.ok_or_else(|| anyhow!("not enrolled"))?;
    let desired = state.supabase.fetch_desired(&token, &rid).await?;

    let snap = state.ollama.snapshot().await;

    // Restart the runtime if it should be running but isn't.
    let want_running = desired.desired_runtime_state.as_deref() != Some("stopped");
    if want_running && snap.state != "running" {
        tracing::info!("reconcile: runtime down, restarting");
        state.ollama.restart().await.ok();
        return Ok(()); // reassess next tick
    }

    // Ensure the desired model is loaded.
    if let Some(model) = desired.desired_model.as_deref() {
        let loaded = snap.models.iter().any(|m| m.name == model && m.loaded);
        if !loaded {
            tracing::info!("reconcile: loading desired model {model}");
            state.ollama.load_model(model).await.ok();
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// OS-level actions. These may require the agent to run with sufficient
// privileges; failures are reported back rather than panicking.
// ---------------------------------------------------------------------------

/// Restart a system service by name (best-effort, platform-specific).
pub async fn restart_service(name: &str) -> Result<()> {
    let (program, args): (&str, Vec<String>) = if cfg!(target_os = "windows") {
        ("powershell", vec!["-Command".into(), format!("Restart-Service {name}")])
    } else if cfg!(target_os = "macos") {
        ("brew", vec!["services".into(), "restart".into(), name.into()])
    } else {
        ("systemctl", vec!["restart".into(), name.into()])
    };
    run_os(program, &args)
}

fn reboot(delay_seconds: u64) -> Result<()> {
    if cfg!(target_os = "windows") {
        run_os("shutdown", &["/r".into(), "/t".into(), delay_seconds.to_string()])
    } else if cfg!(target_os = "macos") {
        run_os("shutdown", &["-r".into(), format!("+{}", delay_seconds / 60)])
    } else {
        let mins = (delay_seconds / 60).to_string();
        run_os("shutdown", &["-r".into(), format!("+{mins}")])
    }
}

fn run_shell(payload: &Value) -> Result<Value> {
    #[cfg(feature = "allow-shell")]
    {
        let cmd = payload
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("run_shell: missing command"))?;
        let out = if cfg!(target_os = "windows") {
            std::process::Command::new("cmd").args(["/C", cmd]).output()?
        } else {
            std::process::Command::new("sh").args(["-c", cmd]).output()?
        };
        Ok(json!({
            "exitCode": out.status.code(),
            "stdout": String::from_utf8_lossy(&out.stdout),
            "stderr": String::from_utf8_lossy(&out.stderr),
        }))
    }
    #[cfg(not(feature = "allow-shell"))]
    {
        let _ = payload;
        Err(anyhow!("run_shell is disabled on this rig"))
    }
}

fn run_os(program: &str, args: &[String]) -> Result<()> {
    let status = std::process::Command::new(program).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("{program} exited with {status}"))
    }
}
