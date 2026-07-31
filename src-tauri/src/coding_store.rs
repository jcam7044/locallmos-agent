//! Persistent local coding sessions: one JSON file per session under
//! `<config_dir>/coding/{id}.json`. Mirrors `chat_store.rs` — the file is the
//! source of truth, and the whole feature works fully offline (no cloud).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

fn default_policy() -> String {
    "approve_writes".to_string()
}

fn default_auto_compact() -> bool {
    true
}

fn default_auto_threshold() -> u8 {
    80
}

fn new_msg_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSession {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub model: String,
    /// Absolute, canonicalized workspace root on this machine.
    pub workspace_root: String,
    #[serde(default = "default_policy")]
    pub approval_policy: String,
    /// Whether this session offers the configured MCP servers' tools to the model.
    /// Off by default so a session opts in explicitly.
    #[serde(default)]
    pub mcp_enabled: bool,
    #[serde(default)]
    pub messages: Vec<CodingStoredMessage>,
    /// Full transcript storage is independent from the active model context.
    /// The checkpoint summarizes messages through `summarized_through_message_id`.
    #[serde(default)]
    pub context_state: CodingContextState,
    /// When enrolled, the mirrored cloud `chat_conversations` id (for web
    /// pickup). Absent until the first successful sync.
    #[serde(default)]
    pub remote_id: Option<String>,
    /// The mirrored `coding_workspaces` id backing web-side continuation.
    #[serde(default)]
    pub remote_workspace_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingContextState {
    #[serde(default)]
    pub checkpoint: Option<String>,
    #[serde(default)]
    pub summarized_through_message_id: Option<String>,
    #[serde(default)]
    pub latest_used_tokens: Option<u32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub count_exact: bool,
    #[serde(default)]
    pub reserve_tokens: Option<u32>,
    /// Multiplier learned by comparing an estimated preflight with the
    /// runtime-reported prompt count after generation (primarily Ollama).
    #[serde(default)]
    pub token_estimate_scale: Option<f32>,
    #[serde(default = "default_auto_compact")]
    pub auto_compact: bool,
    #[serde(default = "default_auto_threshold")]
    pub auto_threshold: u8,
    #[serde(default)]
    pub last_compacted_at: Option<DateTime<Utc>>,
}

impl Default for CodingContextState {
    fn default() -> Self {
        Self {
            checkpoint: None,
            summarized_through_message_id: None,
            latest_used_tokens: None,
            max_tokens: None,
            count_exact: false,
            reserve_tokens: None,
            token_estimate_scale: None,
            auto_compact: default_auto_compact(),
            auto_threshold: default_auto_threshold(),
            last_compacted_at: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingStoredMessage {
    /// Stable id shared with the mirrored cloud `chat_messages` row, so pushes
    /// and pull-backs stay idempotent across the local/cloud stores.
    #[serde(default = "new_msg_id")]
    pub id: String,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub thinking: Option<String>,
    /// Persisted record of tool runs this turn (summaries), for re-render.
    #[serde(default)]
    pub tool_activity: Option<Value>,
    #[serde(default)]
    pub cancelled: bool,
    pub created_at: DateTime<Utc>,
}

impl CodingStoredMessage {
    pub fn new(role: &str, content: String) -> Self {
        Self {
            id: new_msg_id(),
            role: role.to_string(),
            content,
            thinking: None,
            tool_activity: None,
            cancelled: false,
            created_at: Utc::now(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSessionMeta {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub model: String,
    pub workspace_root: String,
    pub approval_policy: String,
    pub message_count: usize,
}

impl CodingSession {
    pub fn new(model: String, workspace_root: String, approval_policy: String) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: "New session".to_string(),
            created_at: now,
            updated_at: now,
            model,
            workspace_root,
            approval_policy,
            mcp_enabled: false,
            messages: Vec::new(),
            context_state: CodingContextState::default(),
            remote_id: None,
            remote_workspace_id: None,
        }
    }

    pub fn meta(&self) -> CodingSessionMeta {
        CodingSessionMeta {
            id: self.id.clone(),
            title: self.title.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            model: self.model.clone(),
            workspace_root: self.workspace_root.clone(),
            approval_policy: self.approval_policy.clone(),
            message_count: self.messages.iter().filter(|m| m.role != "system").count(),
        }
    }
}

fn coding_dir() -> Result<PathBuf> {
    let dir = crate::config::config_dir()?.join("coding");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn session_path(id: &str) -> Result<PathBuf> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        anyhow::bail!("invalid session id");
    }
    Ok(coding_dir()?.join(format!("{id}.json")))
}

pub fn list() -> Result<Vec<CodingSessionMeta>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(coding_dir()?)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if let Ok(session) = serde_json::from_str::<CodingSession>(&text) {
            out.push(session.meta());
        }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

pub fn load(id: &str) -> Result<CodingSession> {
    let text = std::fs::read_to_string(session_path(id)?).context("coding session not found")?;
    Ok(serde_json::from_str(&text)?)
}

pub fn save(session: &CodingSession) -> Result<()> {
    let path = session_path(&session.id)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(session)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn delete(id: &str) -> Result<()> {
    std::fs::remove_file(session_path(id)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_session_roundtrip() {
        let _lock = crate::config::TEST_CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("locallmos-coding-test-{}", uuid::Uuid::new_v4()));
        std::env::set_var("LOCALLMOS_CONFIG_DIR", &dir);

        let mut s = CodingSession::new("qwen2.5-coder".into(), "/tmp/repo".into(), "approve_writes".into());
        s.messages.push(CodingStoredMessage::new("user", "add a test".into()));
        save(&s).unwrap();

        let loaded = load(&s.id).unwrap();
        assert_eq!(loaded.workspace_root, "/tmp/repo");
        assert_eq!(loaded.messages.len(), 1);
        assert!(loaded.context_state.auto_compact);
        assert_eq!(loaded.context_state.auto_threshold, 80);

        let metas = list().unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].message_count, 1);

        delete(&s.id).unwrap();
        assert!(list().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_bad_ids() {
        assert!(session_path("../evil").is_err());
        assert!(session_path("").is_err());
    }
}
