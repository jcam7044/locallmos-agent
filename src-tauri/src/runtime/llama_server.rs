//! llama.cpp (`llama-server`) runtime adapter — the strategic primary engine.
//!
//! Unlike Ollama, tool calling here is **native and grammar-constrained**: the
//! agent talks to llama-server's OpenAI-compatible `/v1/chat/completions` with a
//! `tools` array and `tool_choice:"auto"`, and llama.cpp constrains decoding to a
//! grammar built from the tool schemas. The model cannot emit a malformed or
//! hallucinated call, so `chat.rs` uses its native `tool_calls` path with no
//! prompt-injection fallback.
//!
//! llama-server serves a single model per process, so this adapter manages the
//! child process: it (re)spawns llama-server for the requested model, waits for
//! `/health`, and reuses it while the same model is requested.

use super::{ChatDelta, ChatOutput, ModelInfo, RuntimeAdapter, RuntimeSnapshot, ToolCall};
use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// A running llama-server child and the model it was launched for.
struct ChildProc {
    child: Child,
    model: String,
}

impl ChildProc {
    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

pub struct LlamaServerAdapter {
    http: reqwest::Client,
    base: String,
    host: String,
    port: u16,
    bin: String,
    models_dir: String,
    ngl: String,
    ctx: String,
    /// Extra `llama-server` args (whitespace-split from `LOCALLMOS_LLAMACPP_ARGS`).
    extra_args: Vec<String>,
    /// Whether this rig's model reasons; toggles Qwen-style `enable_thinking` and
    /// the reported "thinking" capability.
    thinking: bool,
    startup_timeout: u64,
    proc: Mutex<Option<ChildProc>>,
}

impl LlamaServerAdapter {
    pub fn new(http: reqwest::Client) -> Self {
        let host =
            std::env::var("LOCALLMOS_LLAMACPP_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port = std::env::var("LOCALLMOS_LLAMACPP_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8080u16);
        let extra_args = std::env::var("LOCALLMOS_LLAMACPP_ARGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        Self {
            http,
            base: format!("http://{host}:{port}"),
            host,
            port,
            // Fall back to the installer's conventional paths when unset, so a
            // GUI-switched rig (no env) still finds its provisioned engine/models.
            bin: std::env::var("LOCALLMOS_LLAMACPP_BIN")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(default_bin),
            models_dir: std::env::var("LOCALLMOS_LLAMACPP_MODELS_DIR")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(default_models_dir),
            ngl: std::env::var("LOCALLMOS_LLAMACPP_NGL").unwrap_or_else(|_| "999".into()),
            ctx: std::env::var("LOCALLMOS_LLAMACPP_CTX").unwrap_or_else(|_| "8192".into()),
            extra_args,
            thinking: !std::env::var("LOCALLMOS_LLAMACPP_THINKING")
                .unwrap_or_default()
                .is_empty(),
            startup_timeout: std::env::var("LOCALLMOS_LLAMACPP_STARTUP_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(180),
            proc: Mutex::new(None),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.base
    }

    /// The configured GGUF directory, if any (users drop models here).
    pub fn models_dir(&self) -> Option<String> {
        (!self.models_dir.is_empty()).then(|| self.models_dir.clone())
    }

    // llama-server serves whatever model process it was launched with, and its
    // decoding is grammar-constrained, so tools are always natively available.
    pub async fn model_supports_tools(&self, _model: &str) -> bool {
        true
    }
    pub async fn template_supports_tools(&self, _model: &str) -> bool {
        true
    }
    pub async fn model_supports_thinking(&self, _model: &str) -> bool {
        self.thinking
    }

    pub async fn is_model_loaded(&self, model: &str) -> bool {
        let mut guard = self.proc.lock().await;
        match guard.as_mut() {
            Some(p) => p.model == model && p.is_alive(),
            None => false,
        }
    }

    /// Resolve a model name to a GGUF path: an exact file-stem match under the
    /// models dir, else a stem prefix match, else (no dir configured) an absolute
    /// `.gguf` path passed as the model name.
    fn resolve_gguf(&self, model: &str) -> Option<PathBuf> {
        if self.models_dir.is_empty() {
            let p = PathBuf::from(model);
            let is_gguf = p.extension().and_then(|e| e.to_str()) == Some("gguf");
            return (is_gguf && p.exists()).then_some(p);
        }
        let ggufs = list_ggufs(&self.models_dir);
        let want = model.to_lowercase();
        ggufs
            .iter()
            .find(|p| stem(p).to_lowercase() == want)
            .or_else(|| ggufs.iter().find(|p| stem(p).to_lowercase().starts_with(&want)))
            .cloned()
    }

    /// Ensure a llama-server child is running and serving `model`, (re)spawning
    /// if none is running or a different model was requested.
    async fn ensure_running(&self, model: &str) -> Result<()> {
        // Resolve the model file BEFORE touching the running process, so an
        // unknown/stale request (e.g. a reconcile `desired_model` that isn't a
        // local gguf) can never tear down a healthy server mid-load.
        let gguf = self.resolve_gguf(model).ok_or_else(|| {
            anyhow!("no .gguf for model {model:?} in {:?}", self.models_dir)
        })?;
        {
            let mut guard = self.proc.lock().await;
            if let Some(p) = guard.as_mut() {
                if p.model == model && p.is_alive() {
                    return Ok(());
                }
                // Wrong model or dead process: stop it before starting the next.
                let _ = p.child.start_kill();
                let _ = p.child.wait().await;
                *guard = None;
            }
            let mut cmd = Command::new(&self.bin);
            cmd.arg("-m")
                .arg(&gguf)
                .arg("--alias")
                .arg(model)
                .arg("--host")
                .arg(&self.host)
                .arg("--port")
                .arg(self.port.to_string())
                // --jinja applies the GGUF's chat template and enables native,
                // grammar-constrained tool calling.
                .arg("--jinja")
                .arg("--ctx-size")
                .arg(&self.ctx)
                .arg("--n-gpu-layers")
                .arg(&self.ngl);
            for a in &self.extra_args {
                cmd.arg(a);
            }
            // Surface llama-server load progress/errors in the agent terminal.
            cmd.stdout(Stdio::null()).stderr(Stdio::inherit());
            cmd.kill_on_drop(true);
            let child = cmd
                .spawn()
                .map_err(|e| anyhow!("failed to spawn {}: {e}", self.bin))?;
            *guard = Some(ChildProc {
                child,
                model: model.to_string(),
            });
        }
        self.wait_healthy().await
    }

    /// Poll `/health` until the server is ready, failing fast if the child exits.
    async fn wait_healthy(&self) -> Result<()> {
        let url = format!("{}/health", self.base);
        let deadline = Instant::now() + Duration::from_secs(self.startup_timeout);
        loop {
            {
                let mut guard = self.proc.lock().await;
                match guard.as_mut() {
                    Some(p) => {
                        if let Ok(Some(status)) = p.child.try_wait() {
                            return Err(anyhow!("llama-server exited during startup: {status}"));
                        }
                    }
                    None => return Err(anyhow!("llama-server process missing")),
                }
            }
            if let Ok(r) = self.http.get(url.as_str()).send().await {
                if r.status().is_success() {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("llama-server did not become healthy in time"));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn list_models(&self, current: Option<&str>) -> Vec<ModelInfo> {
        let mut caps = vec!["tools".to_string()];
        if self.thinking {
            caps.push("thinking".to_string());
        }
        list_ggufs(&self.models_dir)
            .into_iter()
            .map(|p| {
                let name = stem(&p);
                ModelInfo {
                    size_bytes: std::fs::metadata(&p).ok().map(|m| m.len()),
                    quantization: None,
                    loaded: current == Some(name.as_str()),
                    capabilities: caps.clone(),
                    name,
                }
            })
            .collect()
    }

    /// Stream a chat completion from llama-server's OpenAI-compatible endpoint.
    #[allow(clippy::too_many_arguments)]
    pub async fn chat_stream<F: FnMut(ChatDelta)>(
        &self,
        model: &str,
        messages: Value,
        think: bool,
        tools: Option<&Value>,
        options: Option<&Value>,
        cancel: Arc<AtomicBool>,
        mut on_delta: F,
    ) -> Result<ChatOutput> {
        self.ensure_running(model).await?;

        let mut body = json!({
            "model": model,
            "messages": to_openai_messages(&messages),
            "stream": true,
            "stream_options": { "include_usage": true },
            // Qwen-style templates honor this under --jinja to toggle reasoning.
            "chat_template_kwargs": { "enable_thinking": think },
        });
        // Map sampling options (temperature, top_p, …); num_ctx is a launch flag.
        if let Some(map) = options.and_then(Value::as_object) {
            for (k, v) in map {
                if k != "num_ctx" {
                    body[k] = v.clone();
                }
            }
        }
        if let Some(t) = tools {
            if t.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                body["tools"] = t.clone();
                body["tool_choice"] = json!("auto");
            }
        }

        let resp = self
            .http
            .post(format!("{}/v1/chat/completions", self.base))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let msg = resp.text().await.unwrap_or_default();
            return Err(anyhow!("llama-server chat failed: HTTP {status}: {msg}"));
        }

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut state = StreamState::default();
        'outer: while let Some(chunk) = stream.next().await {
            // Stop-generation: drop the stream (closing the connection halts the
            // server) and return what we have.
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            buf.extend_from_slice(&chunk?);
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=nl).collect();
                let line = String::from_utf8_lossy(&line[..line.len() - 1]);
                let line = line.trim();
                let Some(payload) = line.strip_prefix("data:") else {
                    continue; // SSE comments / blank separators
                };
                let payload = payload.trim();
                if payload == "[DONE]" {
                    break 'outer;
                }
                let Ok(v) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                let (content, thinking) = ingest(&mut state, &v);
                if let Some(s) = thinking {
                    on_delta(ChatDelta::Thinking(&s));
                }
                if let Some(s) = content {
                    on_delta(ChatDelta::Content(&s));
                }
            }
        }
        Ok(finalize(state))
    }
}

impl RuntimeAdapter for LlamaServerAdapter {
    fn kind(&self) -> &'static str {
        "llamacpp"
    }

    async fn snapshot(&self) -> RuntimeSnapshot {
        let current = {
            let guard = self.proc.lock().await;
            guard.as_ref().map(|p| p.model.clone())
        };
        let models = self.list_models(current.as_deref()).await;
        // The managed runtime is "available" whenever we can see models to serve.
        let state = if models.is_empty() && current.is_none() {
            "stopped"
        } else {
            "running"
        };
        RuntimeSnapshot {
            kind: "llamacpp".into(),
            version: None,
            state: state.into(),
            endpoint: Some(self.base.clone()),
            models,
        }
    }

    async fn load_model(&self, model: &str) -> Result<()> {
        self.ensure_running(model).await
    }

    async fn restart(&self) -> Result<()> {
        // Stop the current child; the next chat/load respawns it on demand.
        let mut guard = self.proc.lock().await;
        if let Some(mut p) = guard.take() {
            let _ = p.child.start_kill();
            let _ = p.child.wait().await;
        }
        Ok(())
    }
}

/// Translate the canonical (Ollama-shaped) message history into the OpenAI wire
/// format llama-server requires. `chat.rs` echoes tool calls as Ollama does —
/// `{"function":{name,arguments}}` with an object `arguments`, and tool results
/// with a `tool_name` — which the OpenAI endpoint rejects ("Missing tool call
/// type"). Here each assistant tool call gains an `id` + `type:"function"` and a
/// string `arguments`, and each following tool result gets the matching
/// `tool_call_id`. Non-tool messages pass through unchanged.
fn to_openai_messages(messages: &Value) -> Value {
    let Some(arr) = messages.as_array() else {
        return messages.clone();
    };
    let mut out: Vec<Value> = Vec::with_capacity(arr.len());
    // Ids assigned to an assistant's tool calls, consumed in order by the tool
    // results that follow (chat.rs appends them in the same order).
    let mut pending: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut n = 0usize;
    for msg in arr {
        match msg.get("role").and_then(Value::as_str) {
            Some("assistant") if msg.get("tool_calls").is_some() => {
                let calls = msg.get("tool_calls").and_then(Value::as_array);
                let new_calls: Vec<Value> = calls
                    .into_iter()
                    .flatten()
                    .map(|tc| {
                        let func = tc.get("function").unwrap_or(tc);
                        let name = func.get("name").and_then(Value::as_str).unwrap_or("");
                        let args = match func.get("arguments") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".into()),
                            None => "{}".into(),
                        };
                        let id = tc
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                n += 1;
                                format!("call_{n}")
                            });
                        pending.push_back(id.clone());
                        json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": args },
                        })
                    })
                    .collect();
                out.push(json!({
                    "role": "assistant",
                    "content": msg.get("content").cloned().unwrap_or_else(|| json!("")),
                    "tool_calls": new_calls,
                }));
            }
            Some("tool") => {
                let id = pending.pop_front().unwrap_or_else(|| {
                    n += 1;
                    format!("call_{n}")
                });
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": msg.get("content").cloned().unwrap_or_else(|| json!("")),
                }));
            }
            _ => out.push(msg.clone()),
        }
    }
    Value::Array(out)
}

/// Default models dir when unset — matches the installer's desktop provisioning
/// path (`$XDG_DATA_HOME/locallmos/models`), so a GUI-switched rig finds models
/// with no env configuration.
fn default_models_dir() -> String {
    dirs::data_dir()
        .map(|p| p.join("locallmos").join("models").to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Default llama-server path when unset: the provisioned binary under the user
/// (`~/.local/opt/locallmos/llama`) or system (`/opt/locallmos/llama`) install
/// dir, else bare "llama-server" (resolved on PATH).
fn default_bin() -> String {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".local/opt/locallmos/llama"));
    }
    roots.push(PathBuf::from("/opt/locallmos/llama"));
    for root in roots {
        if let Some(bin) = find_llama_server(&root) {
            return bin.to_string_lossy().into_owned();
        }
    }
    "llama-server".into()
}

/// Find `llama-server` directly under `root` or one level down (the release
/// tarball extracts into a `llama-<tag>/` subdir).
fn find_llama_server(root: &Path) -> Option<PathBuf> {
    let direct = root.join("llama-server");
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let cand = entry.path().join("llama-server");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

fn list_ggufs(dir: &str) -> Vec<PathBuf> {
    if dir.is_empty() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("gguf"))
        .collect()
}

// ---------------------------------------------------------------------------
// Streaming assembly (pure; unit-tested).
// ---------------------------------------------------------------------------

/// One tool call being assembled across streamed deltas. OpenAI streams the
/// `arguments` JSON as string fragments per `index`.
#[derive(Default)]
struct ToolFrag {
    name: String,
    args: String,
}

#[derive(Default)]
struct StreamState {
    content: String,
    thinking: String,
    tools: Vec<ToolFrag>,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

/// Fold one parsed SSE `chunk` into `state`, returning any (content, thinking)
/// text to stream to the UI now.
fn ingest(state: &mut StreamState, chunk: &Value) -> (Option<String>, Option<String>) {
    if let Some(u) = chunk.get("usage").and_then(Value::as_object) {
        if let Some(p) = u.get("prompt_tokens").and_then(Value::as_u64) {
            state.prompt_tokens = Some(p as u32);
        }
        if let Some(c) = u.get("completion_tokens").and_then(Value::as_u64) {
            state.completion_tokens = Some(c as u32);
        }
    }
    let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else {
        return (None, None);
    };
    let Some(delta) = choice.get("delta") else {
        return (None, None);
    };

    let mut out_content = None;
    let mut out_thinking = None;
    if let Some(s) = delta.get("content").and_then(Value::as_str) {
        if !s.is_empty() {
            state.content.push_str(s);
            out_content = Some(s.to_string());
        }
    }
    // Reasoning models (via llama.cpp --reasoning-format) stream reasoning_content.
    if let Some(s) = delta.get("reasoning_content").and_then(Value::as_str) {
        if !s.is_empty() {
            state.thinking.push_str(s);
            out_thinking = Some(s.to_string());
        }
    }
    if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
        for tc in tcs {
            let idx = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            while state.tools.len() <= idx {
                state.tools.push(ToolFrag::default());
            }
            let frag = &mut state.tools[idx];
            if let Some(f) = tc.get("function") {
                if let Some(n) = f.get("name").and_then(Value::as_str) {
                    if !n.is_empty() {
                        frag.name = n.to_string();
                    }
                }
                if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                    frag.args.push_str(a);
                }
            }
        }
    }
    (out_content, out_thinking)
}

fn finalize(state: StreamState) -> ChatOutput {
    let tool_calls = state
        .tools
        .into_iter()
        .filter(|f| !f.name.is_empty())
        .map(|f| ToolCall {
            name: f.name,
            arguments: serde_json::from_str(f.args.trim()).unwrap_or_else(|_| json!({})),
        })
        .collect();
    ChatOutput {
        content: state.content,
        thinking: state.thinking,
        prompt_tokens: state.prompt_tokens,
        completion_tokens: state.completion_tokens,
        tool_calls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a sequence of parsed SSE chunks and finalize.
    fn run(chunks: &[Value]) -> (String, String, ChatOutput) {
        let mut state = StreamState::default();
        let mut content = String::new();
        let mut thinking = String::new();
        for c in chunks {
            let (dc, dt) = ingest(&mut state, c);
            if let Some(s) = dc {
                content.push_str(&s);
            }
            if let Some(s) = dt {
                thinking.push_str(&s);
            }
        }
        let out = finalize(state);
        (content, thinking, out)
    }

    #[test]
    fn concatenates_content_and_reports_usage() {
        let chunks = vec![
            json!({"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}),
            json!({"choices":[{"delta":{"content":", world"},"finish_reason":null}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":4}}),
        ];
        let (streamed, _thinking, out) = run(&chunks);
        assert_eq!(streamed, "Hello, world");
        assert_eq!(out.content, "Hello, world");
        assert!(out.tool_calls.is_empty());
        assert_eq!(out.prompt_tokens, Some(11));
        assert_eq!(out.completion_tokens, Some(4));
    }

    #[test]
    fn reassembles_tool_call_arguments_across_deltas() {
        // Name arrives once; arguments stream as fragments per index.
        let chunks = vec![
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"web_search","arguments":""}}]},"finish_reason":null}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"query\":\"flor"}}]},"finish_reason":null}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ida\"}"}}]},"finish_reason":null}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ];
        let (streamed, _t, out) = run(&chunks);
        assert_eq!(streamed, ""); // tool calls never stream as content
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].name, "web_search");
        assert_eq!(out.tool_calls[0].arguments["query"], "florida");
    }

    #[test]
    fn handles_multiple_parallel_tool_calls() {
        let chunks = vec![
            json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"function":{"name":"web_search","arguments":"{\"query\":\"a\"}"}},
                {"index":1,"function":{"name":"web_fetch_page","arguments":"{\"url\":\"u\"}"}}
            ]},"finish_reason":null}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ];
        let (_c, _t, out) = run(&chunks);
        let names: Vec<&str> = out.tool_calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["web_search", "web_fetch_page"]);
        assert_eq!(out.tool_calls[1].arguments["url"], "u");
    }

    #[test]
    fn separates_reasoning_from_content() {
        let chunks = vec![
            json!({"choices":[{"delta":{"reasoning_content":"thinking..."},"finish_reason":null}]}),
            json!({"choices":[{"delta":{"content":"answer"},"finish_reason":"stop"}]}),
        ];
        let (content, thinking, out) = run(&chunks);
        assert_eq!(content, "answer");
        assert_eq!(thinking, "thinking...");
        assert_eq!(out.thinking, "thinking...");
    }

    #[test]
    fn openai_messages_add_tool_call_ids_type_and_string_args() {
        let msgs = json!([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "",
             "tool_calls": [{"function": {"name": "web_search", "arguments": {"query": "x"}}}]},
            {"role": "tool", "tool_name": "web_search", "content": "results"},
        ]);
        let arr = to_openai_messages(&msgs);
        let arr = arr.as_array().unwrap();
        // user passes through unchanged
        assert_eq!(arr[0]["content"], "hi");
        // assistant tool call gains id + type + string arguments
        let tc = &arr[1]["tool_calls"][0];
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "web_search");
        assert!(tc["function"]["arguments"].is_string(), "arguments must be a JSON string");
        let id = tc["id"].as_str().unwrap();
        // tool result carries the matching tool_call_id and drops tool_name
        assert_eq!(arr[2]["role"], "tool");
        assert_eq!(arr[2]["tool_call_id"], id);
        assert!(arr[2].get("tool_name").is_none());
    }

    #[test]
    fn malformed_tool_arguments_degrade_to_empty_object() {
        let chunks = vec![
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"web_search","arguments":"not json"}}]},"finish_reason":"tool_calls"}]}),
        ];
        let (_c, _t, out) = run(&chunks);
        assert_eq!(out.tool_calls.len(), 1);
        assert!(out.tool_calls[0].arguments.is_object());
    }
}
