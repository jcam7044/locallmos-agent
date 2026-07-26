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

/// Whether a call would change the workspace. `git` depends on the subcommand.
fn is_mutating(name: &str, args: &Value) -> bool {
    match name {
        "write_file" | "edit_file" | "run_command" => true,
        "git" => !tools::git_is_readonly(args.get("args").and_then(Value::as_str).unwrap_or("")),
        _ => false,
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

/// The tools offered to the model under `policy`. Inspection-only modes withhold
/// the mutating tools so the model doesn't plan around capabilities it lacks;
/// `execute` refuses them regardless. `git` stays available in every mode — its
/// read-only subcommands are useful, and mutating ones are refused per call.
pub fn tool_defs_for(policy: ApprovalPolicy) -> Vec<Value> {
    let all = tool_defs();
    if policy.allows_mutations() {
        return all;
    }
    all.into_iter()
        .filter(|d| {
            let name = d.pointer("/function/name").and_then(Value::as_str).unwrap_or("");
            !matches!(name, "write_file" | "edit_file" | "run_command")
        })
        .collect()
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
    if !cx.policy.gates_mutations() || !is_mutating(name, args) {
        return None;
    }
    match name {
        "write_file" | "edit_file" => Some(
            tools::change_preview(cx, name, args)
                .unwrap_or_else(|e| format!("{name}: {e}")),
        ),
        "run_command" => {
            Some(format!("$ {}", args.get("command").and_then(Value::as_str).unwrap_or("")))
        }
        "git" => Some(format!("$ git {}", args.get("args").and_then(Value::as_str).unwrap_or(""))),
        _ => None,
    }
}

/// Execute a coding tool by name. Errors are returned as tool content so a
/// single failure does not abort the turn (mirrors the built-in tool path).
pub async fn execute(cx: &CodingContext, name: &str, args: &Value) -> ToolRun {
    // Withholding the schemas is not enforcement — a model can still emit a call
    // for a tool it was never offered, and in prompt-injection tool mode it only
    // ever sees a text manifest. Refuse here, the single choke point.
    if !cx.policy.allows_mutations() && is_mutating(name, args) {
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
            let offered = names(&tool_defs_for(p));
            for withheld in ["write_file", "edit_file", "run_command"] {
                assert!(!offered.contains(&withheld.to_string()), "{p:?} offered {withheld}");
            }
            // Reads and git stay available — git's mutating subcommands are
            // refused per call rather than by withholding the whole tool.
            for kept in ["read_file", "list_dir", "search", "git"] {
                assert!(offered.contains(&kept.to_string()), "{p:?} withheld {kept}");
            }
        }
        assert_eq!(names(&tool_defs_for(ApprovalPolicy::Auto)).len(), tool_defs().len());
    }

    #[test]
    fn git_mutation_depends_on_subcommand() {
        assert!(!is_mutating("git", &json!({ "args": "status" })));
        assert!(is_mutating("git", &json!({ "args": "commit -m x" })));
        assert!(is_mutating("write_file", &json!({})));
        assert!(!is_mutating("read_file", &json!({})));
    }
}
