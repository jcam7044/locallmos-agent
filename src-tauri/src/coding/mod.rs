//! Agentic coding harness: locally-executed tools scoped to an attached
//! workspace, with a policy that decides which calls pause for approval.
//!
//! These tools run on the rig itself (execution='local'); the agent never
//! relays them to the hosted tool gateway. `chat.rs` routes local platform-tool
//! calls here, gating mutating ones through the cross-device approval flow.

mod prompt;
mod tools;
mod workspace;

pub use prompt::system_prompt;
pub use workspace::Workspace;

use serde_json::Value;

/// How mutating tools are gated. Mirrors `coding_workspaces.approval_policy`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ApprovalPolicy {
    /// Every mutating tool is gated (read-only session until approved).
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
            _ => Self::ApproveWrites,
        }
    }

    fn gates_mutations(self) -> bool {
        !matches!(self, Self::Auto)
    }
}

/// Everything a coding turn needs to execute tools: the confined workspace and
/// the approval policy resolved from the session's workspace row.
pub struct CodingContext {
    pub workspace: Workspace,
    pub policy: ApprovalPolicy,
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

/// True for the coding tool function names the agent executes locally.
pub fn is_known_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "list_dir" | "search" | "write_file" | "edit_file" | "run_command" | "git"
    )
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
    if !cx.policy.gates_mutations() {
        return None;
    }
    match name {
        "read_file" | "list_dir" | "search" => None,
        "write_file" | "edit_file" => Some(
            tools::change_preview(cx, name, args)
                .unwrap_or_else(|e| format!("{name}: {e}")),
        ),
        "run_command" => {
            Some(format!("$ {}", args.get("command").and_then(Value::as_str).unwrap_or("")))
        }
        "git" => {
            let a = args.get("args").and_then(Value::as_str).unwrap_or("");
            if tools::git_is_readonly(a) {
                None
            } else {
                Some(format!("$ git {a}"))
            }
        }
        _ => None,
    }
}

/// Execute a coding tool by name. Errors are returned as tool content so a
/// single failure does not abort the turn (mirrors the built-in tool path).
pub async fn execute(cx: &CodingContext, name: &str, args: &Value) -> ToolRun {
    match tools::run(cx, name, args).await {
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
