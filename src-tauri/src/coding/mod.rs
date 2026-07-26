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
