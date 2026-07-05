//! Agent self-update. Resolves a signed release artifact from `agent_releases`,
//! verifies its checksum + minisign signature, atomically swaps our own running
//! binary, and restarts. Two entry points funnel into one routine:
//!   * `apply`                — an `update_agent` command from the web app.
//!   * `check_and_auto_update` — the opt-in periodic auto-update loop.
//!
//! Because we may be replacing a root-owned binary, signature verification
//! against an embedded public key is mandatory before we ever run the download.

use crate::supabase::ReleaseArtifact;
use crate::worker::{self, AGENT_VERSION};
use crate::AppState;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Minisign public key (the base64 payload line of a `minisign.pub`) whose
/// matching secret is held only by release CI (`MINISIGN_SECRET_KEY`). Every
/// downloaded binary must carry a valid signature from this key before we run
/// it — this is what makes swapping a privileged binary safe.
///
const RELEASE_PUBLIC_KEY: &str =
    "RWR+94+uka+PJB5Wbmak5GN2J+eZjIgoj3PGFH4dAoqhBuCfIFjBy6u7";

/// "{os}-{arch}" — matches Rust's std::env::consts and the keys CI writes into
/// `agent_releases.artifacts` (e.g. "linux-x86_64", "macos-aarch64").
fn platform_key() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Handle an `update_agent` command payload: `{ version, channel }`.
pub async fn apply(state: &Arc<AppState>, payload: &Value) -> Result<Value> {
    let channel = payload
        .get("channel")
        .and_then(Value::as_str)
        .unwrap_or("stable");
    let version = payload
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("latest");
    run(state, channel, version).await
}

/// Opt-in auto-update: when the owner enabled it, install the newest release on
/// the rig's channel. Called periodically from the worker's update loop.
pub async fn check_and_auto_update(state: &Arc<AppState>) -> Result<()> {
    let token = worker::ensure_token(state).await?;
    let rid = worker::rig_id(state)
        .await
        .ok_or_else(|| anyhow!("not enrolled"))?;
    let prefs = state.supabase.fetch_update_prefs(&token, &rid).await?;
    if !prefs.auto_update {
        return Ok(());
    }
    let channel = prefs.update_channel.as_deref().unwrap_or("stable");
    let Some(latest) = state.supabase.fetch_latest_release(&token, channel).await? else {
        return Ok(());
    };
    if is_newer(&latest.version, AGENT_VERSION) {
        tracing::info!(
            "auto-update: {} -> {} on {channel}",
            AGENT_VERSION,
            latest.version
        );
        run(state, channel, &latest.version).await?;
    }
    Ok(())
}

/// Resolve → download → verify → swap → restart. Reports progress to
/// `rigs.update_status` throughout so the web app can reflect it live.
async fn run(state: &Arc<AppState>, channel: &str, version: &str) -> Result<Value> {
    let token = worker::ensure_token(state).await?;
    let rid = worker::rig_id(state)
        .await
        .ok_or_else(|| anyhow!("not enrolled"))?;

    let release = if version == "latest" {
        state
            .supabase
            .fetch_latest_release(&token, channel)
            .await?
            .ok_or_else(|| anyhow!("no {channel} release available"))?
    } else {
        state
            .supabase
            .fetch_release(&token, channel, version)
            .await?
            .ok_or_else(|| anyhow!("release {version} not found on {channel}"))?
    };

    if release.version == AGENT_VERSION {
        state
            .supabase
            .set_update_status(&token, &rid, "idle", None)
            .await
            .ok();
        return Ok(json!({ "version": AGENT_VERSION, "skipped": "already up to date" }));
    }

    let key = platform_key();
    let artifact: ReleaseArtifact = release
        .artifacts
        .get(&key)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| anyhow!("release {} has no artifact for {key}", release.version))?;

    // Fail path helper: record the error on the rig and bubble it up.
    macro_rules! fail {
        ($e:expr) => {{
            let e = $e;
            state
                .supabase
                .set_update_status(&token, &rid, "failed", Some(&e.to_string()))
                .await
                .ok();
            return Err(e);
        }};
    }

    state
        .supabase
        .set_update_status(&token, &rid, "downloading", None)
        .await
        .ok();
    let bytes = match download_and_verify(&artifact).await {
        Ok(b) => b,
        Err(e) => fail!(e),
    };

    state
        .supabase
        .set_update_status(&token, &rid, "installing", None)
        .await
        .ok();
    if let Err(e) = swap_binary(&bytes) {
        fail!(e);
    }

    tracing::info!(
        "updated agent binary {} -> {}; restarting",
        AGENT_VERSION,
        release.version
    );
    schedule_restart();
    Ok(json!({ "from": AGENT_VERSION, "to": release.version }))
}

/// Download the artifact and verify SHA-256 + minisign signature. Returns the
/// verified bytes; any mismatch is an error and nothing is written to disk.
async fn download_and_verify(artifact: &ReleaseArtifact) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let resp = client.get(&artifact.url).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("download failed: HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await?.to_vec();

    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(&bytes));
    if !digest.eq_ignore_ascii_case(artifact.sha256.trim()) {
        return Err(anyhow!(
            "sha256 mismatch: expected {}, got {digest}",
            artifact.sha256
        ));
    }

    use minisign_verify::{PublicKey, Signature};
    let pk = PublicKey::from_base64(RELEASE_PUBLIC_KEY)
        .map_err(|e| anyhow!("bad embedded public key: {e}"))?;
    let sig = Signature::decode(&artifact.sig).map_err(|e| anyhow!("bad signature: {e}"))?;
    pk.verify(&bytes, &sig, false)
        .map_err(|e| anyhow!("signature verification failed: {e}"))?;

    Ok(bytes)
}

/// Stage the verified bytes and atomically replace our own running executable.
/// `self_replace` handles the platform quirks (on Windows a running .exe can't
/// be overwritten, so it renames the old one out of the way first).
fn swap_binary(bytes: &[u8]) -> Result<()> {
    let mut staged = std::env::temp_dir();
    staged.push(format!("locallmos-agent-update-{}", std::process::id()));
    std::fs::write(&staged, bytes).context("write staged binary")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .context("chmod staged binary")?;
    }
    let result = self_replace::self_replace(&staged).context("replace running binary");
    let _ = std::fs::remove_file(&staged);
    result
}

/// Exit shortly after a successful swap so the supervising service manager
/// (systemd/launchd/Task Scheduler — all configured to restart on exit)
/// relaunches us on the new binary. The delay lets the in-flight command result
/// and status writes flush first. (A GUI tray instance relies on autostart to
/// relaunch on next login.)
fn schedule_restart() {
    tauri::async_runtime::spawn(async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        std::process::exit(0);
    });
}

/// True when `candidate` is a strictly higher semver than `current`. Any parse
/// failure returns false so a malformed version never drives a downgrade/loop.
fn is_newer(candidate: &str, current: &str) -> bool {
    match (
        semver::Version::parse(candidate.trim_start_matches('v')),
        semver::Version::parse(current.trim_start_matches('v')),
    ) {
        (Ok(c), Ok(cur)) => c > cur,
        _ => false,
    }
}
