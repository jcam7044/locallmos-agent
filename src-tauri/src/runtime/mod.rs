//! Runtime adapter abstraction. Ollama is the only implementation today; the
//! trait exists so LM Studio / llama.cpp / vLLM slot in without touching the
//! sync, reconcile, or command layers.

pub mod ollama;
pub mod tool_protocol;
pub mod tools;

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub name: String,
    pub size_bytes: Option<u64>,
    pub quantization: Option<String>,
    pub loaded: bool,
    /// Ollama model capabilities (e.g. "vision", "thinking", "tools").
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSnapshot {
    /// One of the `runtime_kind` enum values, e.g. "ollama".
    pub kind: String,
    pub version: Option<String>,
    /// One of the `runtime_state` enum values: running|stopped|unknown|error.
    pub state: String,
    pub endpoint: Option<String>,
    pub models: Vec<ModelInfo>,
}

/// A managed local LLM runtime. Methods are best-effort and should degrade to a
/// sensible snapshot rather than panicking when the runtime is down.
pub trait RuntimeAdapter {
    fn kind(&self) -> &'static str;

    /// Full snapshot: version, state, installed + loaded models.
    fn snapshot(&self) -> impl std::future::Future<Output = RuntimeSnapshot> + Send;

    /// Ensure `model` is loaded into memory.
    fn load_model(
        &self,
        model: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    /// Restart the runtime process/service (platform-specific, may no-op).
    fn restart(&self) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
}
