//! Local (offline) coding turn engine. The on-disk session is the source of
//! truth; deltas stream to the webview as `local-coding` events; the assistant
//! message is persisted on completion. No cloud/Supabase involvement — this is
//! the local-first path, mirroring `local_chat.rs` but with the coding tools,
//! workspace confinement, and an in-process approval gate.

use crate::coding::{self, CodingContext};
use crate::coding_store::{self, CodingStoredMessage};
use crate::runtime::ollama::{ChatDelta, ToolCall};
use crate::runtime::tool_protocol;
use crate::AppState;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;

const EVENT: &str = "local-coding";
const MAX_TOOL_ROUNDS: usize = 24;

fn emit(app: &tauri::AppHandle, session_id: &str, message_id: &str, event: Value) {
    let _ = app.emit(
        EVENT,
        json!({ "sessionId": session_id, "messageId": message_id, "event": event }),
    );
}

/// Run one persisted local coding turn. `request_id` doubles as the streamed
/// assistant message id the frontend correlates events + approvals against.
pub async fn send(
    app: tauri::AppHandle,
    state: Arc<AppState>,
    session_id: String,
    request_id: String,
    content: String,
) -> Result<CodingStoredMessage, String> {
    // Persist the user turn up front so it survives a crash mid-generation.
    let mut session = {
        let _guard = state.chat_lock.lock().await;
        let mut session = coding_store::load(&session_id).map_err(|e| e.to_string())?;
        session.messages.push(CodingStoredMessage::new("user", content));
        if session.title == "New session" {
            if let Some(first) = session.messages.iter().find(|m| m.role == "user") {
                session.title = crate::chat_store::derive_title(&first.content);
            }
        }
        session.updated_at = chrono::Utc::now();
        coding_store::save(&session).map_err(|e| e.to_string())?;
        session
    };

    let model = session.model.clone();
    if model.is_empty() {
        return Err("no model selected".to_string());
    }

    // The workspace must exist + resolve on this machine.
    let workspace = coding::Workspace::new(&session.workspace_root).map_err(|e| e.to_string())?;
    let cx = CodingContext {
        workspace,
        policy: coding::ApprovalPolicy::parse(&session.approval_policy),
    };

    // Build the model history: coding system prompt, then prior messages.
    let mut messages: Vec<Value> = Vec::with_capacity(session.messages.len() + 1);
    messages.push(json!({ "role": "system", "content": coding::system_prompt(&cx.workspace.root_str(), cx.policy) }));
    for m in &session.messages {
        messages.push(json!({ "role": m.role, "content": m.content }));
    }

    let cancel = Arc::new(AtomicBool::new(false));
    state.cancels.lock().await.insert(request_id.clone(), cancel.clone());

    let result = run_turn(&app, &state, &cx, &session_id, &request_id, &model, messages, cancel.clone()).await;

    state.cancels.lock().await.remove(&request_id);

    let turn = match result {
        Ok(t) => t,
        Err(e) => {
            emit(&app, &session_id, &request_id, json!({ "type": "error", "message": e.to_string() }));
            return Err(e.to_string());
        }
    };
    emit(&app, &session_id, &request_id, json!({ "type": "done" }));

    let mut assistant = CodingStoredMessage::new("assistant", turn.content);
    assistant.thinking = (!turn.thinking.is_empty()).then_some(turn.thinking);
    assistant.tool_activity = (!turn.tool_activity.is_empty()).then(|| Value::Array(turn.tool_activity));
    assistant.cancelled = cancel.load(Ordering::Relaxed);

    {
        let _guard = state.chat_lock.lock().await;
        if let Ok(current) = coding_store::load(&session_id) {
            session = current;
        }
        session.messages.push(assistant.clone());
        session.updated_at = chrono::Utc::now();
        coding_store::save(&session).map_err(|e| e.to_string())?;
    }

    // Mirror to the cloud so the session is visible/continuable from the web.
    // Best-effort: an offline / unenrolled rig just keeps the local record.
    push_to_cloud(&state, &session).await;

    Ok(assistant)
}

/// Drop a session's cloud mirror so deleting it in the desktop app also removes
/// it from the web. Keyed by the on-disk session id rather than `remote_id`, so
/// it still cleans up a session that synced but never recorded its remote ids.
/// No-op when unenrolled; best-effort otherwise.
pub async fn delete_from_cloud(state: &Arc<AppState>, local_session_id: &str) {
    if !state.config.lock().await.is_enrolled() {
        return;
    }
    let token = match crate::worker::ensure_token(state).await {
        Ok(t) => t,
        Err(_) => return,
    };
    if let Err(e) = state.supabase.coding_sync_delete(&token, local_session_id).await {
        tracing::debug!("coding-sync delete failed: {e}");
    }
}

/// Mirror any session that has never reached the cloud — created while offline,
/// before the rig was enrolled, or while sync was failing. Without this, such a
/// session stays invisible to the web until its next turn happens to push it.
/// Runs once at startup, off the critical path.
pub async fn backfill_unsynced(state: Arc<AppState>) {
    if !state.config.lock().await.is_enrolled() {
        return;
    }
    let ids: Vec<String> = {
        let _guard = state.chat_lock.lock().await;
        match coding_store::list() {
            Ok(metas) => metas.into_iter().map(|m| m.id).collect(),
            Err(e) => {
                tracing::debug!("coding backfill: list failed: {e}");
                return;
            }
        }
    };
    for id in ids {
        // Reload per session and drop the guard before pushing — `push_to_cloud`
        // takes `chat_lock` itself, and it is not reentrant.
        let session = {
            let _guard = state.chat_lock.lock().await;
            match coding_store::load(&id) {
                Ok(s) => s,
                Err(_) => continue,
            }
        };
        if session.remote_id.is_some() {
            continue;
        }
        push_to_cloud(&state, &session).await;
    }
}

/// Push the on-disk transcript to Supabase via `coding-sync` (no-op when not
/// enrolled). Persists the returned remote ids on the session's first sync.
pub async fn push_to_cloud(state: &Arc<AppState>, session: &coding_store::CodingSession) {
    if !state.config.lock().await.is_enrolled() {
        return;
    }
    let token = match crate::worker::ensure_token(state).await {
        Ok(t) => t,
        Err(_) => return,
    };
    let messages: Vec<Value> = session
        .messages
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| {
            json!({
                "id": m.id, "role": m.role, "content": m.content,
                "thinking": m.thinking, "toolActivity": m.tool_activity, "createdAt": m.created_at,
            })
        })
        .collect();
    let body = json!({
        "localSessionId": session.id,
        "title": session.title,
        "model": session.model,
        "workspaceRoot": session.workspace_root,
        "approvalPolicy": session.approval_policy,
        "messages": messages,
    });
    match state.supabase.coding_sync_push(&token, body).await {
        Ok(v) => {
            let remote_id = v.get("conversationId").and_then(Value::as_str).map(str::to_string);
            let remote_ws = v.get("workspaceId").and_then(Value::as_str).map(str::to_string);
            if remote_id != session.remote_id || remote_ws != session.remote_workspace_id {
                let _guard = state.chat_lock.lock().await;
                if let Ok(mut s) = coding_store::load(&session.id) {
                    s.remote_id = remote_id;
                    s.remote_workspace_id = remote_ws;
                    coding_store::save(&s).ok();
                }
            }
        }
        Err(e) => tracing::debug!("coding-sync push failed: {e}"),
    }
}

struct TurnOutput {
    content: String,
    thinking: String,
    tool_activity: Vec<Value>,
}

#[allow(clippy::too_many_arguments)]
async fn run_turn(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
    cx: &CodingContext,
    session_id: &str,
    request_id: &str,
    model: &str,
    mut messages: Vec<Value>,
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<TurnOutput> {
    let (_, load_settings) = state.model_settings(model).await?;

    // Native tool calling only works when the model's template renders `.Tools`;
    // otherwise inject a manifest into the last user turn and parse `<tool_call>`
    // blocks ourselves (model-agnostic), exactly as the cloud path does.
    let tool_defs = coding::tool_defs_for(cx.policy, true);
    let native_tools = state.runtime.template_supports_tools(model).await;
    let prompt_tool_mode = !native_tools;
    if prompt_tool_mode {
        let manifest = tool_protocol::manifest_system_prompt(&tool_defs);
        if !manifest.is_empty() {
            if let Some(msg) = messages
                .iter_mut()
                .rev()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
            {
                let existing = msg.get("content").and_then(Value::as_str).unwrap_or("");
                msg["content"] = json!(format!("{manifest}\n\n---\n\n{existing}"));
            }
        }
    }
    let tool_names: Vec<String> = tool_defs
        .iter()
        .filter_map(|d| d.get("function").and_then(|f| f.get("name")).and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    let tools_value = native_tools.then(|| Value::Array(tool_defs));

    let mut out = TurnOutput { content: String::new(), thinking: String::new(), tool_activity: Vec::new() };

    for _round in 0..MAX_TOOL_ROUNDS {
        let round_out = {
            let app = app.clone();
            let session_id = session_id.to_string();
            let request_id = request_id.to_string();
            let mut filter = tool_protocol::ToolCallStreamFilter::new();
            state
                .runtime
                .chat_stream(
                    model,
                    Value::Array(messages.clone()),
                    false,
                    tools_value.as_ref(),
                    None,
                    &load_settings,
                    cancel.clone(),
                    move |delta| match delta {
                        ChatDelta::Content(s) => {
                            let shown = if prompt_tool_mode { filter.push(s) } else { s.to_string() };
                            if !shown.is_empty() {
                                emit(&app, &session_id, &request_id, json!({ "type": "token", "delta": shown }));
                            }
                        }
                        ChatDelta::Thinking(s) => {
                            emit(&app, &session_id, &request_id, json!({ "type": "thinking", "delta": s }));
                        }
                    },
                )
                .await?
        };

        let mut calls = round_out.tool_calls;
        if prompt_tool_mode {
            calls = tool_protocol::parse_text_tool_calls(&round_out.content, &tool_names);
        }

        // The model may explain what it is doing and then call a tool. That
        // prose has already streamed to the UI and belongs in the persisted
        // assistant message even if a later tool round returns no text. The old
        // behavior replaced it with only the final round, producing empty
        // bubbles after the live overlay was folded into the transcript.
        let visible_round = if prompt_tool_mode {
            tool_protocol::strip_tool_calls(&round_out.content)
        } else {
            round_out.content.clone()
        };
        append_round_text(&mut out.content, &visible_round);
        append_round_text(&mut out.thinking, &round_out.thinking);

        // No tool calls (or cancelled) → this round's text is the final answer.
        if calls.is_empty() || cancel.load(Ordering::Relaxed) {
            return Ok(out);
        }

        // Echo the assistant tool call(s) so the model has its own context.
        if prompt_tool_mode {
            messages.push(json!({ "role": "assistant", "content": round_out.content }));
        } else {
            let assistant_calls: Vec<Value> = calls.iter().map(|c| c.to_request_value()).collect();
            messages.push(json!({ "role": "assistant", "content": "", "tool_calls": assistant_calls }));
        }

        for call in &calls {
            let (result_text, activity) =
                run_one(app, state, cx, session_id, request_id, &cancel, call).await;
            if let Some(a) = activity {
                out.tool_activity.push(a);
            }
            push_tool_result(&mut messages, prompt_tool_mode, &call.name, &result_text);
        }

        // At the round cap `out` already contains all prose streamed so far.
    }
    Ok(out)
}

fn append_round_text(target: &mut String, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let target_has_space = target.chars().last().map(char::is_whitespace).unwrap_or(false);
    let text_has_space = text.chars().next().map(char::is_whitespace).unwrap_or(false);
    if !target.is_empty() && !target_has_space && !text_has_space {
        target.push_str("\n\n");
    }
    target.push_str(text);
}

/// Execute one coding tool call with the approval gate, streaming its events.
async fn run_one(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
    cx: &CodingContext,
    session_id: &str,
    request_id: &str,
    cancel: &Arc<AtomicBool>,
    call: &ToolCall,
) -> (String, Option<Value>) {
    emit(app, session_id, request_id, json!({ "type": "tool", "name": call.name, "arguments": call.arguments.to_string() }));

    if !coding::is_known_tool(&call.name) {
        let msg = format!("unknown tool: {}", call.name);
        emit(app, session_id, request_id, json!({ "type": "tool_result", "name": call.name, "summary": "unknown" }));
        return (msg, None);
    }

    // Preview control has a security boundary independent of workspace edit
    // mode: approve each loopback origin once per in-memory coding session.
    if call.name == "web_preview_open" {
        let url = call.arguments.get("url").and_then(Value::as_str).unwrap_or("");
        match state.preview.needs_authorization(session_id, url).await {
            Ok(Some(origin)) => {
                let invocation_id = uuid::Uuid::new_v4().to_string();
                let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
                state.coding_approvals.lock().await.insert(invocation_id.clone(), tx);
                emit(
                    app,
                    session_id,
                    request_id,
                    json!({
                        "type": "approval_needed", "invocationId": invocation_id,
                        "name": call.name,
                        "preview": format!("Allow the model to inspect and interact with {origin} for this coding session?")
                    }),
                );
                let approved = await_decision(rx, cancel).await;
                state.coding_approvals.lock().await.remove(&invocation_id);
                emit(
                    app,
                    session_id,
                    request_id,
                    json!({ "type": "approval_resolved", "invocationId": invocation_id, "decision": if approved { "approved" } else { "denied" } }),
                );
                if !approved {
                    emit(app, session_id, request_id, json!({ "type": "tool_result", "name": call.name, "summary": "denied" }));
                    return ("The user denied preview access to that origin. Do not retry it without a different user request.".into(), None);
                }
                state.preview.authorize(session_id, origin).await;
            }
            Ok(None) => {}
            Err(e) => {
                emit(app, session_id, request_id, json!({ "type": "tool_result", "name": call.name, "summary": "invalid URL" }));
                return (format!("web_preview_open failed: {e}"), None);
            }
        }
    }

    // Approval gate for mutating calls.
    if let Some(preview) = coding::approval_preview(cx, &call.name, &call.arguments) {
        let invocation_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        state.coding_approvals.lock().await.insert(invocation_id.clone(), tx);
        emit(
            app,
            session_id,
            request_id,
            json!({ "type": "approval_needed", "invocationId": invocation_id, "name": call.name, "preview": preview }),
        );
        let approved = await_decision(rx, cancel).await;
        state.coding_approvals.lock().await.remove(&invocation_id);
        emit(
            app,
            session_id,
            request_id,
            json!({ "type": "approval_resolved", "invocationId": invocation_id, "decision": if approved { "approved" } else { "denied" } }),
        );
        if !approved {
            emit(app, session_id, request_id, json!({ "type": "tool_result", "name": call.name, "summary": "denied" }));
            return (
                format!(
                    "The user denied the {} action. Do not retry it; propose an alternative or ask how to proceed.",
                    call.name
                ),
                None,
            );
        }
    }

    let host = coding::CodingHost {
        app: app.clone(),
        preview: state.preview.clone(),
        session_id: session_id.to_string(),
    };
    let run = coding::execute(cx, Some(&host), &call.name, &call.arguments).await;
    if let Some(mut event) = run.event {
        // file_edit / command event for the live trace.
        let _ = event.as_object_mut();
        emit(app, session_id, request_id, event.clone());
    }
    emit(app, session_id, request_id, json!({ "type": "tool_result", "name": call.name, "summary": run.summary }));
    (run.content, run.activity)
}

/// Wait for the approval decision, checking the cancel flag periodically.
async fn await_decision(rx: tokio::sync::oneshot::Receiver<bool>, cancel: &Arc<AtomicBool>) -> bool {
    tokio::pin!(rx);
    loop {
        tokio::select! {
            decided = &mut rx => return decided.unwrap_or(false),
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                if cancel.load(Ordering::Relaxed) {
                    return false;
                }
            }
        }
    }
}

/// Append a tool result. In prompt-injection mode the model's template ignores
/// `role:"tool"`, so replay the result as user text the template renders.
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

#[cfg(test)]
mod tests {
    use super::append_round_text;

    #[test]
    fn preserves_prose_from_tool_rounds_when_the_final_round_is_empty() {
        let mut content = String::new();
        append_round_text(&mut content, "I checked the app and will update it.");
        append_round_text(&mut content, "");
        assert_eq!(content, "I checked the app and will update it.");
    }

    #[test]
    fn separates_visible_text_from_multiple_tool_rounds() {
        let mut content = String::new();
        append_round_text(&mut content, "First step.");
        append_round_text(&mut content, "Second step.");
        assert_eq!(content, "First step.\n\nSecond step.");
    }
}
