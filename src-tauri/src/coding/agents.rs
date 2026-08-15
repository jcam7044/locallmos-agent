//! Sub-agent definitions: the built-in read-only `explore` agent plus any
//! user-authored agents discovered as markdown files. A sub-agent runs in its
//! own isolated context (see `crate::subagent`) with a read-only tool set, and
//! reports a compact summary back to the main agent — keeping broad, token-hungry
//! exploration out of the orchestrator's context window.
//!
//! Custom agent files live at `<workspace>/.agents/*.md` (project-local) and
//! `<config_dir>/agents/*.md` (global). Each may carry a minimal `--- … ---`
//! frontmatter (`name`, `description`, `tools`); the markdown body is the
//! agent's system prompt. Parsing is deliberately dependency-free and forgiving:
//! a malformed file is skipped rather than failing discovery.

use super::Workspace;

/// The only tools a sub-agent may use in this version. Sub-agents cannot mutate
/// the workspace, so they never need the approval gate — which is what keeps the
/// nested loop simple. `git` is included for read-only subcommands (mutating
/// ones are refused per-call by the executor regardless).
pub const READONLY_TOOLS: &[&str] = &["read_file", "list_dir", "search", "git"];

/// The built-in agent's reserved name; a custom file cannot shadow it.
pub const EXPLORE_AGENT: &str = "explore";

/// A resolved sub-agent the orchestrator can dispatch with `run_agent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// The sub-agent's system prompt (its role/instructions).
    pub system_prompt: String,
    /// Read-only tools this agent may call (always a subset of `READONLY_TOOLS`).
    pub tools: Vec<String>,
}

impl AgentDef {
    fn built_in_explore() -> Self {
        Self {
            name: EXPLORE_AGENT.to_string(),
            description:
                "Read-only codebase explorer. Give it a search-style task; it fans out across \
files with read_file/list_dir/search and returns a compact summary with the relevant \
file paths — not full file dumps."
                    .to_string(),
            system_prompt:
                "You are a read-only exploration sub-agent working inside a single project \
directory. You have read_file, list_dir, search, and read-only git.\n\n\
Find what the task asks for by searching and reading the relevant files, then STOP and \
report. Your answer must be a concise, self-contained summary the calling agent can act on:\n\
- the concrete file paths (with line numbers where useful) that matter,\n\
- a one-line explanation of what each is/does,\n\
- any directly relevant snippets, kept short.\n\n\
Do not dump whole files, do not speculate beyond what you read, and do not attempt to make \
changes — you cannot. Be thorough in searching but terse in reporting."
                    .to_string(),
            tools: READONLY_TOOLS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// All sub-agents available for a session: the built-in `explore` first, then
/// project-local (`<workspace>/.agents`) and global (`<config_dir>/agents`)
/// custom agents. Names are unique — the built-in and earlier sources win, so a
/// project agent shadows a global one of the same name but nothing shadows
/// `explore`.
pub fn discover_agents(ws: &Workspace) -> Vec<AgentDef> {
    let mut agents = vec![AgentDef::built_in_explore()];

    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(local) = ws.resolve(".agents") {
        dirs.push(local);
    }
    if let Ok(config) = crate::config::config_dir() {
        dirs.push(config.join("agents"));
    }

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .collect();
        files.sort(); // stable order across runs
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("agent");
            if let Some(agent) = parse_agent(stem, &text) {
                if !agents.iter().any(|a| a.name == agent.name) {
                    agents.push(agent);
                }
            }
        }
    }
    agents
}

/// Parse one agent file. Returns `None` for content that can't yield a usable
/// prompt (e.g. empty body). `default_name` is the filename stem, used when the
/// frontmatter omits `name`.
fn parse_agent(default_name: &str, text: &str) -> Option<AgentDef> {
    let (front, body) = split_frontmatter(text);

    let name = front
        .as_ref()
        .and_then(|f| field(f, "name"))
        .unwrap_or_else(|| default_name.to_string());
    // Never let a custom file claim the reserved built-in name.
    if name == EXPLORE_AGENT {
        return None;
    }

    let body = body.trim();
    if body.is_empty() {
        return None;
    }

    let description = front
        .as_ref()
        .and_then(|f| field(f, "description"))
        .unwrap_or_else(|| {
            body.lines()
                .next()
                .unwrap_or("Custom sub-agent")
                .trim_start_matches('#')
                .trim()
                .chars()
                .take(200)
                .collect()
        });

    // A custom agent's declared tools are intersected with the read-only set;
    // an empty/invalid list falls back to all read-only tools.
    let tools = front
        .as_ref()
        .and_then(|f| field(f, "tools"))
        .map(|raw| {
            raw.split([',', ' ', '\t'])
                .map(|t| t.trim())
                .filter(|t| READONLY_TOOLS.contains(t))
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| READONLY_TOOLS.iter().map(|s| s.to_string()).collect());

    Some(AgentDef {
        name,
        description,
        system_prompt: body.to_string(),
        tools,
    })
}

/// Split a leading `--- … ---` YAML-ish frontmatter block from the body. Returns
/// `(Some(frontmatter), body)` when present, else `(None, whole_text)`.
fn split_frontmatter(text: &str) -> (Option<String>, String) {
    let trimmed = text.trim_start_matches('\u{feff}');
    let rest = match trimmed.strip_prefix("---\n").or_else(|| trimmed.strip_prefix("---\r\n")) {
        Some(r) => r,
        None => return (None, text.to_string()),
    };
    // Find the closing delimiter line.
    for delim in ["\n---\n", "\n---\r\n", "\r\n---\r\n"] {
        if let Some(end) = rest.find(delim) {
            let front = rest[..end].to_string();
            let body = rest[end + delim.len()..].to_string();
            return (Some(front), body);
        }
    }
    // Frontmatter opened but never closed — treat the whole thing as body.
    (None, text.to_string())
}

/// Read a `key: value` field from a frontmatter block (first match wins).
fn field(front: &str, key: &str) -> Option<String> {
    for line in front.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case(key) {
                let v = v.trim().trim_matches(['"', '\'']).trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_explore_is_read_only() {
        let e = AgentDef::built_in_explore();
        assert_eq!(e.name, EXPLORE_AGENT);
        assert!(!e.tools.contains(&"write_file".to_string()));
        assert!(e.tools.contains(&"search".to_string()));
    }

    #[test]
    fn parses_frontmatter_and_intersects_tools() {
        let file = "---\nname: reviewer\ndescription: Reviews diffs for bugs\ntools: read_file, write_file, search\n---\nYou are a careful code reviewer.";
        let a = parse_agent("fallback", file).unwrap();
        assert_eq!(a.name, "reviewer");
        assert_eq!(a.description, "Reviews diffs for bugs");
        // write_file is dropped (not read-only); read_file + search survive.
        assert_eq!(a.tools, vec!["read_file".to_string(), "search".to_string()]);
        assert!(a.system_prompt.contains("careful code reviewer"));
    }

    #[test]
    fn no_frontmatter_uses_stem_and_first_line() {
        let a = parse_agent("planner", "# Planner\nBreak work into steps.").unwrap();
        assert_eq!(a.name, "planner");
        assert_eq!(a.description, "Planner");
        // No tools declared → full read-only set.
        assert_eq!(a.tools.len(), READONLY_TOOLS.len());
    }

    #[test]
    fn empty_body_and_reserved_name_are_rejected() {
        assert!(parse_agent("x", "---\nname: y\n---\n   ").is_none());
        assert!(parse_agent("explore", "some body").is_none());
        assert!(parse_agent("x", "---\nname: explore\n---\nbody").is_none());
    }

    #[test]
    fn invalid_tools_fall_back_to_all_readonly() {
        let a = parse_agent("x", "---\ntools: nonsense, danger\n---\nbody").unwrap();
        assert_eq!(a.tools.len(), READONLY_TOOLS.len());
    }

    #[test]
    fn discover_includes_builtin_and_project_agents() {
        let dir = std::env::temp_dir().join(format!("locallmos-agents-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".agents")).unwrap();
        std::fs::write(
            dir.join(".agents").join("reviewer.md"),
            "---\nname: reviewer\ndescription: d\n---\nReview things.",
        )
        .unwrap();
        let ws = Workspace::new(dir.to_str().unwrap()).unwrap();
        let agents = discover_agents(&ws);
        assert!(agents.iter().any(|a| a.name == EXPLORE_AGENT));
        assert!(agents.iter().any(|a| a.name == "reviewer"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
