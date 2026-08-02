//! Persisted rig credentials. Written to the OS config dir after enrollment so
//! the agent reconnects automatically on restart.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::runtime::ModelLoadSettings;

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
    /// User-selected local runtime ("ollama" | "llamacpp"). Chosen from the tray
    /// GUI; consulted at startup unless `LOCALLMOS_RUNTIME` is set (env wins, for
    /// installer/service-managed rigs). `None` → default ("ollama").
    #[serde(default)]
    pub runtime: Option<String>,
    /// A model explicitly ejected from the local UI. While the cloud still
    /// requests this same model, reconciliation leaves it unloaded. A changed
    /// desired model (or an explicit local load) clears this override.
    #[serde(default)]
    pub locally_ejected_model: Option<String>,
    /// Per-GGUF llama.cpp launch overrides. Keys are canonical paths relative to
    /// the configured models directory, so cloud aliases and local IDs converge.
    #[serde(default)]
    pub model_load_settings: BTreeMap<String, ModelLoadSettings>,
    /// Configured MCP servers. Secret env values live in `mcp_secrets.json`.
    #[serde(default)]
    pub mcp_servers: Vec<crate::mcp::McpServerConfig>,
    /// User-selected maximum number of MCP tools offered to a model. `None`
    /// preserves the recommended default so older configs migrate cleanly.
    #[serde(default)]
    pub mcp_tool_limit: Option<u16>,
}

/// Serializes tests that mutate the process-global `LOCALLMOS_CONFIG_DIR` env
/// var (chat_store / coding_store round-trips), which otherwise race under the
/// parallel test runner.
#[cfg(test)]
pub(crate) static TEST_CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Resolved agent config dir (credentials, chat sessions, …), created on demand.
pub fn config_dir() -> Result<PathBuf> {
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
    Ok(dir)
}

impl AgentConfig {
    pub fn is_enrolled(&self) -> bool {
        self.rig_id.is_some() && self.refresh_secret.is_some()
    }

    fn path() -> Result<PathBuf> {
        Ok(config_dir()?.join("config.json"))
    }

    pub fn load() -> Self {
        let path = match Self::path() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("cannot resolve config path: {e}; using defaults");
                return Self::default();
            }
        };
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            // No file yet (first run) is the normal case — start from defaults.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!("cannot read {}: {e}; using defaults", path.display());
                return Self::default();
            }
        };
        match serde_json::from_str(&raw) {
            Ok(config) => config,
            Err(e) => {
                // A corrupt/hand-edited config would otherwise silently reset to
                // defaults, discarding the rig's enrollment (rig_id +
                // refresh_secret) with no trace. Preserve the file so it can be
                // recovered, and log loudly rather than quietly un-enrolling.
                let backup = path.with_extension("json.bak");
                if let Err(be) = std::fs::rename(&path, &backup) {
                    tracing::error!(
                        "config.json is invalid ({e}) and could not be backed up ({be}); using defaults"
                    );
                } else {
                    tracing::error!(
                        "config.json is invalid ({e}); backed up to {} and using defaults",
                        backup.display()
                    );
                }
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        // The file holds `refresh_secret` (and, once configured, MCP server
        // credentials). Restrict it to the owner on unix; Windows inherits the
        // per-user config dir's ACL.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            {
                tracing::warn!("could not restrict permissions on {}: {e}", path.display());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_config_without_model_settings_remains_compatible() {
        let config: AgentConfig = serde_json::from_str(r#"{"rig_name":"old rig"}"#).unwrap();
        assert_eq!(config.rig_name.as_deref(), Some("old rig"));
        assert!(config.model_load_settings.is_empty());
        assert!(config.mcp_tool_limit.is_none());
    }

    #[test]
    fn invalid_config_is_backed_up_not_discarded_silently() {
        let _guard = TEST_CONFIG_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("llmos-cfg-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("LOCALLMOS_CONFIG_DIR", &dir);

        let path = dir.join("config.json");
        std::fs::write(&path, "{ not valid json,,, }").unwrap();

        // Load recovers to defaults rather than propagating the error…
        let config = AgentConfig::load();
        assert!(config.rig_id.is_none());
        // …and the corrupt file is preserved for recovery, not deleted.
        assert!(!path.exists(), "invalid config should have been moved aside");
        assert!(path.with_extension("json.bak").exists(), "backup should exist");

        std::env::remove_var("LOCALLMOS_CONFIG_DIR");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn saved_config_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = TEST_CONFIG_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("llmos-cfg-perm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("LOCALLMOS_CONFIG_DIR", &dir);

        AgentConfig {
            refresh_secret: Some("super-secret".into()),
            ..Default::default()
        }
        .save()
        .unwrap();

        let mode = std::fs::metadata(dir.join("config.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "config.json must not be group/world readable");

        std::env::remove_var("LOCALLMOS_CONFIG_DIR");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn model_settings_round_trip_by_canonical_key() {
        let mut config = AgentConfig::default();
        config.model_load_settings.insert(
            "huggingface/owner/model/model-Q4_K_M.gguf".into(),
            ModelLoadSettings {
                context_size: Some(16384),
                ..Default::default()
            },
        );
        let json = serde_json::to_string(&config).unwrap();
        let restored: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.model_load_settings["huggingface/owner/model/model-Q4_K_M.gguf"]
                .context_size,
            Some(16384)
        );
    }

    #[test]
    fn mcp_tool_limit_round_trips() {
        let config = AgentConfig {
            mcp_tool_limit: Some(96),
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let restored: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.mcp_tool_limit, Some(96));
    }
}
