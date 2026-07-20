//! Background orchestration: telemetry push, command polling/execution, and
//! desired-state reconciliation. Also hosts OS-level actions (service restart,
//! reboot) shared with the runtime adapters.

use crate::config::AgentConfig;
use crate::status::AgentStatus;
use crate::supabase::CredentialsRevoked;
use crate::AppState;
use anyhow::{anyhow, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::path::Path;
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
pub async fn ensure_token(state: &Arc<AppState>) -> Result<String> {
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
        match state.supabase.refresh_token(&rig_id, &secret).await {
            Ok(tr) => {
                cfg.token = Some(tr.token);
                cfg.token_expires_at = Some(now + tr.expires_in);
                cfg.save().ok();
            }
            Err(e) => {
                // If the refresh secret was revoked (rig deleted from the
                // dashboard), wipe local credentials and return to pairing.
                // Other errors are transient — leave enrollment intact.
                if e.downcast_ref::<CredentialsRevoked>().is_some() {
                    drop(cfg); // release before reset_to_pairing re-locks config
                    reset_to_pairing(state).await;
                }
                return Err(e);
            }
        }
    }
    Ok(cfg.token.clone().unwrap())
}

/// Wipe local enrollment and drop the tray UI back to the pairing screen.
/// Called when the rig no longer exists server-side (owner deleted it) or its
/// credentials were revoked. Idempotent: safe to call from any loop.
pub async fn reset_to_pairing(state: &Arc<AppState>) {
    {
        let mut cfg = state.config.lock().await;
        if !cfg.is_enrolled() {
            return; // already reset by another loop
        }
        tracing::warn!("rig removed server-side; clearing credentials and returning to pairing");
        *cfg = AgentConfig::default();
        cfg.save().ok();
    }
    let mut s = state.status.lock().await;
    *s = AgentStatus::default();
    s.last_error =
        Some("This rig was removed from the dashboard. Enter a new pairing code to reconnect.".into());
}

pub async fn rig_id(state: &Arc<AppState>) -> Option<String> {
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
    // A freshly started process is never mid-update, so clear any 'installing'
    // status the previous (pre-restart) process left behind. Runs once, before
    // the auto-update loop's initial delay, so it can't race a real update.
    tauri::async_runtime::spawn(clear_update_status(state.clone()));
    tauri::async_runtime::spawn(telemetry_loop(state.clone()));
    tauri::async_runtime::spawn(reconcile_loop(state.clone()));
    // Commands + chat arrive instantly over Realtime; the fallback poll is a
    // safety net for reconnects / missed events.
    tauri::async_runtime::spawn(fallback_loop(state.clone()));
    tauri::async_runtime::spawn(update_loop(state.clone()));
    tauri::async_runtime::spawn(crate::realtime::run(state));
}

/// One-shot: settle `update_status` back to `idle` after a restart.
async fn clear_update_status(state: Arc<AppState>) {
    if rig_id(&state).await.is_none() {
        return;
    }
    if let (Ok(token), Some(rid)) = (ensure_token(&state).await, rig_id(&state).await) {
        state
            .supabase
            .set_update_status(&token, &rid, "idle", None)
            .await
            .ok();
    }
}

/// Opt-in auto-update poll. Delayed at startup so it doesn't compete with
/// enrollment / the first heartbeat; the work itself no-ops unless the owner
/// enabled `auto_update` for this rig.
async fn update_loop(state: Arc<AppState>) {
    tokio::time::sleep(Duration::from_secs(30)).await;
    let period = Duration::from_secs(state.settings.update_check_secs);
    loop {
        if rig_id(&state).await.is_some() {
            if let Err(e) = crate::updater::check_and_auto_update(&state).await {
                tracing::debug!("auto-update check: {e}");
            }
        }
        tokio::time::sleep(period).await;
    }
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

    // Heartbeat first: it doubles as an "is this rig still enrolled?" probe. If
    // the owner deleted the rig from the dashboard, the row is gone and the
    // child-table writes below would fail their foreign key — so detect it here
    // and drop back to pairing instead of erroring in a loop.
    let exists = state
        .supabase
        .heartbeat(&token, &rid, os_name(), arch_name(), AGENT_VERSION)
        .await?;
    if !exists {
        reset_to_pairing(state).await;
        return Ok(());
    }

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
    let snap = state.runtime.snapshot().await;
    state.supabase.upsert_runtime(&token, &rid, &snap).await?;
    state.supabase.upsert_models(&token, &rid, &snap).await?;

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

/// Safety-net poll (30s): catches commands/chat missed while the Realtime
/// socket was down or reconnecting. The claim step makes this idempotent with
/// the Realtime path.
async fn fallback_loop(state: Arc<AppState>) {
    let period = Duration::from_secs(30);
    loop {
        if rig_id(&state).await.is_some() {
            if let Err(e) = fallback_tick(&state).await {
                tracing::debug!("fallback poll: {e}");
            }
        }
        tokio::time::sleep(period).await;
    }
}

async fn fallback_tick(state: &Arc<AppState>) -> Result<()> {
    let token = ensure_token(state).await?;
    let rid = rig_id(state).await.ok_or_else(|| anyhow!("not enrolled"))?;

    for cmd in state.supabase.fetch_pending_commands(&token, &rid).await? {
        process_command(state, &cmd.id, &cmd.kind, &cmd.payload).await.ok();
    }
    for pending in state.supabase.fetch_pending_chat(&token, &rid).await? {
        crate::chat::process(state, pending).await.ok();
    }
    Ok(())
}

/// Claim + execute a single command. Safe to call from both the Realtime
/// handler and the fallback poll — only the caller that wins the claim runs it.
pub async fn process_command(
    state: &Arc<AppState>,
    id: &str,
    kind: &str,
    payload: &Value,
) -> Result<()> {
    let token = ensure_token(state).await?;
    if !state.supabase.claim_command(&token, id).await? {
        return Ok(());
    }
    let (ok, result) = execute(state, kind, payload).await;
    let status = if ok { "done" } else { "error" };
    state
        .supabase
        .update_command(&token, id, status, Some(result))
        .await
        .ok();
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
                state.load_model_configured(model, false).await.map(|_| json!({ "model": model }))
            }
        }
        "restart_runtime" => state.runtime.restart().await.map(|_| json!({})),
        "reboot_machine" => {
            let delay = payload.get("delaySeconds").and_then(|v| v.as_u64()).unwrap_or(0);
            reboot(delay).map(|_| json!({ "scheduled": true }))
        }
        "run_shell" => run_shell(payload),
        "update_agent" => crate::updater::apply(state, payload).await,
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

    let snap = state.runtime.snapshot().await;

    // Restart the runtime if it should be running but isn't.
    let want_running = desired.desired_runtime_state.as_deref() != Some("stopped");
    if want_running && snap.state != "running" {
        tracing::info!("reconcile: runtime down, restarting");
        state.runtime.restart().await.ok();
        return Ok(()); // reassess next tick
    }

    // An explicit local Eject takes precedence over the unchanged cloud
    // desired-model value. This is persisted so a restarted agent does not
    // immediately consume VRAM again. Selecting a different desired model in
    // the web app clears the override on the next reconcile.
    if let Some(model) = desired.desired_model.as_deref() {
        let suppressed = {
            let mut config = state.config.lock().await;
            match config.locally_ejected_model.as_deref() {
                Some(ejected) if same_model(ejected, model) => true,
                Some(_) => {
                    config.locally_ejected_model = None;
                    config.save().ok();
                    false
                }
                None => false,
            }
        };
        if suppressed {
            tracing::debug!("reconcile: leaving locally ejected model {model} unloaded");
            return Ok(());
        }
        let loaded = snap.models.iter().any(|m| m.name == model && m.loaded);
        if !loaded {
            tracing::info!("reconcile: loading desired model {model}");
            state.load_model_configured(model, false).await.ok();
        }
    }
    Ok(())
}

fn same_model(left: &str, right: &str) -> bool {
    fn normalized(value: &str) -> String {
        Path::new(value)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(value)
            .trim_end_matches(".gguf")
            .to_ascii_lowercase()
    }
    normalized(left) == normalized(right)
}

// ---------------------------------------------------------------------------
// OS-level actions. These may require the agent to run with sufficient
// privileges; failures are reported back rather than panicking.
// ---------------------------------------------------------------------------

/// Restart a local runtime by name (best-effort, platform-specific). Only Linux
/// runs Ollama as a managed service (`systemctl`). On macOS and Windows Ollama is
/// normally a per-user desktop app, not a service, so we try a real service first
/// (e.g. one set up via Homebrew or NSSM) and otherwise bounce the app itself. A
/// headless root/SYSTEM daemon can't perfectly drive a user-session GUI app, so
/// callers treat a failure here as advisory rather than fatal.
pub async fn restart_service(name: &str) -> Result<()> {
    if cfg!(target_os = "windows") {
        // No Ollama Windows service exists by default; try one anyway (NSSM or a
        // custom install), then fall back to bouncing the tray app, which is what
        // supervises `ollama serve`.
        if run_os(
            "powershell",
            &["-Command".into(), format!("Restart-Service {name} -ErrorAction Stop")],
        )
        .is_ok()
        {
            return Ok(());
        }
        let _ = run_os("taskkill", &["/F".into(), "/IM".into(), "ollama app.exe".into()]);
        let _ = run_os("taskkill", &["/F".into(), "/IM".into(), "ollama.exe".into()]);
        run_os("powershell", &["-Command".into(), "Start-Process 'ollama app.exe'".into()])
    } else if cfg!(target_os = "macos") {
        // Homebrew service when installed via brew; otherwise bounce the official
        // .app — `open -a` (re)launches it, which starts its bundled server.
        if run_os("brew", &["services".into(), "restart".into(), name.into()]).is_ok() {
            return Ok(());
        }
        let _ = run_os("killall", &["Ollama".into()]);
        run_os("open", &["-a".into(), "Ollama".into()])
    } else {
        run_os("systemctl", &["restart".into(), name.into()])
    }
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

#[cfg(test)]
mod tests {
    use super::same_model;

    #[test]
    fn locally_ejected_file_id_matches_cloud_model_alias() {
        assert!(same_model(
            "huggingface/owner/repo/model-Q4_K_M.gguf",
            "model-Q4_K_M"
        ));
        assert!(!same_model("model-Q4_K_M.gguf", "model-Q5_K_M"));
    }
}
