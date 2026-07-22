//! Local chat turn engine: the on-disk session file is the source of truth
//! (mirroring how `chat.rs` treats the `chat_messages` row for cloud turns).
//! The user message is persisted before generation starts, deltas stream to the
//! webview as `local-chat` events, and the assistant message is persisted on
//! completion — including partial output when the turn is cancelled.

use crate::chat_store::{self, StoredMessage};
use crate::runtime::ollama::{ChatDelta, ToolCall};
use crate::runtime::GenerationMetrics;
use crate::runtime::tools;
use crate::AppState;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Emitter;

/// Event name for streamed deltas; payloads carry `requestId`/`sessionId` so a
/// single frontend listener can route concurrent turns.
const EVENT: &str = "local-chat";

fn emit(app: &tauri::AppHandle, request_id: &str, session_id: &str, mut payload: Value) {
    payload["requestId"] = json!(request_id);
    payload["sessionId"] = json!(session_id);
    let _ = app.emit(EVENT, payload);
}

pub async fn send(
    app: tauri::AppHandle,
    state: Arc<AppState>,
    session_id: String,
    request_id: String,
    content: String,
    attachments: Vec<chat_store::Attachment>,
    regenerate: bool,
) -> Result<StoredMessage, String> {
    // Persist the user's side of the turn up front so it survives a crash or
    // error mid-generation.
    let mut session = {
        let _guard = state.chat_lock.lock().await;
        let mut session = chat_store::load(&session_id).map_err(|e| e.to_string())?;
        if regenerate {
            if session.messages.last().map(|m| m.role == "assistant").unwrap_or(false) {
                session.messages.pop();
            }
        } else {
            let mut msg = StoredMessage::new("user", content);
            msg.attachments = attachments;
            session.messages.push(msg);
            if session.title == "New chat" {
                if let Some(first) = session.messages.iter().find(|m| m.role == "user") {
                    session.title = chat_store::derive_title(&first.content);
                }
            }
        }
        session.updated_at = chrono::Utc::now();
        chat_store::save(&session).map_err(|e| e.to_string())?;
        session
    };
    let model = session.model.clone();
    if model.is_empty() {
        return Err("no model selected".to_string());
    }

    // Ollama chat history: optional system prompt, then messages with
    // attachments folded in (images → base64 `images`, text → inlined content).
    let mut messages: Vec<Value> = Vec::with_capacity(session.messages.len() + 1);
    if let Some(sys) = session.settings.system_prompt.as_deref().filter(|s| !s.trim().is_empty()) {
        messages.push(json!({ "role": "system", "content": sys }));
    }
    for m in &session.messages {
        let mut content = m.content.clone();
        let mut images: Vec<String> = Vec::new();
        for a in &m.attachments {
            match a.kind.as_str() {
                "image" => {
                    if let Some(data) = a.data.as_deref().filter(|d| !d.is_empty()) {
                        images.push(data.to_string());
                    }
                }
                "text" => {
                    if let Some(text) = a.text.as_deref().filter(|t| !t.is_empty()) {
                        content.push_str(&format!("\n\n[Attached file: {}]\n{text}", a.name));
                    }
                }
                _ => {}
            }
        }
        let mut obj = json!({ "role": m.role, "content": content });
        if !images.is_empty() {
            obj["images"] = json!(images);
        }
        messages.push(obj);
    }

    // Generation options from session settings.
    let mut opts = serde_json::Map::new();
    if let Some(t) = session.settings.temperature {
        opts.insert("temperature".into(), json!(t));
    }
    if let Some(n) = session.settings.num_ctx {
        opts.insert("num_ctx".into(), json!(n));
    }
    let options = (!opts.is_empty()).then(|| Value::Object(opts));

    let think = session.settings.think && state.runtime.model_supports_thinking(&model).await;

    // Built-in web tools when enabled and the model can call tools. Unenrolled
    // rigs get web_fetch only — web_search relays through the cloud.
    let tools_value = if session.settings.web_tools && state.runtime.model_supports_tools(&model).await
    {
        let enrolled = state.config.lock().await.is_enrolled();
        Some(if enrolled { tools::builtin_defs() } else { tools::fetch_only_defs() })
    } else {
        None
    };

    // Register the cancel flag so `local_chat_cancel` can stop this turn.
    let cancel = Arc::new(AtomicBool::new(false));
    state.cancels.lock().await.insert(request_id.clone(), cancel.clone());

    let result = run_turn(
        &app,
        &state,
        &request_id,
        &session_id,
        &model,
        messages,
        think,
        tools_value,
        options,
        cancel.clone(),
    )
    .await;

    state.cancels.lock().await.remove(&request_id);

    let turn = result.map_err(|e| e.to_string())?;
    let mut assistant = StoredMessage::new("assistant", turn.content);
    assistant.thinking = (!turn.thinking.is_empty()).then_some(turn.thinking);
    assistant.prompt_tokens = (turn.prompt_tokens > 0).then_some(turn.prompt_tokens);
    assistant.completion_tokens = (turn.completion_tokens > 0).then_some(turn.completion_tokens);
    assistant.generation_metrics = turn.generation_metrics;
    assistant.tool_limit_reached = turn.tool_limit_reached;
    assistant.tool_activity =
        (!turn.tool_activity.is_empty()).then(|| Value::Array(turn.tool_activity));
    assistant.cancelled = cancel.load(Ordering::Relaxed);

    {
        let _guard = state.chat_lock.lock().await;
        // Reload in case the session was renamed while streaming.
        if let Ok(current) = chat_store::load(&session_id) {
            session = current;
        }
        session.messages.push(assistant.clone());
        session.updated_at = chrono::Utc::now();
        chat_store::save(&session).map_err(|e| e.to_string())?;
    }

    Ok(assistant)
}

struct TurnOutput {
    content: String,
    thinking: String,
    prompt_tokens: u32,
    completion_tokens: u32,
    generation_metrics: Option<GenerationMetrics>,
    tool_limit_reached: Option<u16>,
    tool_activity: Vec<Value>,
}

#[allow(clippy::too_many_arguments)]
async fn run_turn(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
    request_id: &str,
    session_id: &str,
    model: &str,
    mut messages: Vec<Value>,
    think: bool,
    tools_value: Option<Value>,
    options: Option<Value>,
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<TurnOutput> {
    let (_, load_settings) = state.model_settings(model).await?;
    let tool_call_limit = load_settings.tool_call_limit();
    let mut out = TurnOutput {
        content: String::new(),
        thinking: String::new(),
        prompt_tokens: 0,
        completion_tokens: 0,
        generation_metrics: None,
        tool_limit_reached: None,
        tool_activity: Vec::new(),
    };
    let mut executed_tool_calls = 0usize;
    let mut synthesis_only = false;

    loop {
        let round_out = {
            let app = app.clone();
            let request_id = request_id.to_string();
            let session_id = session_id.to_string();
            state
                .runtime
                .chat_stream(
                    model,
                    Value::Array(messages.clone()),
                    think,
                    if synthesis_only { None } else { tools_value.as_ref() },
                    options.as_ref(),
                    &load_settings,
                    cancel.clone(),
                    move |delta| match delta {
                        ChatDelta::Content(s) => emit(
                            &app,
                            &request_id,
                            &session_id,
                            json!({ "type": "content", "delta": s }),
                        ),
                        ChatDelta::Thinking(s) => emit(
                            &app,
                            &request_id,
                            &session_id,
                            json!({ "type": "thinking", "delta": s }),
                        ),
                    },
                )
                .await?
        };

        out.prompt_tokens += round_out.prompt_tokens.unwrap_or(0);
        out.completion_tokens += round_out.completion_tokens.unwrap_or(0);

        // No tool calls (or cancelled) → this round's text is the final answer.
        if round_out.tool_calls.is_empty() || cancel.load(Ordering::Relaxed) {
            out.content = round_out.content;
            out.thinking = round_out.thinking;
            out.generation_metrics = round_out.generation_metrics;
            return Ok(out);
        }

        // The reserve pass is deliberately tool-disabled. Native tool calling
        // should make this unreachable, but returning here prevents a malformed
        // provider response from creating an unbounded loop.
        if synthesis_only {
            out.content = round_out.content;
            out.thinking = round_out.thinking;
            out.generation_metrics = round_out.generation_metrics;
            return Ok(out);
        }

        // Execute built-in tools: echo an assistant tool_calls message, then one
        // tool result per call, and loop so the model can use them.
        let calls: Vec<ToolCall> =
            round_out.tool_calls.into_iter().filter(|c| tools::is_builtin(&c.name)).collect();
        if calls.is_empty() {
            return Err(anyhow::anyhow!("model requested an unsupported tool"));
        }
        let assistant_calls: Vec<Value> = calls.iter().map(|c| c.to_request_value()).collect();
        messages.push(json!({ "role": "assistant", "content": "", "tool_calls": assistant_calls }));
        let remaining = tool_call_limit.saturating_sub(executed_tool_calls);
        for (index, call) in calls.iter().enumerate() {
            emit(
                app,
                request_id,
                session_id,
                json!({ "type": "tool", "name": call.name, "arguments": call.arguments.to_string() }),
            );
            let (result_text, activity, summary) = if index < remaining {
                executed_tool_calls += 1;
                run_builtin(state, call).await
            } else {
                (
                    "Tool call was not run because the maximum tool count for this message was reached. Use the completed tool results to answer the user.".into(),
                    None,
                    "not run: maximum tool count reached".into(),
                )
            };
            emit(
                app,
                request_id,
                session_id,
                json!({ "type": "tool_result", "name": call.name, "summary": summary }),
            );
            if let Some(a) = activity {
                out.tool_activity.push(a);
            }
            messages.push(json!({ "role": "tool", "tool_name": call.name, "content": result_text }));
        }

        if calls.len() >= remaining {
            out.tool_limit_reached = u16::try_from(tool_call_limit).ok();
            synthesis_only = true;
            // Reserve one final, tool-disabled request so a model that used its
            // full budget still returns an answer instead of an empty tool call.
            messages.push(json!({
                "role": "system",
                "content": "The maximum tool call count for this message has been reached. Use the completed results above to provide the final answer now. Do not request more tools."
            }));
        }
    }
}

/// Execute a built-in tool for a local turn. Like `chat::run_builtin`, errors are
/// fed back to the model as tool content rather than failing the turn; the
/// difference is `web_search` needs cloud enrollment, so unenrolled rigs get a
/// graceful explanation instead.
async fn run_builtin(state: &Arc<AppState>, call: &ToolCall) -> (String, Option<Value>, String) {
    match call.name.as_str() {
        tools::WEB_SEARCH => {
            let query = call
                .arguments
                .get("query")
                .and_then(|q| q.as_str())
                .unwrap_or("")
                .to_string();
            let count = call
                .arguments
                .get("count")
                .and_then(|c| c.as_u64())
                .unwrap_or(5)
                .clamp(1, 10) as u32;
            if query.is_empty() {
                return ("web_search error: missing 'query'".into(), None, "no query".into());
            }
            let token = match crate::worker::ensure_token(state).await {
                Ok(t) => t,
                Err(_) => {
                    return (
                        "web_search is unavailable: this rig is not connected to the cloud. \
                         Use web_fetch with a direct URL, or answer from your own knowledge."
                            .into(),
                        None,
                        "not enrolled".into(),
                    )
                }
            };
            match state.supabase.web_search(&token, "local", &query, count).await {
                Ok(crate::supabase::WebSearchOutcome::Results(results)) => {
                    let text = if results.is_empty() {
                        "No results.".to_string()
                    } else {
                        results
                            .iter()
                            .enumerate()
                            .map(|(i, r)| format!("[{}] {}\n{}\n{}", i + 1, r.title, r.url, r.snippet))
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    };
                    let citations: Vec<Value> = results
                        .iter()
                        .map(|r| json!({ "title": r.title, "url": r.url, "snippet": r.snippet }))
                        .collect();
                    let n = results.len();
                    let activity =
                        json!({ "name": tools::WEB_SEARCH, "query": query, "citations": citations });
                    (text, Some(activity), format!("{n} result{}", if n == 1 { "" } else { "s" }))
                }
                Ok(crate::supabase::WebSearchOutcome::NoKey) => (
                    "web_search is unavailable: no Brave Search API key is configured. \
                     Ask the user to add one in Settings."
                        .into(),
                    None,
                    "no API key".into(),
                ),
                Err(e) => (format!("web_search failed: {e}"), None, "search failed".into()),
            }
        }
        tools::WEB_FETCH => {
            let url = call
                .arguments
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();
            if url.is_empty() {
                return ("web_fetch error: missing 'url'".into(), None, "no url".into());
            }
            match tools::web_fetch(&state.http, &url).await {
                Ok(text) => {
                    let activity = json!({ "name": tools::WEB_FETCH, "query": url, "citations": [] });
                    (text, Some(activity), "fetched".into())
                }
                Err(e) => (format!("web_fetch failed: {e}"), None, "fetch failed".into()),
            }
        }
        other => (format!("unknown tool: {other}"), None, "unknown".into()),
    }
}
