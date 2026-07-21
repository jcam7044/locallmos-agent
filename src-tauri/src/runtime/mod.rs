//! Runtime adapter abstraction. Ollama and llama.cpp (`llama-server`) are the
//! implementations today; llama.cpp is the strategic primary (native, grammar-
//! constrained tool calling) with Ollama on a deprecation path. The `Runtime`
//! enum lets the rest of the agent (sync, reconcile, chat, command layers) stay
//! engine-agnostic — each rig selects its runtime via `LOCALLMOS_RUNTIME`.

pub mod llama_server;
pub mod ollama;
pub mod tool_protocol;
pub mod tools;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use llama_server::LlamaServerAdapter;
use ollama::OllamaAdapter;

/// Recommended per-message ceiling for executed tool calls. The chat engine
/// reserves one additional, tool-disabled request to synthesize the answer.
pub const DEFAULT_MAX_TOOL_CALLS: u16 = 25;
pub const MAX_TOOL_CALLS: u16 = 100;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelLoadSettings {
    #[serde(default)]
    pub context_size: Option<u32>,
    #[serde(default)]
    pub kv_cache_type: KvCacheType,
    #[serde(default)]
    pub gpu_offload: GpuOffload,
    #[serde(default)]
    pub flash_attention: FlashAttention,
    #[serde(default)]
    pub cpu_threads: Option<u16>,
    #[serde(default)]
    pub speculative_decoding: SpeculativeDecoding,
    /// `None` uses the recommended 25-call tool budget.
    #[serde(default)]
    pub max_tool_calls: Option<u16>,
}

impl ModelLoadSettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(size) = self.context_size {
            if !(512..=1_048_576).contains(&size) {
                anyhow::bail!("context size must be between 512 and 1,048,576 tokens");
            }
        }
        if let Some(threads) = self.cpu_threads {
            if !(1..=512).contains(&threads) {
                anyhow::bail!("CPU threads must be between 1 and 512");
            }
        }
        if let Some(limit) = self.max_tool_calls {
            if !(1..=MAX_TOOL_CALLS).contains(&limit) {
                anyhow::bail!("max tool calls must be between 1 and {MAX_TOOL_CALLS}");
            }
        }
        Ok(())
    }

    pub fn is_recommended(&self) -> bool {
        self == &Self::default()
    }

    pub fn tool_call_limit(&self) -> usize {
        self.max_tool_calls.unwrap_or(DEFAULT_MAX_TOOL_CALLS) as usize
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KvCacheType {
    #[default]
    Auto,
    F16,
    Q8_0,
    Q4_0,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuOffload {
    #[default]
    Auto,
    All,
    CpuOnly,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlashAttention {
    #[default]
    Auto,
    On,
    Off,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeculativeDecoding {
    #[default]
    Auto,
    Off,
    Mtp,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub size_bytes: Option<u64>,
    pub quantization: Option<String>,
    pub loaded: bool,
    /// Runtime-reported capabilities (e.g. "vision", "thinking", "tools").
    pub capabilities: Vec<String>,
    pub source_repo: Option<String>,
    pub revision: Option<String>,
    pub variant_id: Option<String>,
    pub files: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSnapshot {
    /// One of the `runtime_kind` enum values, e.g. "ollama", "llamacpp".
    pub kind: String,
    pub version: Option<String>,
    /// The active acceleration backend for llama.cpp (cuda|rocm|vulkan|cpu|metal),
    /// from the installer env/marker. `None` for Ollama (which owns its own build).
    pub backend: Option<String>,
    /// One of the `runtime_state` enum values: running|stopped|unknown|error.
    pub state: String,
    pub endpoint: Option<String>,
    pub models: Vec<ModelInfo>,
}

/// A streamed delta from a chat turn — reasoning ("thinking") or answer content.
pub enum ChatDelta<'a> {
    Content(&'a str),
    Thinking(&'a str),
}

/// A tool call the model requested during a round. `arguments` is the parsed
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

/// Performance information reported by a completed llama.cpp streamed response.
/// Times are milliseconds and rates are tokens per second.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationMetrics {
    pub prompt_eval_tokens: Option<u32>,
    pub cached_tokens: Option<u32>,
    pub prompt_eval_ms: Option<f64>,
    pub prompt_tokens_per_second: Option<f64>,
    pub generation_ms: Option<f64>,
    pub tokens_per_second: Option<f64>,
    pub time_to_first_token_ms: Option<f64>,
    pub stream_chunks: u32,
}

/// The assembled result of a chat turn.
pub struct ChatOutput {
    pub content: String,
    pub thinking: String,
    /// Token counts from the final chunk; `None` if the turn was cancelled or
    /// the stream ended without reporting them.
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    /// Timing and cache information from the runtime, when it reports it.
    pub generation_metrics: Option<GenerationMetrics>,
    /// Tool calls the model requested this round (empty when it answered directly).
    pub tool_calls: Vec<ToolCall>,
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

    /// Remove a resident model from memory without deleting its local files.
    fn unload_model(
        &self,
        model: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    /// Restart the runtime process/service (platform-specific, may no-op).
    fn restart(&self) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
}

/// The runtime selected for this rig. Enum dispatch (rather than `dyn`) keeps the
/// `impl Future` trait methods usable while letting `AppState` hold whichever
/// engine `LOCALLMOS_RUNTIME` chose. All call sites go through `state.runtime`.
pub enum Runtime {
    Ollama(OllamaAdapter),
    LlamaCpp(LlamaServerAdapter),
}

impl Runtime {
    /// Build the runtime for this rig from a kind string ("ollama" default
    /// during the transition; unknown values fall back to Ollama with a warning).
    /// The caller resolves precedence (env `LOCALLMOS_RUNTIME` > persisted config
    /// > default) — see `build_state`.
    pub fn from_kind(http: reqwest::Client, kind: &str) -> Self {
        match kind {
            "llamacpp" | "llama_cpp" | "llama.cpp" => {
                Runtime::LlamaCpp(LlamaServerAdapter::new(http))
            }
            "" | "ollama" => Runtime::Ollama(OllamaAdapter::new(http)),
            other => {
                tracing::warn!("unknown runtime {other:?}, using ollama");
                Runtime::Ollama(OllamaAdapter::new(http))
            }
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Runtime::Ollama(a) => a.kind(),
            Runtime::LlamaCpp(a) => a.kind(),
        }
    }

    pub async fn snapshot(&self) -> RuntimeSnapshot {
        match self {
            Runtime::Ollama(a) => a.snapshot().await,
            Runtime::LlamaCpp(a) => a.snapshot().await,
        }
    }

    pub async fn load_model(&self, model: &str) -> anyhow::Result<()> {
        match self {
            Runtime::Ollama(a) => a.load_model(model).await,
            Runtime::LlamaCpp(a) => a.load_model(model).await,
        }
    }

    pub async fn load_model_configured(
        &self,
        model: &str,
        settings: &ModelLoadSettings,
    ) -> anyhow::Result<()> {
        match self {
            Runtime::Ollama(a) => a.load_model(model).await,
            Runtime::LlamaCpp(a) => a.load_model_configured(model, settings).await,
        }
    }

    pub fn canonical_model_id(&self, model: &str) -> anyhow::Result<String> {
        match self {
            Runtime::Ollama(_) => Ok(model.to_string()),
            Runtime::LlamaCpp(a) => a.canonical_model_id(model),
        }
    }

    pub async fn unload_model(&self, model: &str) -> anyhow::Result<()> {
        match self {
            Runtime::Ollama(a) => a.unload_model(model).await,
            Runtime::LlamaCpp(a) => a.unload_model(model).await,
        }
    }

    pub async fn restart(&self) -> anyhow::Result<()> {
        match self {
            Runtime::Ollama(a) => a.restart().await,
            Runtime::LlamaCpp(a) => a.restart().await,
        }
    }

    pub async fn is_model_loaded(&self, model: &str) -> bool {
        match self {
            Runtime::Ollama(a) => a.is_model_loaded(model).await,
            Runtime::LlamaCpp(a) => a.is_model_loaded(model).await,
        }
    }

    pub async fn model_supports_tools(&self, model: &str) -> bool {
        match self {
            Runtime::Ollama(a) => a.model_supports_tools(model).await,
            Runtime::LlamaCpp(a) => a.model_supports_tools(model).await,
        }
    }

    pub async fn template_supports_tools(&self, model: &str) -> bool {
        match self {
            Runtime::Ollama(a) => a.template_supports_tools(model).await,
            Runtime::LlamaCpp(a) => a.template_supports_tools(model).await,
        }
    }

    pub async fn model_supports_thinking(&self, model: &str) -> bool {
        match self {
            Runtime::Ollama(a) => a.model_supports_thinking(model).await,
            Runtime::LlamaCpp(a) => a.model_supports_thinking(model).await,
        }
    }

    pub fn endpoint(&self) -> &str {
        match self {
            Runtime::Ollama(a) => a.endpoint(),
            Runtime::LlamaCpp(a) => a.endpoint(),
        }
    }

    /// The directory GGUF models are served from, when the runtime has one the
    /// user drops files into (llama.cpp). `None` for Ollama, which owns its store.
    pub fn models_dir(&self) -> Option<String> {
        match self {
            Runtime::Ollama(_) => None,
            Runtime::LlamaCpp(a) => a.models_dir(),
        }
    }

    pub async fn context_size(&self) -> Option<u64> {
        match self {
            Runtime::Ollama(_) => Some(8192),
            Runtime::LlamaCpp(a) => a.effective_context_size().await,
        }
    }

    /// Stream a chat completion, dispatching to the active engine. The delta
    /// callback and `ChatOutput` result are identical across engines.
    #[allow(clippy::too_many_arguments)]
    pub async fn chat_stream<F: FnMut(ChatDelta)>(
        &self,
        model: &str,
        messages: Value,
        think: bool,
        tools: Option<&Value>,
        options: Option<&Value>,
        load_settings: &ModelLoadSettings,
        cancel: Arc<AtomicBool>,
        on_delta: F,
    ) -> anyhow::Result<ChatOutput> {
        match self {
            Runtime::Ollama(a) => {
                a.chat_stream(model, messages, think, tools, options, cancel, on_delta)
                    .await
            }
            Runtime::LlamaCpp(a) => {
                a.chat_stream(model, messages, think, tools, options, load_settings, cancel, on_delta)
                    .await
            }
        }
    }
}

/// Resolve the llama.cpp model store independently of the active engine.
pub fn llamacpp_models_dir() -> String {
    std::env::var("LOCALLMOS_LLAMACPP_MODELS_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(llama_server::default_models_dir)
}

#[cfg(test)]
mod model_settings_tests {
    use super::*;

    #[test]
    fn validates_model_load_setting_bounds() {
        assert!(ModelLoadSettings::default().validate().is_ok());
        assert_eq!(ModelLoadSettings::default().tool_call_limit(), 25);
        assert!(ModelLoadSettings {
            context_size: Some(511),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(ModelLoadSettings {
            max_tool_calls: Some(101),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(ModelLoadSettings {
            cpu_threads: Some(513),
            ..Default::default()
        }
        .validate()
        .is_err());
    }
}
