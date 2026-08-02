//! Secret environment values for MCP servers (API tokens, connection strings),
//! stored apart from `config.json` in a `0600` sibling file so they never travel
//! in a config export or bug report. Keyed by server id, then env var name.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// server_id → (env var name → secret value)
type SecretStore = BTreeMap<String, BTreeMap<String, String>>;

fn path() -> Result<PathBuf> {
    Ok(crate::config::config_dir()?.join("mcp_secrets.json"))
}

fn load_store() -> SecretStore {
    let Ok(p) = path() else { return SecretStore::new() };
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            tracing::warn!("mcp_secrets.json is invalid ({e}); ignoring");
            SecretStore::new()
        }),
        Err(_) => SecretStore::new(),
    }
}

fn save_store(store: &SecretStore) -> Result<()> {
    let p = path()?;
    std::fs::write(&p, serde_json::to_string_pretty(store)?).context("writing mcp_secrets.json")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!("could not restrict permissions on {}: {e}", p.display());
        }
    }
    Ok(())
}

/// The secret env vars configured for `server_id`, merged over the config's
/// non-secret env at spawn time.
pub fn env_for(server_id: &str) -> BTreeMap<String, String> {
    load_store().remove(server_id).unwrap_or_default()
}

/// The env var names (not values) that have a stored secret for `server_id`, so
/// the UI can show which are set without exposing them.
pub fn keys_for(server_id: &str) -> Vec<String> {
    load_store().remove(server_id).map(|m| m.into_keys().collect()).unwrap_or_default()
}

/// Store (or overwrite) one secret. An empty value clears it.
pub fn set(server_id: &str, key: &str, value: &str) -> Result<()> {
    let mut store = load_store();
    let entry = store.entry(server_id.to_string()).or_default();
    if value.is_empty() {
        entry.remove(key);
    } else {
        entry.insert(key.to_string(), value.to_string());
    }
    if entry.is_empty() {
        store.remove(server_id);
    }
    save_store(&store)
}

/// Drop every secret for a server (on delete).
pub fn remove_server(server_id: &str) -> Result<()> {
    let mut store = load_store();
    if store.remove(server_id).is_some() {
        save_store(&store)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_and_clear_round_trip() {
        let _guard = crate::config::TEST_CONFIG_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("mcp-secrets-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("LOCALLMOS_CONFIG_DIR", &dir);

        set("gh", "GITHUB_TOKEN", "abc").unwrap();
        set("gh", "OTHER", "xyz").unwrap();
        let env = env_for("gh");
        assert_eq!(env.get("GITHUB_TOKEN").map(String::as_str), Some("abc"));
        assert_eq!(keys_for("gh").len(), 2);

        // Empty value clears just that key.
        set("gh", "OTHER", "").unwrap();
        assert_eq!(keys_for("gh"), vec!["GITHUB_TOKEN".to_string()]);

        // Removing the server clears everything.
        remove_server("gh").unwrap();
        assert!(env_for("gh").is_empty());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            set("gh", "T", "v").unwrap();
            let mode = std::fs::metadata(dir.join("mcp_secrets.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        std::env::remove_var("LOCALLMOS_CONFIG_DIR");
        std::fs::remove_dir_all(&dir).ok();
    }
}
