//! Sub-agent execution: an isolated, read-only agent loop the orchestrator
//! dispatches via the `run_agent` tool. A sub-agent gets a *fresh* context (its
//! own system prompt + the delegated task — never the parent transcript), a
//! read-only tool subset, and a bounded round budget. It returns a compact text
//! summary that the orchestrator receives as the `run_agent` tool result — so
//! broad, token-hungry exploration happens off to the side and only the
//! conclusion lands in the main context window.
//!
//! Intentionally simpler than `local_coding::run_turn`: no approval gate (the
//! tools cannot mutate), no compaction, no persistence. It shares the parent's
//! cancel flag, so cancelling the turn cancels its sub-agents too.

use crate::coding::{self, AgentDef, ApprovalPolicy, CodingContext, McpAccess, Workspace};
use crate::runtime::ollama::ChatDelta;
use crate::runtime::tool_protocol;
use crate::AppState;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const MAX_SUBAGENT_ROUNDS: usize = 12;
/// Cap on the summary handed back to the orchestrator, protecting its context.
const MAX_RESULT_CHARS: usize = 8_000;

/// Run one sub-agent to completion and return its final text (the `run_agent`
/// tool result). Errors are returned as text so a failed sub-agent degrades to a
/// message the orchestrator can react to rather than aborting the parent turn.
///
/// `workspace` is the parent's (reused, still confined) and the sub-agent's
/// policy is forced to read-only regardless of the parent's. `emit` receives the
/// `subagent_started` / `subagent_result` UI events; the caller wraps them for
/// its transport (local Tauri event vs. cloud realtime), so this loop is shared
/// by both engines.
pub async fn run_agent<E: Fn(Value)>(
    state: &Arc<AppState>,
    workspace: &Workspace,
    model: &str,
    agent: &AgentDef,
    task: &str,
    cancel: Arc<AtomicBool>,
    emit: E,
) -> String {
    emit(json!({ "type": "subagent_started", "agent": agent.name, "task": task }));

    let result = run_loop(state, workspace, model, agent, task, cancel).await;
    let summary = match &result {
        Ok(text) if !text.trim().is_empty() => clamp(text.trim(), MAX_RESULT_CHARS),
        Ok(_) => "The sub-agent finished without producing a summary.".to_string(),
        Err(e) => format!("The {} sub-agent failed: {e}", agent.name),
    };

    emit(json!({ "type": "subagent_result", "agent": agent.name, "summary": summarize_line(&summary) }));
    // The orchestrator gets the full (clamped) summary; the UI event carries a
    // short one-liner for the trace row.
    format!("Result from the {} sub-agent:\n\n{summary}", agent.name)
}

async fn run_loop(
    state: &Arc<AppState>,
    workspace: &Workspace,
    model: &str,
    agent: &AgentDef,
    task: &str,
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<String> {
    // A read-only, MCP-free context. Reads run freely; the ReadOnly policy makes
    // `coding::execute` refuse any mutating call the model might still emit.
    let cx = CodingContext {
        workspace: workspace.clone(),
        policy: ApprovalPolicy::ReadOnly,
        mcp: McpAccess::disabled(),
    };

    // Only this agent's allowed read-only tools, drawn from the canonical defs.
    let allowed: Vec<Value> = coding::tool_defs()
        .into_iter()
        .filter(|d| {
            d.pointer("/function/name")
                .and_then(Value::as_str)
                .map(|n| agent.tools.iter().any(|t| t == n))
                .unwrap_or(false)
        })
        .collect();
    let tool_names: Vec<String> = allowed
        .iter()
        .filter_map(|d| d.pointer("/function/name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();

    let (_, settings) = state.model_settings(model).await?;
    let native_tools = state.runtime.template_supports_tools(model).await;
    let prompt_tool_mode = !native_tools;
    let tools_value = native_tools.then(|| Value::Array(allowed.clone()));
    let options = settings.context_size.map(|size| json!({ "num_ctx": size }));

    let system = format!(
        "{}\n\nWorkspace root: {}\nAll paths are relative to it; you cannot read outside it.",
        agent.system_prompt,
        cx.workspace.root_str()
    );
    let mut user_task = task.to_string();
    if prompt_tool_mode {
        let manifest = tool_protocol::manifest_system_prompt(&allowed);
        user_task = format!("{manifest}\n\n---\n\n{user_task}");
    }
    let mut messages = vec![
        json!({ "role": "system", "content": system }),
        json!({ "role": "user", "content": user_task }),
    ];

    let mut answer = String::new();
    for _round in 0..MAX_SUBAGENT_ROUNDS {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let round_out = {
            let mut filter = tool_protocol::ToolCallStreamFilter::new();
            state
                .runtime
                .chat_stream(
                    model,
                    Value::Array(messages.clone()),
                    false,
                    tools_value.as_ref(),
                    options.as_ref(),
                    &settings,
                    cancel.clone(),
                    move |delta| {
                        // Sub-agent tokens are consumed to drive the loop but not
                        // streamed into the parent transcript; the UI shows the
                        // agent's start + final summary instead.
                        if let ChatDelta::Content(s) = delta {
                            let _ = filter.push(s);
                        }
                    },
                )
                .await?
        };

        let calls = if prompt_tool_mode {
            tool_protocol::parse_text_tool_calls(&round_out.content, &tool_names)
        } else {
            round_out.tool_calls.clone()
        };

        let visible = if prompt_tool_mode {
            tool_protocol::strip_tool_calls(&round_out.content)
        } else {
            round_out.content.clone()
        };
        if !visible.trim().is_empty() {
            if !answer.is_empty() {
                answer.push_str("\n\n");
            }
            answer.push_str(visible.trim());
        }

        if calls.is_empty() || cancel.load(Ordering::Relaxed) {
            return Ok(answer);
        }

        // Echo the assistant tool call(s), then execute each read-only tool.
        if prompt_tool_mode {
            messages.push(json!({ "role": "assistant", "content": round_out.content }));
        } else {
            let assistant_calls: Vec<Value> = calls.iter().map(|c| c.to_request_value()).collect();
            messages.push(json!({ "role": "assistant", "content": "", "tool_calls": assistant_calls }));
        }
        for call in &calls {
            let run = coding::execute(&cx, None, &call.name, &call.arguments).await;
            if prompt_tool_mode {
                messages.push(json!({
                    "role": "user",
                    "content": format!("<tool_response name=\"{}\">\n{}\n</tool_response>", call.name, run.content),
                }));
            } else {
                messages.push(json!({ "role": "tool", "tool_name": call.name, "content": run.content }));
            }
        }
    }
    // Hit the round cap: return whatever prose accumulated, with a note.
    if answer.trim().is_empty() {
        answer = format!(
            "(reached the {MAX_SUBAGENT_ROUNDS}-round limit without a final summary)"
        );
    }
    Ok(answer)
}

fn clamp(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…[truncated]");
    out
}

/// A single-line, length-bounded version of a summary for the trace-row event.
fn summarize_line(s: &str) -> String {
    let line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    line.chars().take(160).collect()
}
