//! Ollama adapter over its local HTTP API (default http://127.0.0.1:11434).

use super::{ModelInfo, RuntimeAdapter, RuntimeSnapshot};
use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct OllamaAdapter {
    base: String,
    http: reqwest::Client,
    /// How long Ollama keeps a model resident after a request. Sent on every
    /// request so it overrides the server's OLLAMA_KEEP_ALIVE default — otherwise
    /// a short/zero default unloads the model between chat turns and reloads it
    /// cold each time.
    keep_alive: String,
    /// Per-model capabilities from `/api/show`, cached so we don't re-query on
    /// every telemetry snapshot (model metadata rarely changes).
    caps_cache: Mutex<HashMap<String, Vec<String>>>,
}

#[derive(Deserialize)]
struct VersionResp {
    version: String,
}

#[derive(Deserialize)]
struct TagsResp {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    details: Option<ModelDetails>,
}

#[derive(Deserialize)]
struct ModelDetails {
    #[serde(default)]
    quantization_level: Option<String>,
}

#[derive(Deserialize)]
struct PsResp {
    #[serde(default)]
    models: Vec<PsModel>,
}

#[derive(Deserialize)]
struct PsModel {
    name: String,
}

impl OllamaAdapter {
    pub fn new(http: reqwest::Client) -> Self {
        let base = std::env::var("LOCALLMOS_OLLAMA_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
        let keep_alive =
            std::env::var("LOCALLMOS_CHAT_KEEP_ALIVE").unwrap_or_else(|_| "30m".to_string());
        Self {
            base: base.trim_end_matches('/').to_string(),
            http,
            keep_alive,
            caps_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Model capabilities (e.g. "vision", "thinking", "tools") via `/api/show`,
    /// cached by model name. Non-empty results are cached; a transient failure
    /// (empty) is retried on the next snapshot.
    async fn capabilities(&self, model: &str) -> Vec<String> {
        if let Some(c) = self.caps_cache.lock().await.get(model) {
            return c.clone();
        }
        let caps = self.fetch_capabilities(model).await;
        if !caps.is_empty() {
            self.caps_cache
                .lock()
                .await
                .insert(model.to_string(), caps.clone());
        }
        caps
    }

    async fn fetch_capabilities(&self, model: &str) -> Vec<String> {
        #[derive(Deserialize)]
        struct ShowResp {
            #[serde(default)]
            capabilities: Vec<String>,
        }
        match self
            .http
            .post(format!("{}/api/show", self.base))
            .json(&serde_json::json!({ "model": model }))
            .send()
            .await
        {
            Ok(resp) => resp
                .json::<ShowResp>()
                .await
                .map(|s| s.capabilities)
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.base
    }

    /// Whether `model` advertises tool-calling support (per its `/api/show`
    /// capabilities). Used to decide whether to advertise tools for a turn.
    pub async fn model_supports_tools(&self, model: &str) -> bool {
        self.capabilities(model).await.iter().any(|c| c == "tools")
    }

    async fn version(&self) -> Option<String> {
        let resp = self
            .http
            .get(format!("{}/api/version", self.base))
            .send()
            .await
            .ok()?;
        resp.json::<VersionResp>().await.ok().map(|v| v.version)
    }

    /// True if `model` is currently resident in Ollama (per `/api/ps`). Lets a
    /// chat turn tell the web when it's about to incur a cold model load. Errs
    /// toward "not loaded" (matches conservatively) so we never suppress the
    /// loading indicator during a real load.
    pub async fn is_model_loaded(&self, model: &str) -> bool {
        let loaded = self.loaded_names().await;
        loaded.contains(model)
            // A bare name (no tag) resolves to ":latest" in Ollama.
            || (!model.contains(':') && loaded.contains(&format!("{model}:latest")))
    }

    async fn loaded_names(&self) -> HashSet<String> {
        let mut set = HashSet::new();
        if let Ok(resp) = self.http.get(format!("{}/api/ps", self.base)).send().await {
            if let Ok(ps) = resp.json::<PsResp>().await {
                for m in ps.models {
                    set.insert(m.name);
                }
            }
        }
        set
    }

    /// Stream a chat completion. `messages` is Ollama's chat format
    /// (`[{"role":..,"content":..,"images":[..]}, ..]`). When `think` is set,
    /// reasoning models emit `message.thinking` deltas separately from
    /// `message.content`; both are surfaced via `on_delta`. Returns the full
    /// assembled `(content, thinking)`.
    pub async fn chat_stream<F: FnMut(ChatDelta)>(
        &self,
        model: &str,
        messages: Value,
        think: bool,
        tools: Option<&Value>,
        cancel: Arc<AtomicBool>,
        mut on_delta: F,
    ) -> Result<ChatOutput> {
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "keep_alive": self.keep_alive,
        });
        if think {
            body["think"] = Value::Bool(true);
        }
        // Only advertise tools when the round has any — otherwise a plain chat.
        if let Some(t) = tools {
            if t.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                body["tools"] = t.clone();
            }
        }
        let resp = self
            .http
            .post(format!("{}/api/chat", self.base))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let msg = resp.text().await.unwrap_or_default();
            return Err(anyhow!("ollama chat failed: HTTP {status}: {msg}"));
        }

        // Ollama streams newline-delimited JSON; buffer partial lines.
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut content = String::new();
        let mut thinking = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        while let Some(chunk) = stream.next().await {
            // Stop-generation: return what we have so far. Dropping the response
            // stream closes the connection, which halts Ollama's generation.
            if cancel.load(Ordering::Relaxed) {
                return Ok(ChatOutput {
                    content,
                    thinking,
                    prompt_tokens: None,
                    completion_tokens: None,
                    tool_calls,
                });
            }
            buf.extend_from_slice(&chunk?);
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=nl).collect();
                let line = &line[..line.len() - 1];
                if line.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_slice::<Value>(line) else {
                    continue;
                };
                let message = v.get("message");
                // Tool calls arrive in the message (usually with empty content).
                if let Some(calls) = message.and_then(|m| m.get("tool_calls")).and_then(|c| c.as_array()) {
                    for call in calls {
                        if let Some(func) = call.get("function") {
                            if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                                tool_calls.push(ToolCall {
                                    name: name.to_string(),
                                    arguments: func.get("arguments").cloned().unwrap_or(Value::Null),
                                });
                            }
                        }
                    }
                }
                if let Some(delta) = message
                    .and_then(|m| m.get("thinking"))
                    .and_then(|c| c.as_str())
                {
                    if !delta.is_empty() {
                        thinking.push_str(delta);
                        on_delta(ChatDelta::Thinking(delta));
                    }
                }
                if let Some(delta) = message
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    if !delta.is_empty() {
                        content.push_str(delta);
                        on_delta(ChatDelta::Content(delta));
                    }
                }
                if v.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                    let prompt_tokens = v
                        .get("prompt_eval_count")
                        .and_then(|n| n.as_u64())
                        .map(|n| n as u32);
                    let completion_tokens =
                        v.get("eval_count").and_then(|n| n.as_u64()).map(|n| n as u32);
                    return Ok(ChatOutput {
                        content,
                        thinking,
                        prompt_tokens,
                        completion_tokens,
                        tool_calls,
                    });
                }
            }
        }
        Ok(ChatOutput {
            content,
            thinking,
            prompt_tokens: None,
            completion_tokens: None,
            tool_calls,
        })
    }
}

/// A streamed delta from a chat turn — reasoning ("thinking") or answer content.
pub enum ChatDelta<'a> {
    Content(&'a str),
    Thinking(&'a str),
}

/// A tool call the model requested during a round. `arguments` is Ollama's parsed
/// argument object; `name` is the function name.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}

impl ToolCall {
    /// Rebuild the tool_call object to echo back in the assistant message that
    /// precedes the tool results (Ollama request format).
    pub fn to_request_value(&self) -> Value {
        serde_json::json!({ "function": { "name": self.name, "arguments": self.arguments } })
    }
}

/// The assembled result of a chat turn.
pub struct ChatOutput {
    pub content: String,
    pub thinking: String,
    /// Ollama token counts from the final `done` line; `None` if the turn was
    /// cancelled or the stream ended without one.
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    /// Tool calls the model requested this round (empty when it answered directly).
    pub tool_calls: Vec<ToolCall>,
}

impl RuntimeAdapter for OllamaAdapter {
    fn kind(&self) -> &'static str {
        "ollama"
    }

    async fn snapshot(&self) -> RuntimeSnapshot {
        let version = self.version().await;
        // If /api/version doesn't answer, treat the runtime as stopped.
        if version.is_none() {
            return RuntimeSnapshot {
                kind: "ollama".into(),
                version: None,
                state: "stopped".into(),
                endpoint: Some(self.base.clone()),
                models: vec![],
            };
        }

        let loaded = self.loaded_names().await;
        let mut models = Vec::new();
        if let Ok(resp) = self.http.get(format!("{}/api/tags", self.base)).send().await {
            if let Ok(tags) = resp.json::<TagsResp>().await {
                for m in tags.models {
                    let is_loaded = loaded.contains(&m.name);
                    let capabilities = self.capabilities(&m.name).await;
                    models.push(ModelInfo {
                        quantization: m.details.and_then(|d| d.quantization_level),
                        size_bytes: m.size,
                        loaded: is_loaded,
                        capabilities,
                        name: m.name,
                    });
                }
            }
        }

        RuntimeSnapshot {
            kind: "ollama".into(),
            version,
            state: "running".into(),
            endpoint: Some(self.base.clone()),
            models,
        }
    }

    async fn load_model(&self, model: &str) -> Result<()> {
        // Empty prompt + keep_alive keeps the model resident without generating.
        let resp = self
            .http
            .post(format!("{}/api/generate", self.base))
            .json(&serde_json::json!({ "model": model, "keep_alive": self.keep_alive }))
            .send()
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("ollama load_model failed: HTTP {}", resp.status()))
        }
    }

    async fn restart(&self) -> Result<()> {
        crate::worker::restart_service("ollama").await
    }
}
