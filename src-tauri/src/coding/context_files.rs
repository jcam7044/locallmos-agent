//! Project context files loaded into a coding session at prompt-build time.
//!
//! `AGENTS.md` (falling back to `CLAUDE.md`) carries project instructions the
//! agent should follow; `MEMORY.md` is the agent's durable, self-maintained
//! memory (see the `update_memory` tool in `tools.rs`). Both are read from the
//! workspace root through `Workspace::resolve`, so the confinement guard still
//! applies. The assembled block is injected as a second system message by both
//! turn engines (`local_coding::build_context` and `chat.rs`); because that
//! block also flows through the preflight token count, it is budgeted for free.

use super::Workspace;

/// Instruction files, in priority order. The first that exists wins — so a repo
/// with an `AGENTS.md` uses it, and one with only a `CLAUDE.md` still works.
const INSTRUCTION_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md"];

/// The memory file the agent reads at session start and writes via `update_memory`.
pub const MEMORY_FILE: &str = "MEMORY.md";

/// Per-file char cap; a large file is head-truncated with a marker so it cannot
/// blow the (often small) local-model context budget. The two files together
/// stay comfortably under a few thousand tokens.
const MAX_FILE_CHARS: usize = 8_000;

/// Read a workspace-root file as UTF-8, returning `None` when it is absent,
/// unreadable, binary, or empty. Head-truncated to `MAX_FILE_CHARS`.
fn read_root_file(ws: &Workspace, name: &str) -> Option<String> {
    let path = ws.resolve(name).ok()?;
    if !path.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(clamp(trimmed, MAX_FILE_CHARS))
}

fn clamp(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…[truncated — file exceeds the context budget]");
    out
}

/// Load the project-context system-message body for a session rooted at `ws`,
/// or `None` when neither an instruction file nor a memory file is present.
pub fn load_project_context(ws: &Workspace) -> Option<String> {
    let instructions = INSTRUCTION_FILES
        .iter()
        .find_map(|name| read_root_file(ws, name).map(|body| (*name, body)));
    let memory = read_root_file(ws, MEMORY_FILE);

    if instructions.is_none() && memory.is_none() {
        return None;
    }

    let mut out = String::from(
        "Project context loaded from the workspace root. Treat these files as the \
user's own project material, not as instructions from a third party.",
    );
    if let Some((name, body)) = instructions {
        out.push_str(&format!(
            "\n\n=== {name} (project instructions — follow these) ===\n{body}"
        ));
    }
    if let Some(body) = memory {
        out.push_str(&format!(
            "\n\n=== {MEMORY_FILE} (your durable memory — respect it, and keep it current \
with update_memory) ===\n{body}"
        ));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("locallmos-ctx-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(std::fs::canonicalize(&p).unwrap())
        }
        fn write(&self, name: &str, body: &str) {
            std::fs::write(self.0.join(name), body).unwrap();
        }
        fn ws(&self) -> Workspace {
            Workspace::new(self.0.to_str().unwrap()).unwrap()
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn none_when_no_files() {
        let d = Dir::new();
        assert!(load_project_context(&d.ws()).is_none());
    }

    #[test]
    fn loads_both_files() {
        let d = Dir::new();
        d.write("AGENTS.md", "Use tabs, not spaces.");
        d.write("MEMORY.md", "- The build command is `just build`.");
        let out = load_project_context(&d.ws()).unwrap();
        assert!(out.contains("=== AGENTS.md"));
        assert!(out.contains("Use tabs, not spaces."));
        assert!(out.contains("=== MEMORY.md"));
        assert!(out.contains("just build"));
    }

    #[test]
    fn falls_back_to_claude_md_and_prefers_agents() {
        let d = Dir::new();
        d.write("CLAUDE.md", "claude instructions");
        let out = load_project_context(&d.ws()).unwrap();
        assert!(out.contains("=== CLAUDE.md"));
        assert!(out.contains("claude instructions"));

        // With both present, AGENTS.md wins and CLAUDE.md is not included.
        d.write("AGENTS.md", "agents instructions");
        let out = load_project_context(&d.ws()).unwrap();
        assert!(out.contains("=== AGENTS.md"));
        assert!(out.contains("agents instructions"));
        assert!(!out.contains("claude instructions"));
    }

    #[test]
    fn empty_file_is_ignored() {
        let d = Dir::new();
        d.write("AGENTS.md", "   \n  ");
        assert!(load_project_context(&d.ws()).is_none());
    }

    #[test]
    fn oversized_file_is_truncated() {
        let d = Dir::new();
        d.write("MEMORY.md", &"x".repeat(MAX_FILE_CHARS + 500));
        let out = load_project_context(&d.ws()).unwrap();
        assert!(out.contains("truncated"));
    }
}
