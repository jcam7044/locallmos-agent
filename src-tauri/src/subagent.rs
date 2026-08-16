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
use crate::relay_inference::RelayLlama;
use crate::runtime::ollama::ChatDelta;
use crate::runtime::{tool_protocol, ChatOutput, ModelLoadSettings, Runtime};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const MAX_SUBAGENT_ROUNDS: usize = 12;
/// Cap on the summary handed back to the orchestrator, protecting its context.
const MAX_RESULT_CHARS: usize = 8_000;

/// Which engine a sub-agent's inference runs on: this rig's local runtime, or a
/// relay to a serving peer in the group. The sub-agent loop is identical either
/// way — only `template_supports_tools` and `chat_stream` differ, so this thin
/// enum keeps the loop (and `Runtime`) untouched.
pub enum SubagentRuntime<'a> {
    Local(&'a Runtime),
    Relay(RelayLlama),
}

impl SubagentRuntime<'_> {
    async fn template_supports_tools(&self, model: &str) -> bool {
        match self {
            SubagentRuntime::Local(r) => r.template_supports_tools(model).await,
            // The peer serves under llama-server `--jinja` (native, grammar-
            // constrained tool calling), so assume tools are available. If its
            // model lacks a tool template the round simply returns prose and the
            // loop still terminates cleanly.
            SubagentRuntime::Relay(_) => true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn chat_stream<F: FnMut(ChatDelta)>(
        &self,
        model: &str,
        messages: Value,
        think: bool,
        tools: Option<&Value>,
        options: Option<&Value>,
        settings: &ModelLoadSettings,
        cancel: Arc<AtomicBool>,
        on_delta: F,
    ) -> anyhow::Result<ChatOutput> {
        match self {
            SubagentRuntime::Local(r) => {
                r.chat_stream(model, messages, think, tools, options, settings, cancel, on_delta)
                    .await
            }
            SubagentRuntime::Relay(relay) => {
                // The peer resolves its own model settings; nothing streams.
                let _ = (options, settings, on_delta);
                relay.chat_stream(model, messages, think, tools, cancel).await
            }
        }
    }
}

/// Run one sub-agent to completion and return its final text (the `run_agent`
/// tool result). Errors are returned as text so a failed sub-agent degrades to a
/// message the orchestrator can react to rather than aborting the parent turn.
///
/// `workspace` is the parent's (reused, still confined) and the sub-agent's
/// policy is forced to read-only regardless of the parent's. It borrows the
/// session's `runtime` + `settings` directly (not the whole `AppState`), so the
/// loop stays testable against a bare runtime. `emit` receives the
/// `subagent_started` / `subagent_result` UI events; the caller wraps them for
/// its transport (local Tauri event vs. cloud realtime), so this loop is shared
/// by both engines.
/// Run one sub-agent and return `(tool_result_text, ok)`. `ok` is false only
/// when the underlying loop errored (transport/model failure) — the dispatcher
/// uses it to fall back to local inference when a relayed sub-agent fails.
#[allow(clippy::too_many_arguments)]
pub async fn run_agent<E: Fn(Value)>(
    runtime: &SubagentRuntime<'_>,
    settings: &ModelLoadSettings,
    workspace: &Workspace,
    model: &str,
    agent: &AgentDef,
    task: &str,
    cancel: Arc<AtomicBool>,
    emit: E,
) -> (String, bool) {
    emit(json!({ "type": "subagent_started", "agent": agent.name, "task": task }));

    let result = run_loop(runtime, settings, workspace, model, agent, task, cancel).await;
    let ok = result.is_ok();
    let summary = match &result {
        Ok(text) if !text.trim().is_empty() => clamp(text.trim(), MAX_RESULT_CHARS),
        Ok(_) => "The sub-agent finished without producing a summary.".to_string(),
        Err(e) => format!("The {} sub-agent failed: {e}", agent.name),
    };

    emit(json!({ "type": "subagent_result", "agent": agent.name, "summary": summarize_line(&summary) }));
    // The orchestrator gets the full (clamped) summary; the UI event carries a
    // short one-liner for the trace row.
    (format!("Result from the {} sub-agent:\n\n{summary}", agent.name), ok)
}

#[allow(clippy::too_many_arguments)]
async fn run_loop(
    runtime: &SubagentRuntime<'_>,
    settings: &ModelLoadSettings,
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

    let native_tools = runtime.template_supports_tools(model).await;
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
            runtime
                .chat_stream(
                    model,
                    Value::Array(messages.clone()),
                    false,
                    tools_value.as_ref(),
                    options.as_ref(),
                    settings,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end run of a custom `.agents/reviewer.md` sub-agent against a live
    /// llama-server. Ignored by default (needs a running model); drive it with:
    ///
    ///   LOCALLMOS_LLAMACPP_MODELS_DIR=/home/jason/.locallmos/models \
    ///   SUBAGENT_TEST_MODEL='huggingface/unsloth/Qwen3.8-27B-GGUF/Qwen3.8-27B-Q5_K_M.gguf' \
    ///   cargo test --lib subagent::tests::reviewer_reviews_a_buggy_file -- --ignored --nocapture
    ///
    /// It reuses an already-healthy server serving the same GGUF (ensure_running
    /// checks /health first), so it will not fight the desktop app for the port.
    #[tokio::test]
    #[ignore = "requires a running llama-server + local model"]
    async fn reviewer_reviews_a_buggy_file() {
        let model = std::env::var("SUBAGENT_TEST_MODEL")
            .expect("set SUBAGENT_TEST_MODEL to a local gguf model id");

        // A scratch workspace with a custom reviewer agent and a file with an
        // off-by-one bug the reviewer should notice.
        let dir = std::env::temp_dir().join(format!("locallmos-reviewer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".agents")).unwrap();
        std::fs::write(
            dir.join(".agents").join("reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews a file for bugs and reports findings.\ntools: read_file, list_dir, search\n---\n\
You are a meticulous code reviewer. Read the file you are asked about, then report concrete bugs with the line and a one-line fix. If you find none, say so.",
        )
        .unwrap();
        std::fs::write(
            dir.join("sum.py"),
            "def sum_first_n(items, n):\n    total = 0\n    # BUG: range(n+1) reads one past the intended n elements\n    for i in range(n + 1):\n        total += items[i]\n    return total\n",
        )
        .unwrap();

        let workspace = Workspace::new(dir.to_str().unwrap()).unwrap();
        let agents = coding::discover_agents(&workspace);
        let reviewer = agents.iter().find(|a| a.name == "reviewer").expect("reviewer discovered");
        // The custom tools list was intersected down to read-only tools.
        assert_eq!(reviewer.tools, vec!["read_file", "list_dir", "search"]);

        let runtime = Runtime::from_kind(reqwest::Client::new(), "llamacpp");
        let settings = ModelLoadSettings::default();
        let cancel = Arc::new(AtomicBool::new(false));

        let (result, ok) = run_agent(
            &SubagentRuntime::Local(&runtime),
            &settings,
            &workspace,
            &model,
            reviewer,
            "Review sum.py for bugs and report what you find.",
            cancel,
            |ev| println!("EVENT: {ev}"),
        )
        .await;

        println!("\n===== SUB-AGENT RESULT =====\n{result}\n============================\n");
        assert!(ok, "sub-agent loop should not have errored");
        assert!(result.contains("reviewer sub-agent"));
        assert!(result.len() > 40, "expected a real summary, got: {result}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// End-to-end proof that the loaded model itself calls `create_agent` from a
    /// plain-English request, and that the saved agent is then discoverable. Same
    /// live-server requirements as the reviewer test; run with:
    ///
    ///   LOCALLMOS_LLAMACPP_MODELS_DIR=/home/jason/.locallmos/models \
    ///   SUBAGENT_TEST_MODEL='huggingface/unsloth/Qwen3.8-27B-GGUF/Qwen3.8-27B-Q5_K_M.gguf' \
    ///   cargo test --lib subagent::tests::model_creates_an_agent_on_request -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires a running llama-server + local model"]
    async fn model_creates_an_agent_on_request() {
        let model = std::env::var("SUBAGENT_TEST_MODEL")
            .expect("set SUBAGENT_TEST_MODEL to a local gguf model id");
        let dir = std::env::temp_dir().join(format!("locallmos-mkagent-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();

        let cx = CodingContext {
            workspace: Workspace::new(dir.to_str().unwrap()).unwrap(),
            policy: ApprovalPolicy::Auto, // no approval gate in the test harness
            mcp: McpAccess::disabled(),
        };
        let runtime = Runtime::from_kind(reqwest::Client::new(), "llamacpp");
        let settings = ModelLoadSettings::default();
        let cancel = Arc::new(AtomicBool::new(false));

        let tool_defs = coding::tool_defs_for(&cx, false, true);
        let tools_value = Value::Array(tool_defs);
        let mut messages = vec![
            json!({ "role": "system", "content": coding::system_prompt(
                &cx.workspace.root_str(), cx.policy, &cx.mcp, &coding::discover_agents(&cx.workspace),
            ) }),
            json!({ "role": "user", "content":
                "Create a reusable sub-agent named 'py-reviewer' that reviews Python files for bugs and reports concrete findings." }),
        ];

        // A few rounds: execute whatever tools the model calls until it saves the
        // agent (it may list_dir first). Then assert the agent exists.
        let mut created = false;
        for _ in 0..5 {
            let out = runtime
                .chat_stream(&model, Value::Array(messages.clone()), false,
                    Some(&tools_value), None, &settings, cancel.clone(), |_| {})
                .await
                .unwrap();
            if out.tool_calls.iter().any(|c| c.name == "create_agent") {
                created = true;
            }
            if out.tool_calls.is_empty() {
                println!("MODEL (no tools): {}", out.content);
                break;
            }
            let echoed: Vec<Value> = out.tool_calls.iter().map(|c| c.to_request_value()).collect();
            messages.push(json!({ "role": "assistant", "content": "", "tool_calls": echoed }));
            for call in &out.tool_calls {
                println!("TOOL CALL: {} {}", call.name, call.arguments);
                let run = coding::execute(&cx, None, &call.name, &call.arguments).await;
                messages.push(json!({ "role": "tool", "tool_name": call.name, "content": run.content }));
            }
            if created {
                break;
            }
        }

        assert!(created, "the model never called create_agent");
        let agents = coding::discover_agents(&cx.workspace);
        println!("DISCOVERED AGENTS: {:?}", agents.iter().map(|a| &a.name).collect::<Vec<_>>());
        let made = agents.iter().find(|a| a.name != "explore").expect("a custom agent was saved");
        // Whatever it saved must be read-only and have a usable prompt.
        let readonly = ["read_file", "list_dir", "search", "git"];
        assert!(made.tools.iter().all(|t| readonly.contains(&t.as_str())), "non-read-only tool saved: {:?}", made.tools);
        assert!(!made.system_prompt.trim().is_empty());
        println!("SAVED AGENT: {} — {}", made.name, made.description);

        std::fs::remove_dir_all(&dir).ok();
    }
}
