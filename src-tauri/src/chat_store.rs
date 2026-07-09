//! Persistent local chat sessions: one JSON file per session under
//! `<config_dir>/chats/{id}.json`. The file is the source of truth for a
//! conversation; the frontend only reads/writes through the Tauri commands.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub model: String,
    #[serde(default)]
    pub settings: SessionSettings,
    #[serde(default)]
    pub messages: Vec<StoredMessage>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSettings {
    pub system_prompt: Option<String>,
    pub temperature: Option<f32>,
    pub num_ctx: Option<u32>,
    #[serde(default)]
    pub think: bool,
    #[serde(default)]
    pub web_tools: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    pub thinking: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub tool_activity: Option<Value>,
    #[serde(default)]
    pub cancelled: bool,
    pub created_at: DateTime<Utc>,
}

impl StoredMessage {
    pub fn new(role: &str, content: String) -> Self {
        Self {
            role: role.to_string(),
            content,
            thinking: None,
            attachments: Vec::new(),
            prompt_tokens: None,
            completion_tokens: None,
            tool_activity: None,
            cancelled: false,
            created_at: Utc::now(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// "image" | "text"
    pub kind: String,
    pub name: String,
    pub mime: String,
    pub size_bytes: u64,
    /// Base64 payload for images.
    pub data: Option<String>,
    /// Extracted text for text files (capped at ingestion).
    pub text: Option<String>,
}

/// Lightweight listing entry for the sidebar.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub model: String,
    pub message_count: usize,
}

impl ChatSession {
    pub fn new(model: String) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: "New chat".to_string(),
            created_at: now,
            updated_at: now,
            model,
            settings: SessionSettings::default(),
            messages: Vec::new(),
        }
    }

    pub fn meta(&self) -> SessionMeta {
        SessionMeta {
            id: self.id.clone(),
            title: self.title.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            model: self.model.clone(),
            message_count: self.messages.len(),
        }
    }
}

fn chats_dir() -> Result<PathBuf> {
    let dir = crate::config::config_dir()?.join("chats");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn session_path(id: &str) -> Result<PathBuf> {
    // Ids are uuids we minted; reject anything that could escape the chats dir.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        anyhow::bail!("invalid session id");
    }
    Ok(chats_dir()?.join(format!("{id}.json")))
}

pub fn list() -> Result<Vec<SessionMeta>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(chats_dir()?)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        // Skip unparseable files rather than failing the whole listing.
        if let Ok(session) = serde_json::from_str::<ChatSession>(&text) {
            out.push(session.meta());
        }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

pub fn load(id: &str) -> Result<ChatSession> {
    let text = std::fs::read_to_string(session_path(id)?).context("session not found")?;
    Ok(serde_json::from_str(&text)?)
}

pub fn save(session: &ChatSession) -> Result<()> {
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

/// Cap on inlined text-file content (chars); larger files are truncated.
pub const TEXT_ATTACHMENT_CAP: usize = 32 * 1024;
/// Cap on image attachment size (bytes) — inline base64 in the session JSON.
const IMAGE_ATTACHMENT_CAP: u64 = 15 * 1024 * 1024;

const IMAGE_EXTS: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
];

/// Build an `Attachment` from a local file path (native drag-drop hands the
/// webview paths, not file contents). Kind is sniffed from the extension:
/// known image types are inlined as base64, anything else is treated as text
/// (with a UTF-8 validity check so binaries are rejected).
pub fn attachment_from_path(path: &str) -> Result<Attachment> {
    use base64::Engine;

    let p = std::path::Path::new(path);
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    if let Some((_, mime)) = IMAGE_EXTS.iter().find(|(e, _)| *e == ext) {
        let meta = std::fs::metadata(p).context("cannot read file")?;
        if meta.len() > IMAGE_ATTACHMENT_CAP {
            anyhow::bail!("image too large (max 15 MB)");
        }
        let bytes = std::fs::read(p).context("cannot read file")?;
        return Ok(Attachment {
            kind: "image".into(),
            name,
            mime: mime.to_string(),
            size_bytes: bytes.len() as u64,
            data: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            text: None,
        });
    }

    let bytes = std::fs::read(p).context("cannot read file")?;
    let size = bytes.len() as u64;
    let mut text = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("unsupported file type (not an image or UTF-8 text)"))?;
    if text.len() > TEXT_ATTACHMENT_CAP {
        let mut end = TEXT_ATTACHMENT_CAP;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n…[truncated]");
    }
    Ok(Attachment {
        kind: "text".into(),
        name,
        mime: "text/plain".into(),
        size_bytes: size,
        data: None,
        text: Some(text),
    })
}

/// Session title derived from the first user message: first line, cut at a
/// word boundary near 48 chars.
pub fn derive_title(content: &str) -> String {
    let line = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.is_empty() {
        return "New chat".to_string();
    }
    let mut title = String::new();
    for word in line.split_whitespace() {
        if !title.is_empty() && title.len() + 1 + word.len() > 48 {
            title.push('…');
            break;
        }
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(word);
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_truncates_at_word_boundary() {
        assert_eq!(derive_title("hello world"), "hello world");
        assert_eq!(derive_title("  \n\nsecond line first\n"), "second line first");
        assert_eq!(derive_title(""), "New chat");
        let long = "one two three four five six seven eight nine ten eleven twelve";
        let t = derive_title(long);
        assert!(t.len() <= 52, "title too long: {t}");
        assert!(t.ends_with('…'));
    }

    #[test]
    fn session_roundtrip() {
        let dir = std::env::temp_dir().join(format!("locallmos-test-{}", uuid::Uuid::new_v4()));
        std::env::set_var("LOCALLMOS_CONFIG_DIR", &dir);

        let mut s = ChatSession::new("llama3.2".into());
        s.messages.push(StoredMessage::new("user", "hi".into()));
        save(&s).unwrap();

        let loaded = load(&s.id).unwrap();
        assert_eq!(loaded.id, s.id);
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.model, "llama3.2");

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
