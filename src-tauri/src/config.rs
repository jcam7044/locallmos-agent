//! Persisted rig credentials. Written to the OS config dir after enrollment so
//! the agent reconnects automatically on restart.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentConfig {
    pub rig_id: Option<String>,
    /// Ephemeral device JWT (refreshed via `refresh_secret`).
    pub token: Option<String>,
    /// Long-lived secret used to mint fresh device tokens.
    pub refresh_secret: Option<String>,
    /// Unix seconds at which `token` expires.
    pub token_expires_at: Option<i64>,
    pub rig_name: Option<String>,
}

impl AgentConfig {
    pub fn is_enrolled(&self) -> bool {
        self.rig_id.is_some() && self.refresh_secret.is_some()
    }

    fn path() -> Result<PathBuf> {
        // Allow an explicit override so a system service and CLI enrollment
        // (which may run as different users) can share the same config file.
        let dir = match std::env::var("LOCALLMOS_CONFIG_DIR") {
            Ok(d) if !d.is_empty() => PathBuf::from(d),
            _ => dirs::config_dir().context("no config dir")?.join("locallmos-agent"),
        };
        std::fs::create_dir_all(&dir).ok();
        // Log the resolved dir once so it's discoverable which credentials store is
        // in use. The tray GUI (per-user config dir) and a headless service (its
        // own LOCALLMOS_CONFIG_DIR, e.g. /etc/locallmos-agent) are independent — see
        // SERVICE.md. This line makes a mismatch obvious in the agent's logs.
        static LOGGED: std::sync::Once = std::sync::Once::new();
        LOGGED.call_once(|| tracing::info!("agent config dir: {}", dir.display()));
        Ok(dir.join("config.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| Ok(std::fs::read_to_string(p)?))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}
