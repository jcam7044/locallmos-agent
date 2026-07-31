//! Agentic coding harness: locally-executed tools scoped to an attached
//! workspace, with a policy that decides which calls pause for approval.
//!
//! These tools run on the rig itself (execution='local'); the agent never
//! relays them to the hosted tool gateway. `chat.rs` routes local platform-tool
//! calls here, gating mutating ones through the cross-device approval flow.

mod prompt;
pub mod preview;
mod tools;
mod workspace;

pub use prompt::system_prompt;
pub use workspace::Workspace;

use crate::mcp;
use serde_json::Value;
use std::sync::Arc;

/// A coding turn's access to MCP tools: the frozen tool snapshot the model was
/// shown (for defs, the prompt, and token accounting) plus a handle to execute
/// one. Both are resolved from the same `McpManager` at context-construction
/// time, so the preflight token count and the turn itself see the same tools.
///
/// Kept on `CodingContext` — not `CodingHost` — because cloud-driven turns pass
/// `host: None`, and MCP must be reachable there too.
#[derive(Clone)]
pub struct McpAccess {
    pub snapshot: Arc<mcp::McpSnapshot>,
    pub manager: Option<Arc<mcp::McpManager>>,
}

impl McpAccess {
    /// No MCP tools (the default for sessions that haven't enabled it).
    // Used by the per-session gate added in phase 3.
    #[allow(dead_code)]
    pub fn disabled() -> Self {
        Self { snapshot: mcp::McpSnapshot::empty(), manager: None }
    }

    /// Freeze the manager's current snapshot for a turn.
    pub fn frozen(manager: Arc<mcp::McpManager>) -> Self {
        let snapshot = manager.snapshot();
        Self { snapshot, manager: Some(manager) }
    }
}

/// How mutating tools are handled. Mirrors `coding_workspaces.approval_policy`.
///
/// Two independent axes: whether mutating tools exist at all for the turn
/// (`allows_mutations`) and, if they do, whether each call pauses for approval
/// (`gates_mutations`). Previously `Plan` and `ApproveWrites` both merely gated,
/// making them behaviourally identical despite the UI offering them as separate
/// choices; `Plan` now genuinely cannot mutate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ApprovalPolicy {
    /// Inspection only — mutating tools are withheld and refused.
    ReadOnly,
    /// Like `ReadOnly`, but the prompt asks for an implementation plan.
    Plan,
    /// Reads run freely; writes + commands need approval (default).
    ApproveWrites,
    /// Nothing is gated — runs unattended within the workspace.
    Auto,
}

impl ApprovalPolicy {
    pub fn parse(s: &str) -> Self {
        match s {
            "auto" => Self::Auto,
            "plan" => Self::Plan,
            "read_only" => Self::ReadOnly,
            _ => Self::ApproveWrites,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Plan => "plan",
            Self::ApproveWrites => "approve_writes",
            Self::Auto => "auto",
        }
    }

    /// False for the inspection-only modes, which never mutate the workspace.
    fn allows_mutations(self) -> bool {
        !matches!(self, Self::ReadOnly | Self::Plan)
    }

    /// True when a permitted mutation still pauses for the user's approval.
    fn gates_mutations(self) -> bool {
        matches!(self, Self::ApproveWrites)
    }
}

/// Whether a built-in call would change the workspace. `git` depends on the
/// subcommand. Pure — MCP tools are classified by [`is_mutating`].
fn builtin_is_mutating(name: &str, args: &Value) -> bool {
    match name {
        "write_file" | "edit_file" | "run_command" | "dev_server_start" => true,
        "git" => !tools::git_is_readonly(args.get("args").and_then(Value::as_str).unwrap_or("")),
        _ => false,
    }
}

/// Whether a call (built-in or MCP) would change the workspace. MCP tools are
/// mutating by default — a third-party tool is only treated as read-only when the
/// user marked its server `Trusted` *and* the tool declared `readOnlyHint`, a
/// decision precomputed into `McpToolDef.mutating`. A call to a tool not in the
/// snapshot fails closed (mutating), so the approval gate still covers it.
fn is_mutating(cx: &CodingContext, name: &str, args: &Value) -> bool {
    if name.starts_with(mcp::MCP_PREFIX) {
        return cx.mcp.snapshot.find(name).map(|t| t.mutating).unwrap_or(true);
    }
    builtin_is_mutating(name, args)
}

/// Everything a coding turn needs to execute tools: the confined workspace and
/// the approval policy resolved from the session's workspace row.
pub struct CodingContext {
    pub workspace: Workspace,
    pub policy: ApprovalPolicy,
    pub mcp: McpAccess,
}

/// GUI-only services available to local desktop coding turns. Keeping this
/// explicit prevents preview tools from silently appearing in headless/cloud
/// execution paths.
#[derive(Clone)]
pub struct CodingHost {
    pub app: tauri::AppHandle,
    pub preview: std::sync::Arc<preview::PreviewManager>,
    pub session_id: String,
}

/// The result of running one coding tool.
pub struct ToolRun {
    /// Model-visible tool result text (fed back into the conversation).
    pub content: String,
    /// Short human label for the live stream / activity row.
    pub summary: String,
    /// A structured `file_edit` / `command` event to broadcast live (if any).
    pub event: Option<Value>,
    /// A persisted `tool_activity` row so the web can render the run after the
    /// fact, independent of the live stream.
    pub activity: Option<Value>,
}

/// True for the coding tool function names the agent executes locally. Any
/// `mcp__…` name is admitted here; whether that specific MCP tool is actually
/// available is resolved at execution time (an unknown one fails per-call rather
/// than aborting the turn).
pub fn is_known_tool(name: &str) -> bool {
    name.starts_with(mcp::MCP_PREFIX)
        || matches!(
            name,
            "read_file" | "list_dir" | "search" | "write_file" | "edit_file" | "run_command" | "git"
                | "dev_server_start" | "dev_server_logs" | "dev_server_stop"
                | "web_preview_open" | "web_preview_snapshot" | "web_preview_click"
                | "web_preview_fill" | "web_preview_press" | "web_preview_reload"
                | "web_preview_resize" | "web_preview_console" | "web_preview_close"
        )
}

/// The tools offered to the model for this turn. Inspection-only modes withhold
/// the mutating tools so the model doesn't plan around capabilities it lacks;
/// `execute` refuses them regardless. `git` stays available in every mode — its
/// read-only subcommands are useful, and mutating ones are refused per call.
///
/// `include_mcp` is set only when the runtime supports native tool calling.
/// Prompt-injection tool mode re-serializes every full JSON Schema into the
/// prompt each round, so third-party MCP schemas are withheld there — see
/// `local_coding::build_context`.
pub fn tool_defs_for(cx: &CodingContext, include_preview: bool, include_mcp: bool) -> Vec<Value> {
    let allows = cx.policy.allows_mutations();
    let mut all = tool_defs();
    if include_preview {
        all.extend(preview_tool_defs());
    }
    if !allows {
        all.retain(|d| {
            let name = d.pointer("/function/name").and_then(Value::as_str).unwrap_or("");
            !matches!(name, "write_file" | "edit_file" | "run_command" | "dev_server_start")
        });
    }
    if include_mcp {
        for tool in &cx.mcp.snapshot.tools {
            // Withhold mutating MCP tools under inspection-only modes, mirroring
            // the built-in filter above.
            if !allows && tool.mutating {
                continue;
            }
            all.push(tool.to_ollama_def());
        }
    }
    all
}

/// Desktop-only preview tools. These are appended only by the local GUI coding
/// path; cloud/headless turns never receive the schemas.
pub fn preview_tool_defs() -> Vec<Value> {
    fn def(name: &str, description: &str, params: Value) -> Value {
        serde_json::json!({
            "type": "function",
            "function": { "name": name, "description": description, "parameters": params },
        })
    }
    let s = |t: &str| serde_json::json!({ "type": t });
    vec![
        def("dev_server_start", "Start one persistent workspace development server and wait for its loopback URL to respond. Requires edit approval unless Auto mode is active.", serde_json::json!({"type":"object","properties":{"command":s("string"),"url":s("string"),"timeout_seconds":s("integer")},"required":["command","url"]})),
        def("dev_server_logs", "Read bounded stdout/stderr from the managed development server.", serde_json::json!({"type":"object","properties":{"clear":s("boolean")}})),
        def("dev_server_stop", "Stop the managed development server and its child process tree.", serde_json::json!({"type":"object","properties":{}})),
        def("web_preview_open", "Open or focus a visible desktop preview at an approved loopback URL. The first origin in a session requires one user approval.", serde_json::json!({"type":"object","properties":{"url":s("string"),"width":s("integer"),"height":s("integer")},"required":["url"]})),
        def("web_preview_snapshot", "Inspect the rendered page. Returns URL, title, visible text, and interactive elements with short refs such as e1. Take a new snapshot after navigation or major UI changes.", serde_json::json!({"type":"object","properties":{"selector":s("string")}})),
        def("web_preview_click", "Click an element ref from the latest preview snapshot.", serde_json::json!({"type":"object","properties":{"ref":s("string")},"required":["ref"]})),
        def("web_preview_fill", "Replace the value of an input/textarea/select ref and dispatch input/change events; optionally submit its form.", serde_json::json!({"type":"object","properties":{"ref":s("string"),"text":s("string"),"submit":s("boolean")},"required":["ref","text"]})),
        def("web_preview_press", "Dispatch a keyboard key at an element ref from the latest snapshot.", serde_json::json!({"type":"object","properties":{"ref":s("string"),"key":s("string")},"required":["ref","key"]})),
        def("web_preview_reload", "Reload the current preview page.", serde_json::json!({"type":"object","properties":{}})),
        def("web_preview_resize", "Resize the preview viewport in logical pixels.", serde_json::json!({"type":"object","properties":{"width":s("integer"),"height":s("integer")},"required":["width","height"]})),
        def("web_preview_console", "Read captured console messages, page errors, and unhandled promise rejections.", serde_json::json!({"type":"object","properties":{"clear":s("boolean")}})),
        def("web_preview_close", "Close the preview and stop its managed development server.", serde_json::json!({"type":"object","properties":{}})),
    ]
}

/// Ollama-style tool schemas for all coding tools, for the local (offline)
/// engine which has no server-authored snapshot to draw from. Mirrors the
/// `tool_catalog` rows in supabase migration 0036.
pub fn tool_defs() -> Vec<Value> {
    fn def(name: &str, description: &str, params: Value) -> Value {
        serde_json::json!({
            "type": "function",
            "function": { "name": name, "description": description, "parameters": params },
        })
    }
    let s = |t: &str| serde_json::json!({ "type": t });
    vec![
        def(
            "read_file",
            "Read a UTF-8 text file inside the workspace. Optionally limit to a line range.",
            serde_json::json!({"type":"object","properties":{"path":s("string"),"start_line":s("integer"),"end_line":s("integer")},"required":["path"]}),
        ),
        def(
            "list_dir",
            "List files and subdirectories inside the workspace.",
            serde_json::json!({"type":"object","properties":{"path":s("string")}}),
        ),
        def(
            "search",
            "Regex-search file contents in the workspace; returns matching lines with paths.",
            serde_json::json!({"type":"object","properties":{"query":s("string"),"path":s("string"),"max_results":s("integer")},"required":["query"]}),
        ),
        def(
            "write_file",
            "Create or overwrite a file in the workspace with full content. Requires approval.",
            serde_json::json!({"type":"object","properties":{"path":s("string"),"content":s("string")},"required":["path","content"]}),
        ),
        def(
            "edit_file",
            "Replace an exact substring in a workspace file. old_string must be unique unless replace_all. Requires approval.",
            serde_json::json!({"type":"object","properties":{"path":s("string"),"old_string":s("string"),"new_string":s("string"),"replace_all":s("boolean")},"required":["path","old_string","new_string"]}),
        ),
        def(
            "run_command",
            "Run a shell command with the working directory pinned to the workspace root. Requires approval.",
            serde_json::json!({"type":"object","properties":{"command":s("string")},"required":["command"]}),
        ),
        def(
            "git",
            "Run a git subcommand in the workspace. Read-only subcommands run without approval.",
            serde_json::json!({"type":"object","properties":{"args":s("string")},"required":["args"]}),
        ),
    ]
}

/// If this call must be approved before it runs, return the human-readable
/// preview (a diff or the command) to show the approver. `None` means run now.
pub fn approval_preview(cx: &CodingContext, name: &str, args: &Value) -> Option<String> {
    if !cx.policy.gates_mutations() || !is_mutating(cx, name, args) {
        return None;
    }
    if name.starts_with(mcp::MCP_PREFIX) {
        return Some(mcp_approval_preview(cx, name, args));
    }
    match name {
        "write_file" | "edit_file" => Some(
            tools::change_preview(cx, name, args)
                .unwrap_or_else(|e| format!("{name}: {e}")),
        ),
        "run_command" => {
            Some(format!("$ {}", args.get("command").and_then(Value::as_str).unwrap_or("")))
        }
        "dev_server_start" => Some(format!(
            "$ {}\nPreview URL: {}",
            args.get("command").and_then(Value::as_str).unwrap_or(""),
            args.get("url").and_then(Value::as_str).unwrap_or("")
        )),
        "git" => Some(format!("$ git {}", args.get("args").and_then(Value::as_str).unwrap_or(""))),
        _ => None,
    }
}

/// Human-readable approval preview for an MCP call: the owning server and tool
/// plus its (pretty-printed, bounded) arguments.
fn mcp_approval_preview(cx: &CodingContext, name: &str, args: &Value) -> String {
    let (server, tool) = cx
        .mcp
        .snapshot
        .find(name)
        .map(|t| (t.server_id.clone(), t.tool_name.clone()))
        .unwrap_or_else(|| (String::from("?"), name.to_string()));
    let mut rendered = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
    const MAX: usize = 2_000;
    if rendered.len() > MAX {
        let mut end = MAX;
        while !rendered.is_char_boundary(end) {
            end -= 1;
        }
        rendered.truncate(end);
        rendered.push_str("\n…[truncated]");
    }
    format!("MCP {server} · {tool}\n{rendered}")
}

/// Execute a coding tool by name. Errors are returned as tool content so a
/// single failure does not abort the turn (mirrors the built-in tool path).
pub async fn execute(cx: &CodingContext, host: Option<&CodingHost>, name: &str, args: &Value) -> ToolRun {
    // Withholding the schemas is not enforcement — a model can still emit a call
    // for a tool it was never offered, and in prompt-injection tool mode it only
    // ever sees a text manifest. Refuse here, the single choke point.
    if !cx.policy.allows_mutations() && is_mutating(cx, name, args) {
        let mode = if cx.policy == ApprovalPolicy::Plan { "plan" } else { "read-only" };
        return ToolRun {
            content: format!(
                "{name} is unavailable: this session is in {mode} mode and cannot modify the \
workspace. Describe the change instead — the user can switch modes to apply it."
            ),
            summary: format!("blocked ({mode})"),
            event: None,
            activity: Some(serde_json::json!({
                "name": name, "provider": "coding", "status": "blocked",
                "summary": format!("blocked ({mode})"), "citations": [],
            })),
        };
    }
    match tools::run(cx, host, name, args).await {
        Ok(run) => run,
        Err(e) => ToolRun {
            content: format!("{name} failed: {e}"),
            summary: "error".into(),
            event: None,
            activity: Some(serde_json::json!({
                "name": name, "provider": "coding", "status": "failed",
                "summary": "error", "citations": [],
            })),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names(defs: &[Value]) -> Vec<String> {
        defs.iter()
            .filter_map(|d| d.pointer("/function/name").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    fn mcp_tool(server: &str, tool: &str, mutating: bool) -> mcp::McpToolDef {
        mcp::McpToolDef {
            server_id: server.into(),
            tool_name: tool.into(),
            qualified: mcp::qualified_name(server, tool),
            description: String::new(),
            parameters: json!({ "type": "object" }),
            read_only_hint: !mutating,
            mutating,
        }
    }

    /// A context with a synthetic MCP snapshot and no live manager (gating and
    /// def-assembly are pure; execution is covered by the mcp module's tests).
    fn ctx(policy: ApprovalPolicy, mcp_tools: Vec<mcp::McpToolDef>) -> CodingContext {
        let snapshot = Arc::new(mcp::McpSnapshot { tools: mcp_tools, truncated: 0 });
        CodingContext {
            workspace: Workspace::new(std::env::temp_dir().to_str().unwrap()).unwrap(),
            policy,
            mcp: McpAccess { snapshot, manager: None },
        }
    }

    #[test]
    fn policy_strings_round_trip() {
        for p in [
            ApprovalPolicy::ReadOnly,
            ApprovalPolicy::Plan,
            ApprovalPolicy::ApproveWrites,
            ApprovalPolicy::Auto,
        ] {
            assert_eq!(ApprovalPolicy::parse(p.as_str()), p);
        }
        // Unknown input falls back to the gated default, never to Auto.
        assert_eq!(ApprovalPolicy::parse("nonsense"), ApprovalPolicy::ApproveWrites);
    }

    /// Regression: Plan and ApproveWrites were previously identical — both only
    /// gated mutations — so "Plan" silently permitted edits once approved.
    #[test]
    fn plan_is_not_merely_approve_writes() {
        assert!(!ApprovalPolicy::Plan.allows_mutations());
        assert!(ApprovalPolicy::ApproveWrites.allows_mutations());
        assert!(!ApprovalPolicy::ReadOnly.allows_mutations());
        assert!(ApprovalPolicy::Auto.allows_mutations());

        // Only ApproveWrites pauses; Auto runs freely, the other two never run.
        assert!(ApprovalPolicy::ApproveWrites.gates_mutations());
        assert!(!ApprovalPolicy::Auto.gates_mutations());
    }

    #[test]
    fn inspection_modes_withhold_mutating_tools() {
        for p in [ApprovalPolicy::ReadOnly, ApprovalPolicy::Plan] {
            let cx = ctx(p, vec![]);
            let offered = names(&tool_defs_for(&cx, true, false));
            for withheld in ["write_file", "edit_file", "run_command", "dev_server_start"] {
                assert!(!offered.contains(&withheld.to_string()), "{p:?} offered {withheld}");
            }
            // Reads and git stay available — git's mutating subcommands are
            // refused per call rather than by withholding the whole tool.
            for kept in ["read_file", "list_dir", "search", "git"] {
                assert!(offered.contains(&kept.to_string()), "{p:?} withheld {kept}");
            }
        }
        let cx = ctx(ApprovalPolicy::Auto, vec![]);
        assert_eq!(names(&tool_defs_for(&cx, false, false)).len(), tool_defs().len());
    }

    #[test]
    fn mcp_tools_offered_by_policy_and_mutation() {
        let tools = vec![mcp_tool("db", "read_query", false), mcp_tool("db", "write_query", true)];

        // ApproveWrites/Auto see both MCP tools when include_mcp is set.
        let cx = ctx(ApprovalPolicy::ApproveWrites, tools.clone());
        let offered = names(&tool_defs_for(&cx, false, true));
        assert!(offered.contains(&"mcp__db__read_query".to_string()));
        assert!(offered.contains(&"mcp__db__write_query".to_string()));

        // include_mcp=false (prompt-injection tool mode) withholds all MCP tools.
        let offered = names(&tool_defs_for(&cx, false, false));
        assert!(!offered.iter().any(|n| n.starts_with(mcp::MCP_PREFIX)));

        // Inspection-only modes withhold the mutating MCP tool but keep read-only.
        for p in [ApprovalPolicy::ReadOnly, ApprovalPolicy::Plan] {
            let cx = ctx(p, tools.clone());
            let offered = names(&tool_defs_for(&cx, false, true));
            assert!(offered.contains(&"mcp__db__read_query".to_string()), "{p:?} withheld read");
            assert!(!offered.contains(&"mcp__db__write_query".to_string()), "{p:?} offered write");
        }
    }

    #[test]
    fn git_mutation_depends_on_subcommand() {
        assert!(!builtin_is_mutating("git", &json!({ "args": "status" })));
        assert!(builtin_is_mutating("git", &json!({ "args": "commit -m x" })));
        assert!(builtin_is_mutating("write_file", &json!({})));
        assert!(!builtin_is_mutating("read_file", &json!({})));
        assert!(builtin_is_mutating("dev_server_start", &json!({})));
        assert!(!builtin_is_mutating("web_preview_click", &json!({})));
    }

    #[test]
    fn mcp_tool_mutation_comes_from_snapshot_and_fails_closed() {
        let cx = ctx(
            ApprovalPolicy::ApproveWrites,
            vec![mcp_tool("db", "read_query", false), mcp_tool("db", "write_query", true)],
        );
        // Classification is driven by the precomputed snapshot flag…
        assert!(!is_mutating(&cx, "mcp__db__read_query", &json!({})));
        assert!(is_mutating(&cx, "mcp__db__write_query", &json!({})));
        // …and a tool not in the snapshot is treated as mutating (fail closed).
        assert!(is_mutating(&cx, "mcp__db__unknown", &json!({})));
    }

    #[test]
    fn mcp_calls_are_known_and_get_a_preview() {
        assert!(is_known_tool("mcp__db__anything"));
        let cx = ctx(
            ApprovalPolicy::ApproveWrites,
            // A read-only-hinted tool on an untrusted server: mutating=true.
            vec![mcp_tool("db", "write_query", true), mcp_tool("db", "read_query", false)],
        );
        let preview = approval_preview(&cx, "mcp__db__write_query", &json!({ "sql": "delete" }));
        let preview = preview.expect("mutating MCP call should require approval");
        assert!(preview.contains("MCP db · write_query"));
        assert!(preview.contains("sql"));
        // A non-mutating (trusted read-only) MCP tool runs without a pause.
        assert!(approval_preview(&cx, "mcp__db__read_query", &json!({})).is_none());
    }
}
