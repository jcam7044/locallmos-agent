//! Chat turn handling: claim a pending assistant message, stream the reply from
//! Ollama, and relay token deltas to the web over the agent's Realtime
//! websocket (rig-scoped broadcast). Safe to call from both the Realtime handler
//! and the fallback poll — the claim makes processing single-shot.
//!
//! Resilience: the *authoritative* result is the persisted `chat_messages` row,
//! never the ephemeral broadcast. So disconnects degrade gracefully rather than
//! losing the answer:
//!   * Socket down when the turn starts → `join` fails, `ws_ready = false`: we
//!     skip live streaming but still generate + persist the reply, which the web
//!     picks up via postgres_changes (appears at completion instead of streaming).
//!   * Socket drops mid-stream → `broadcast` calls no-op (`.ok()`); generation
//!     continues and the final content is still persisted. Only that turn's live
//!     streaming is lost — not the answer.
//!   * The Realtime loop (`realtime::run`) reconnects within ~5s, and the 30s
//!     fallback poll in `worker.rs` picks up any assistant turn whose INSERT
//!     event was missed while the socket was down.
//! A mid-stream socket drop is not retried for the *current* turn; that would
//! need per-turn resumable streaming (out of scope for v1).

use crate::coding;
use crate::runtime::ollama::{ChatDelta, ToolCall};
use crate::runtime::tool_protocol;
use crate::runtime::tools;
use crate::supabase::{ChatPending, WebSearchOutcome};
use crate::AppState;
use anyhow::{anyhow, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::Notify;
use uuid::Uuid;

/// Mirror one stream event to the local webview so a web-initiated turn on a
/// mirrored (local-origin) coding session streams live in the desktop UI. Keyed
/// by the on-disk `sessionId`; a no-op for pure cloud sessions (no local UI) and
/// in headless service mode (no app handle).
fn emit_local(state: &Arc<AppState>, local_session_id: Option<&str>, message_id: &str, event: &Value) {
    let Some(session_id) = local_session_id else {
        return;
    };
    if let Some(app) = state.app.get() {
        let _ = app.emit(
            "local-coding",
            json!({ "sessionId": session_id, "messageId": message_id, "event": event }),
        );
    }
}

/// A streamed delta batched by the broadcaster into a `token`/`thinking` event,
/// or a discrete tool-progress event flushed immediately.
enum StreamDelta {
    Content(String),
    Thinking(String),
    /// A built-in tool is about to run (name + JSON-string arguments).
    Tool(String, String),
    /// A built-in tool finished (name + short human summary).
    ToolResult(String, String),
    /// A pre-built coding event (file_edit / command / approval_needed /
    /// approval_resolved) broadcast verbatim (seq is stamped by the broadcaster).
    Event(Value),
}

/// Broadcast event name on the `chat:{id}` channel (mirrors CHAT_STREAM_EVENT
/// in packages/shared/src/chat.ts).
const CHAT_EVENT: &str = "chunk";
/// Flush cadence / size for batching token deltas into broadcasts.
const FLUSH_INTERVAL: Duration = Duration::from_millis(80);
const FLUSH_CHARS: usize = 24;
/// How often to re-emit the "loading" ping while a cold model loads, so a
/// late-subscribing client still catches it before the first token.
const LOADING_HEARTBEAT: Duration = Duration::from_millis(1500);

fn topic(message_id: &str) -> String {
    format!("chat:{message_id}")
}

/// Append a tool result to the running message list. In prompt-injection mode
/// the model's template ignores `role:"tool"` messages, so the result is
/// replayed as plain user text the template renders; otherwise the standard
/// Ollama tool-result shape is used.
fn push_tool_result(messages: &mut Vec<Value>, prompt_tool_mode: bool, name: &str, content: &str) {
    if prompt_tool_mode {
        messages.push(json!({
            "role": "user",
            "content": format!("<tool_response name=\"{name}\">\n{content}\n</tool_response>"),
        }));
    } else {
        messages.push(json!({ "role": "tool", "tool_name": name, "content": content }));
    }
}

/// Content to persist for a turn. In prompt-injection mode any raw `<tool_call>`
/// syntax is stripped so a partial/round-capped answer never shows call markup.
fn finalize_content(prompt_tool_mode: bool, content: String) -> String {
    if prompt_tool_mode {
        tool_protocol::strip_tool_calls(&content)
    } else {
        content
    }
}

/// Process one pending assistant turn end-to-end.
pub async fn process(state: &Arc<AppState>, pending: ChatPending) -> Result<()> {
    let token = crate::worker::ensure_token(state).await?;

    // Single-shot claim (pending → streaming); losers just return.
    if !state.supabase.claim_chat_message(&token, &pending.id).await? {
        return Ok(());
    }

    let model = match pending.model.clone() {
        Some(m) if !m.is_empty() => m,
        _ => {
            let msg = "no model specified for chat turn";
            state
                .supabase
                .update_chat_message(&token, &pending.id, "error", None, None, Some(msg), None, None, None, None)
                .await
                .ok();
            return Err(anyhow!(msg));
        }
    };
    let (_, load_settings) = state.model_settings(&model).await?;

    // Build the Ollama chat history from prior completed messages, folding in
    // attachments: images become per-message base64 `images`; documents have
    // their extracted text appended to the message content.
    let context = state
        .supabase
        .fetch_chat_context(&token, &pending.conversation_id)
        .await?;
    let mut messages: Vec<Value> = Vec::with_capacity(context.len());
    for m in &context {
        let mut content = m.content.clone();
        let mut images: Vec<String> = Vec::new();
        for a in &m.attachments {
            match a.kind.as_str() {
                "image" => match state.supabase.download_attachment(&token, &a.storage_path).await {
                    Ok(bytes) => {
                        images.push(base64::engine::general_purpose::STANDARD.encode(bytes))
                    }
                    Err(e) => tracing::warn!("chat {}: image download failed: {e}", pending.id),
                },
                "document" => {
                    if let Some(text) = a.extracted_text.as_deref().filter(|t| !t.is_empty()) {
                        content.push_str(&format!("\n\n[Attached file]\n{text}"));
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

    // Coding session? Resolve the confined workspace + approval policy and
    // prepend the coding system prompt. A missing/inaccessible workspace fails
    // the turn cleanly rather than silently running unscoped.
    // When the coding conversation mirrors an on-disk local session, the agent
    // streams to the desktop UI and writes the result back to disk on completion.
    let mut local_session_id: Option<String> = None;
    let coding_ctx = match state
        .supabase
        .fetch_coding_context(&token, &pending.conversation_id)
        .await
    {
        Ok(Some(meta)) => match coding::Workspace::new(&meta.root_path) {
            Ok(workspace) => {
                local_session_id = meta.local_session_id.clone();
                let policy = coding::ApprovalPolicy::parse(&meta.approval_policy);
                // Cloud-driven coding turns get their MCP tools from the control
                // plane's platform_tools. Start the rig's enabled servers so the
                // local snapshot is populated (accurate gating + previews) and
                // freeze it for the turn.
                state.mcp.ensure_enabled_started().await;
                let mcp = coding::McpAccess::frozen(state.mcp.clone());
                messages.insert(
                    0,
                    json!({ "role": "system", "content": coding::system_prompt(&workspace.root_str(), policy, &mcp) }),
                );
                Some(coding::CodingContext { workspace, policy, mcp })
            }
            Err(e) => {
                let msg = format!("coding workspace unavailable: {e}");
                state
                    .supabase
                    .update_chat_message(&token, &pending.id, "error", None, None, Some(&msg), None, None, None, None)
                    .await
                    .ok();
                return Err(anyhow!(msg));
            }
        },
        Ok(None) => None,
        Err(e) => {
            tracing::warn!("chat {}: coding context lookup failed: {e}", pending.id);
            None
        }
    };

    // Join the private broadcast channel over the websocket. If the socket is
    // down we still generate + persist the reply, just without live streaming.
    let chan = topic(&pending.id);
    let ws_ready = state.realtime.join(&chan, &token).await.is_ok();
    if !ws_ready {
        tracing::warn!("chat {}: realtime unavailable, streaming disabled", pending.id);
    }

    // Async broadcaster: batches content + thinking deltas from the sync stream
    // callback into `token`/`thinking` events on the shared channel.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamDelta>();
    // Coding turns also mirror every stream event to the local webview (the
    // device can't receive its own Realtime broadcast, so the local UI relies on
    // these Tauri events for live streaming).
    let broadcaster = {
        let state = state.clone();
        let chan = chan.clone();
        let local_sid = local_session_id.clone();
        let msg_id = pending.id.clone();
        tokio::spawn(async move {
            let mut seq: u64 = 0;
            let mut content = String::new();
            let mut thinking = String::new();
            let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
            ticker.tick().await; // consume immediate first tick

            // Flush a non-empty buffer as a typed event, bumping the shared seq.
            macro_rules! emit {
                ($ty:expr, $buf:expr) => {{
                    if !$buf.is_empty() {
                        let payload = json!({ "type": $ty, "seq": seq, "delta": $buf.as_str() });
                        seq += 1;
                        if ws_ready {
                            state.realtime.broadcast(&chan, CHAT_EVENT, payload.clone()).await.ok();
                        }
                        emit_local(&state, local_sid.as_deref(), &msg_id, &payload);
                        $buf.clear();
                    }
                }};
            }

            // Send a discrete typed tool event immediately, flushing any pending
            // text first so ordering is preserved.
            macro_rules! emit_tool {
                ($payload:expr) => {{
                    emit!("thinking", thinking);
                    emit!("token", content);
                    let mut p = $payload;
                    p["seq"] = json!(seq);
                    seq += 1;
                    if ws_ready {
                        state.realtime.broadcast(&chan, CHAT_EVENT, p.clone()).await.ok();
                    }
                    emit_local(&state, local_sid.as_deref(), &msg_id, &p);
                }};
            }

            loop {
                tokio::select! {
                    maybe = rx.recv() => match maybe {
                        Some(StreamDelta::Content(d)) => {
                            content.push_str(&d);
                            if content.len() >= FLUSH_CHARS { emit!("token", content); }
                        }
                        Some(StreamDelta::Thinking(d)) => {
                            thinking.push_str(&d);
                            if thinking.len() >= FLUSH_CHARS { emit!("thinking", thinking); }
                        }
                        Some(StreamDelta::Tool(name, arguments)) => {
                            emit_tool!(json!({ "type": "tool", "name": name, "arguments": arguments }));
                        }
                        Some(StreamDelta::ToolResult(name, summary)) => {
                            emit_tool!(json!({ "type": "tool_result", "name": name, "summary": summary }));
                        }
                        Some(StreamDelta::Event(payload)) => {
                            emit_tool!(payload);
                        }
                        None => {
                            emit!("thinking", thinking);
                            emit!("token", content);
                            break;
                        }
                    },
                    _ = ticker.tick() => {
                        emit!("thinking", thinking);
                        emit!("token", content);
                    }
                }
            }
            seq
        })
    };

    // Register a cancel flag so a Stop request can abort this turn.
    let cancel = Arc::new(AtomicBool::new(false));
    state.cancels.lock().await.insert(pending.id.clone(), cancel.clone());

    // If the model isn't resident, Ollama blocks the chat request while it loads
    // (tens of seconds for large models). Heartbeat a "loading" ping so the web
    // shows a loading state instead of looking hung; stop it on the first token.
    let stop_loading = Arc::new(Notify::new());
    let loading_task = if ws_ready && !state.runtime.is_model_loaded(&model).await {
        let state = state.clone();
        let chan = chan.clone();
        let stop = stop_loading.clone();
        let model = model.clone();
        Some(tokio::spawn(async move {
            let payload = json!({ "type": "loading", "model": model });
            loop {
                state.realtime.broadcast(&chan, CHAT_EVENT, payload.clone()).await.ok();
                tokio::select! {
                    _ = stop.notified() => break,
                    _ = tokio::time::sleep(LOADING_HEARTBEAT) => {}
                }
            }
        }))
    } else {
        None
    };

    // Assemble tool schemas. Protocol v1 uses only the immutable server-authored
    // platform snapshot; protocol v0 retains the legacy Brave/web-fetch path for
    // older control planes during the agent rollout. These schemas are built
    // independently of how they're delivered to the model (native vs prompt).
    let platform_tools = if pending.tool_protocol_version >= 1 {
        tools::platform_tools(pending.platform_tools.as_ref())
    } else {
        Vec::new()
    };
    let mut tool_defs: Vec<Value> = Vec::new();
    if !platform_tools.is_empty() {
        tool_defs.extend(tools::platform_defs(&platform_tools));
    } else if pending.web_search {
        if let Some(arr) = tools::builtin_defs().as_array() {
            tool_defs.extend(arr.iter().cloned());
        }
    }
    if let Some(reqt) = pending.request_tools.as_ref().and_then(|v| v.as_array()) {
        // A caller-defined function must not shadow a platform capability.
        for def in reqt {
            let name = def
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str);
            if !platform_tools.iter().any(|tool| Some(tool.name.as_str()) == name) {
                tool_defs.push(def.clone());
            }
        }
    }

    // Choose the tool-delivery mode. Native tool calling only works when the
    // model's chat template actually renders `.Tools`; a model can advertise the
    // "tools" capability yet ship a passthrough template that never injects the
    // schema (the model then invents a call as plain text the native parser
    // drops). For those, inject the tools into the system prompt and parse
    // `<tool_call>` blocks ourselves — keeping the feature model-agnostic.
    let native_tools =
        !tool_defs.is_empty() && state.runtime.template_supports_tools(&model).await;
    let prompt_tool_mode = !tool_defs.is_empty() && !native_tools;
    if prompt_tool_mode {
        let manifest = tool_protocol::manifest_system_prompt(&tool_defs);
        if !manifest.is_empty() {
            // Inject into the latest user turn, not a system message. The very
            // templates that put us in this mode (e.g. a bare `{{ .Prompt }}`)
            // render only the user prompt and drop system/assistant content, so
            // the user turn is the one delivery point guaranteed to reach the
            // model. Templates that do render `.Messages` handle this fine too.
            let last_user = messages
                .iter_mut()
                .rev()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("user"));
            match last_user {
                Some(msg) => {
                    let existing = msg.get("content").and_then(Value::as_str).unwrap_or("");
                    msg["content"] = json!(format!("{manifest}\n\n---\n\n{existing}"));
                }
                None => messages.push(json!({ "role": "user", "content": manifest })),
            }
        }
    }
    // Names of the tools we offered — used in prompt mode to tell a real tool
    // call from a JSON-shaped answer. Captured before `tool_defs` is consumed.
    let tool_names: Vec<String> = tool_defs
        .iter()
        .filter_map(|d| {
            d.get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .collect();
    let tools_value = native_tools.then(|| Value::Array(tool_defs));

    // Tool loop: call Ollama; if it asks for a built-in tool, run it and feed the
    // result back; caller (passthrough) tool calls are returned unexecuted. Only
    // the final, no-tool round streams the answer (tool rounds have no content).
    const MAX_TOOL_ROUNDS: usize = 5;
    let first_token = Arc::new(AtomicBool::new(false));
    let mut prompt_total: u32 = 0;
    let mut completion_total: u32 = 0;
    let mut tool_activity: Vec<Value> = Vec::new();
    let mut passthrough_calls: Vec<Value> = Vec::new();
    let mut final_content = String::new();
    let mut final_thinking = String::new();
    let mut loop_err: Option<anyhow::Error> = None;

    for round in 0..MAX_TOOL_ROUNDS {
        let result = {
            let first_token = first_token.clone();
            let stop_loading = stop_loading.clone();
            let tx = tx.clone();
            // In prompt-injection mode the model writes its tool call as content;
            // keep that bookkeeping out of the user-visible stream while the full
            // raw content still accumulates in `out.content` for parsing.
            let mut filter = tool_protocol::ToolCallStreamFilter::new();
            state
                .runtime
                .chat_stream(
                    &model,
                    Value::Array(messages.clone()),
                    pending.think,
                    tools_value.as_ref(),
                    None,
                    &load_settings,
                    cancel.clone(),
                    move |delta| {
                        if !first_token.swap(true, Ordering::Relaxed) {
                            stop_loading.notify_one();
                        }
                        let _ = match delta {
                            ChatDelta::Content(s) => {
                                let shown = if prompt_tool_mode {
                                    filter.push(s)
                                } else {
                                    s.to_string()
                                };
                                if shown.is_empty() {
                                    Ok(())
                                } else {
                                    tx.send(StreamDelta::Content(shown))
                                }
                            }
                            ChatDelta::Thinking(s) => tx.send(StreamDelta::Thinking(s.to_string())),
                        };
                    },
                )
                .await
        };

        let mut out = match result {
            Ok(o) => o,
            Err(e) => {
                loop_err = Some(e);
                break;
            }
        };
        prompt_total += out.prompt_tokens.unwrap_or(0);
        completion_total += out.completion_tokens.unwrap_or(0);
        // Recover text-format tool calls the model emitted as content (native
        // `tool_calls` is always empty in prompt-injection mode).
        if prompt_tool_mode {
            out.tool_calls = tool_protocol::parse_text_tool_calls(&out.content, &tool_names);
            if out.tool_calls.is_empty() {
                // Surface the raw output so an unrecognised call format is
                // diagnosable from the terminal without a debug build.
                tracing::info!(
                    chat = %pending.id,
                    round,
                    content = %out.content.trim(),
                    "prompt-tool round parsed no tool call"
                );
            } else {
                let names: Vec<&str> = out.tool_calls.iter().map(|c| c.name.as_str()).collect();
                tracing::info!(chat = %pending.id, round, ?names, "parsed tool call(s) from text");
            }
        }

        // No tool calls (or cancelled) → this round's text is the final answer.
        if out.tool_calls.is_empty() || cancel.load(Ordering::Relaxed) {
            final_content = out.content;
            final_thinking = out.thinking;
            break;
        }

        let content = out.content;
        let thinking = out.thinking;
        let mut platform_calls: Vec<(ToolCall, tools::PlatformTool)> = Vec::new();
        let mut builtin: Vec<ToolCall> = Vec::new();
        let mut passthrough: Vec<ToolCall> = Vec::new();
        for call in out.tool_calls {
            if let Some(tool) = platform_tools.iter().find(|tool| tool.name == call.name) {
                platform_calls.push((call, tool.clone()));
            } else if pending.tool_protocol_version == 0 && tools::is_builtin(&call.name) {
                builtin.push(call);
            } else {
                // Never execute a tool merely because the model invented a name.
                passthrough.push(call);
            }
        }

        // Caller (passthrough) tool calls are returned unexecuted for the API.
        if !passthrough.is_empty() {
            for (i, c) in passthrough.iter().enumerate() {
                passthrough_calls.push(json!({
                    "id": format!("call_{round}_{i}"),
                    "type": "function",
                    "function": { "name": c.name, "arguments": c.arguments.to_string() },
                }));
            }
            final_content = finalize_content(prompt_tool_mode, content);
            final_thinking = thinking;
            break;
        }

        // Echo the assistant's tool call, then append one result per call, and
        // loop so the model can use them. Prompt-injection templates ignore
        // `tool_calls`/`role:"tool"` structure, so in that mode the call and its
        // results are replayed as plain text the template will actually render.
        if prompt_tool_mode {
            messages.push(json!({ "role": "assistant", "content": content }));
        } else {
            let assistant_calls: Vec<Value> = platform_calls
                .iter()
                .map(|(c, _)| c.to_request_value())
                .chain(builtin.iter().map(|c| c.to_request_value()))
                .collect();
            messages.push(json!({ "role": "assistant", "content": "", "tool_calls": assistant_calls }));
        }
        for (call, tool) in &platform_calls {
            let _ = tx.send(StreamDelta::Tool(call.name.clone(), call.arguments.to_string()));
            // Local coding tools run on the rig itself (with the approval gate);
            // hosted tools relay to the cloud gateway as before.
            let (result_text, activity, summary) = if tool.execution == "local" {
                run_local_tool(state, coding_ctx.as_ref(), &token, &pending.id, &tx, &cancel, tool, call).await
            } else {
                let invocation_id = Uuid::new_v4().to_string();
                run_platform_tool(state, &token, &pending.id, &invocation_id, tool, call).await
            };
            let _ = tx.send(StreamDelta::ToolResult(call.name.clone(), summary));
            if let Some(a) = activity {
                tool_activity.push(a);
            }
            push_tool_result(&mut messages, prompt_tool_mode, &call.name, &result_text);
        }
        for call in &builtin {
            let _ = tx.send(StreamDelta::Tool(call.name.clone(), call.arguments.to_string()));
            let (result_text, activity, summary) = run_builtin(state, &token, &pending.id, call).await;
            let _ = tx.send(StreamDelta::ToolResult(call.name.clone(), summary));
            if let Some(a) = activity {
                tool_activity.push(a);
            }
            push_tool_result(&mut messages, prompt_tool_mode, &call.name, &result_text);
        }
        // Hit the round cap without a final answer: persist what we have.
        if round == MAX_TOOL_ROUNDS - 1 {
            final_content = finalize_content(prompt_tool_mode, content);
            final_thinking = thinking;
        }
    }

    state.cancels.lock().await.remove(&pending.id);
    drop(tx);
    // Ensure the heartbeat stops even if the turn ended before any token.
    stop_loading.notify_one();
    if let Some(t) = loading_task {
        let _ = t.await;
    }
    let last_seq = broadcaster.await.unwrap_or(0);

    let tool_calls_json = (!passthrough_calls.is_empty()).then(|| Value::Array(passthrough_calls));
    let tool_activity_json = (!tool_activity.is_empty()).then(|| Value::Array(tool_activity));

    let outcome = match loop_err {
        None => {
            let thinking = (!final_thinking.is_empty()).then_some(final_thinking.as_str());
            state
                .supabase
                .update_chat_message(
                    &token,
                    &pending.id,
                    "done",
                    Some(&final_content),
                    thinking,
                    None,
                    Some(prompt_total),
                    Some(completion_total),
                    tool_calls_json.as_ref(),
                    tool_activity_json.as_ref(),
                )
                .await?;
            // Web-initiated turn on a mirrored session: pull the now-complete
            // cloud transcript into the on-disk session so the desktop UI shows
            // it, then signal done (order matters — the UI reloads on `done`).
            if let Some(lsid) = &local_session_id {
                write_back_to_disk(state, &token, &pending.conversation_id, lsid).await;
            }
            let done = json!({ "type": "done", "seq": last_seq });
            if ws_ready {
                state.realtime.broadcast(&chan, CHAT_EVENT, done.clone()).await.ok();
            }
            emit_local(state, local_session_id.as_deref(), &pending.id, &done);
            Ok(())
        }
        Some(e) => {
            let msg = e.to_string();
            let error_event = json!({ "type": "error", "seq": last_seq, "message": msg });
            if ws_ready {
                state.realtime.broadcast(&chan, CHAT_EVENT, error_event.clone()).await.ok();
            }
            emit_local(state, local_session_id.as_deref(), &pending.id, &error_event);
            state
                .supabase
                .update_chat_message(&token, &pending.id, "error", None, None, Some(&msg), None, None, None, None)
                .await
                .ok();
            Err(e)
        }
    };

    if ws_ready {
        state.realtime.leave(&chan).await;
    }
    outcome
}

/// Overwrite an on-disk coding session's transcript from the cloud conversation
/// after a web-initiated turn, so the desktop app reflects it. The cloud is the
/// merge point when enrolled (local turns push up; web turns pull down), so a
/// full replace is safe and idempotent (shared message ids).
async fn write_back_to_disk(state: &Arc<AppState>, token: &str, conversation_id: &str, local_session_id: &str) {
    let value = match state.supabase.get_coding_messages(token, conversation_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("coding write-back fetch failed: {e}");
            return;
        }
    };
    let Some(rows) = value.as_array() else { return };
    let messages: Vec<crate::coding_store::CodingStoredMessage> = rows
        .iter()
        .filter_map(|r| {
            let role = r.get("role").and_then(Value::as_str)?;
            if role == "system" {
                return None; // the coding system prompt is applied at runtime
            }
            if r.get("status").and_then(Value::as_str) != Some("done") {
                return None; // skip an in-flight sibling turn
            }
            let created_at = r
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&chrono::Utc))
                .unwrap_or_else(chrono::Utc::now);
            Some(crate::coding_store::CodingStoredMessage {
                id: r.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                role: role.to_string(),
                content: r.get("content").and_then(Value::as_str).unwrap_or_default().to_string(),
                thinking: r.get("thinking").and_then(Value::as_str).map(str::to_string),
                tool_activity: r.get("tool_activity").filter(|v| !v.is_null()).cloned(),
                cancelled: false,
                created_at,
            })
        })
        .collect();

    let _guard = state.chat_lock.lock().await;
    if let Ok(mut session) = crate::coding_store::load(local_session_id) {
        session.messages = messages;
        session.updated_at = chrono::Utc::now();
        crate::coding_store::save(&session).ok();
    }
}

/// Execute a server-authorized hosted platform tool through the cloud gateway.
/// Errors become model-visible tool content so a single provider failure does
/// not discard the whole answer.
async fn run_platform_tool(
    state: &Arc<AppState>,
    token: &str,
    message_id: &str,
    invocation_id: &str,
    tool: &tools::PlatformTool,
    call: &ToolCall,
) -> (String, Option<Value>, String) {
    match state
        .supabase
        .execute_tool(token, message_id, invocation_id, &tool.id, &call.arguments)
        .await
    {
        Ok(result) => {
            let activity = result.activity.or_else(|| {
                Some(json!({
                    "name": call.name,
                    "toolId": tool.id,
                    "provider": tool.provider,
                    "invocationId": invocation_id,
                    "status": "succeeded",
                    "summary": result.summary,
                    "citations": [],
                }))
            });
            (result.content, activity, result.summary)
        }
        Err(e) => {
            let summary = "tool failed".to_string();
            let activity = json!({
                "name": call.name,
                "toolId": tool.id,
                "provider": tool.provider,
                "invocationId": invocation_id,
                "status": "failed",
                "summary": summary,
                "citations": [],
            });
            (format!("{} failed: {e}", call.name), Some(activity), summary)
        }
    }
}

/// Execute one local coding tool against the confined workspace. Mutating calls
/// (writes, commands, non-read git) pause on an `awaiting_approval` invocation
/// until either surface sets a decision; reads run immediately. Structured
/// `file_edit`/`command`/`approval_*` events stream to the UI via `tx`.
#[allow(clippy::too_many_arguments)]
async fn run_local_tool(
    state: &Arc<AppState>,
    coding_ctx: Option<&coding::CodingContext>,
    token: &str,
    message_id: &str,
    tx: &tokio::sync::mpsc::UnboundedSender<StreamDelta>,
    cancel: &Arc<AtomicBool>,
    tool: &tools::PlatformTool,
    call: &ToolCall,
) -> (String, Option<Value>, String) {
    let Some(cx) = coding_ctx else {
        return (
            "coding tools are unavailable for this turn".into(),
            None,
            "unavailable".into(),
        );
    };
    let invocation_id = Uuid::new_v4().to_string();
    let args_hash = sha256_hex(&call.arguments);
    // MCP tools are not tool_catalog rows, so their invocation carries a null
    // catalog tool_id (see supabase migration 0040).
    let catalog_tool_id = (tool.provider != "mcp").then_some(tool.id.as_str());

    if let Some(preview) = coding::approval_preview(cx, &call.name, &call.arguments) {
        state
            .supabase
            .create_tool_invocation(
                token, message_id, &invocation_id, catalog_tool_id, &tool.provider, &args_hash,
                Some(&preview), &call.name, true,
            )
            .await
            .ok();
        let _ = tx.send(StreamDelta::Event(json!({
            "type": "approval_needed", "invocationId": invocation_id,
            "name": call.name, "preview": preview,
        })));
        let decision = await_decision(state, token, &invocation_id, cancel).await;
        let _ = tx.send(StreamDelta::Event(json!({
            "type": "approval_resolved", "invocationId": invocation_id, "decision": decision,
        })));
        if decision != "approved" {
            state
                .supabase
                .finalize_tool_invocation(token, &invocation_id, "cancelled", &decision)
                .await
                .ok();
            return (
                format!(
                    "The user denied the {} action. Do not retry it; propose an alternative or ask the user how to proceed.",
                    call.name
                ),
                None,
                "denied".into(),
            );
        }
    } else {
        state
            .supabase
            .create_tool_invocation(
                token, message_id, &invocation_id, catalog_tool_id, &tool.provider, &args_hash,
                None, &call.name, false,
            )
            .await
            .ok();
    }

    let run = coding::execute(cx, None, &call.name, &call.arguments).await;
    if let Some(mut event) = run.event {
        event["invocationId"] = json!(invocation_id);
        let _ = tx.send(StreamDelta::Event(event));
    }
    let status = if run.summary == "error" { "failed" } else { "succeeded" };
    state
        .supabase
        .finalize_tool_invocation(token, &invocation_id, status, &run.summary)
        .await
        .ok();
    (run.content, run.activity, run.summary)
}

/// Poll the invocation's decision until it is approved/denied, the turn is
/// cancelled, or the approval window elapses. Both surfaces write the decision
/// (web via the set_tool_decision RPC, local app via a device PATCH).
async fn await_decision(
    state: &Arc<AppState>,
    token: &str,
    invocation_id: &str,
    cancel: &Arc<AtomicBool>,
) -> String {
    const TIMEOUT: Duration = Duration::from_secs(1800);
    const POLL: Duration = Duration::from_millis(1500);
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return "denied".into();
        }
        if let Ok(Some(decision)) = state.supabase.poll_tool_decision(token, invocation_id).await {
            if decision == "approved" || decision == "denied" {
                return decision;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return "denied".into();
        }
        tokio::time::sleep(POLL).await;
    }
}

/// SHA-256 of a tool call's arguments (stable across retries; matches the
/// gateway's arguments_hash convention).
fn sha256_hex(args: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(args.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

/// Execute a single built-in tool call. Returns `(tool_message_content, activity,
/// short_summary)`. `activity` is a `ToolActivity` JSON row persisted for the web
/// to render citations; the content string is fed back to the model as the tool's
/// result. Errors are returned as content (so the model can react) rather than
/// failing the turn.
async fn run_builtin(
    state: &Arc<AppState>,
    token: &str,
    message_id: &str,
    call: &ToolCall,
) -> (String, Option<Value>, String) {
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
            match state.supabase.web_search(token, message_id, &query, count).await {
                Ok(WebSearchOutcome::Results(results)) => {
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
                    let activity = json!({ "name": tools::WEB_SEARCH, "query": query, "citations": citations });
                    (text, Some(activity), format!("{n} result{}", if n == 1 { "" } else { "s" }))
                }
                Ok(WebSearchOutcome::NoKey) => (
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
