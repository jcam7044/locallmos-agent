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
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// The only tools a sub-agent may use in this version. Sub-agents cannot mutate
/// the workspace, so they never need the approval gate — which is what keeps the
/// nested loop simple. `git` is included for read-only subcommands (mutating
/// ones are refused per-call by the executor regardless).
pub const READONLY_TOOLS: &[&str] = &["read_file", "list_dir", "search", "git"];

/// The built-in agent's reserved name; a custom file cannot shadow it.
pub const EXPLORE_AGENT: &str = "explore";

/// Where a custom agent file lives. `Project` travels with the repo (shareable
/// via git); `Global` applies to every session on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentScope {
    Project,
    Global,
}

impl AgentScope {
    pub fn parse(s: &str) -> Self {
        if s == "global" { Self::Global } else { Self::Project }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Global => "global",
        }
    }
    /// The directory this scope's agent files live in. `Project` is confined to
    /// the workspace root; `Global` is the app config dir (a deliberate, bounded
    /// write outside any workspace — app config, not user project files).
    pub fn dir(self, workspace_root: &Path) -> Result<PathBuf> {
        match self {
            Self::Project => Ok(workspace_root.join(".agents")),
            Self::Global => Ok(crate::config::config_dir()?.join("agents")),
        }
    }
}

/// A resolved sub-agent the orchestrator can dispatch with `run_agent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// The sub-agent's system prompt (its role/instructions).
    pub system_prompt: String,
    /// Read-only tools this agent may call (always a subset of `READONLY_TOOLS`).
    pub tools: Vec<String>,
    /// Optional per-agent exploration round budget (frontmatter `max_rounds`),
    /// already clamped into the sub-agent bounds. `None` falls back to the env
    /// default. See [`crate::subagent`].
    pub max_rounds: Option<usize>,
}

/// Clamp a requested round budget into the sub-agent's hard bounds, so a stored
/// or model-supplied value can never remove the safety backstop.
pub fn clamp_rounds(n: usize) -> usize {
    n.clamp(crate::subagent::SUBAGENT_ROUNDS_MIN, crate::subagent::SUBAGENT_ROUNDS_MAX)
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
            max_rounds: None,
        }
    }
}

/// A discovered agent with its provenance, for the management UI.
#[derive(Clone, Debug)]
pub struct AgentInfo {
    pub def: AgentDef,
    /// "builtin" | "project" | "global".
    pub scope: String,
    /// Built-in agents can't be edited or deleted; file-based ones can.
    pub editable: bool,
}

/// Parse every `*.md` in `dir` into agents, paired with their path, in a stable
/// order. Missing dir or unreadable/malformed files are skipped.
fn scan_agent_dir(dir: &Path) -> Vec<(AgentDef, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    files.sort(); // stable order across runs
    files
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("agent");
            parse_agent(stem, &text).map(|def| (def, path))
        })
        .collect()
}

/// All sub-agents available for a session, with provenance: the built-in
/// `explore` first, then project-local (`<workspace>/.agents`) and global
/// (`<config_dir>/agents`) custom agents. Names are unique — the built-in and
/// earlier sources win, so a project agent shadows a global one of the same
/// name but nothing shadows `explore`.
pub fn list_agents(ws: &Workspace) -> Vec<AgentInfo> {
    let mut out = vec![AgentInfo {
        def: AgentDef::built_in_explore(),
        scope: "builtin".to_string(),
        editable: false,
    }];
    let project = ws.root().join(".agents");
    let global = AgentScope::Global.dir(ws.root()).ok();
    let sources = std::iter::once(("project", project))
        .chain(global.map(|g| ("global", g)));
    for (scope, dir) in sources {
        for (def, _path) in scan_agent_dir(&dir) {
            if !out.iter().any(|a| a.def.name == def.name) {
                out.push(AgentInfo { def, scope: scope.to_string(), editable: true });
            }
        }
    }
    out
}

/// The runtime view: just the dispatchable [`AgentDef`]s, same precedence as
/// [`list_agents`]. This is what the turn engine and tool-def builder use.
pub fn discover_agents(ws: &Workspace) -> Vec<AgentDef> {
    list_agents(ws).into_iter().map(|a| a.def).collect()
}

/// Validate a user/model-supplied agent name: it doubles as a filename, so keep
/// it to a safe charset, and never let it claim the reserved built-in.
pub fn validate_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!("agent name is empty"));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(anyhow!("agent name must use only letters, digits, '-' or '_'"));
    }
    if name == EXPLORE_AGENT {
        return Err(anyhow!("'{EXPLORE_AGENT}' is a reserved built-in agent"));
    }
    Ok(name.to_string())
}

/// Intersect a requested tool list with the read-only set. Empty/all-invalid
/// falls back to the full read-only set (a usable default).
pub fn normalize_tools(raw: &[String]) -> Vec<String> {
    let filtered: Vec<String> = raw
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| READONLY_TOOLS.contains(&t.as_str()))
        .collect();
    if filtered.is_empty() {
        READONLY_TOOLS.iter().map(|s| s.to_string()).collect()
    } else {
        filtered
    }
}

/// Render the `.md` file for an agent: frontmatter (name/description/tools, and
/// `max_rounds` when set) plus the system prompt as the body. Shared by the
/// model tool and the UI.
pub fn render_agent_file(
    name: &str,
    description: &str,
    prompt: &str,
    tools: &[String],
    max_rounds: Option<usize>,
) -> String {
    let description = description.trim().replace(['\n', '\r'], " ");
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {name}\n"));
    out.push_str(&format!("description: {description}\n"));
    out.push_str(&format!("tools: {}\n", tools.join(", ")));
    if let Some(rounds) = max_rounds {
        out.push_str(&format!("max_rounds: {}\n", clamp_rounds(rounds)));
    }
    out.push_str("---\n\n");
    out.push_str(prompt.trim());
    out.push('\n');
    out
}

/// Write an agent file for `scope`, creating the directory if needed. Returns
/// the written path. `tools` is normalized to the read-only set.
pub fn save_agent(
    scope: AgentScope,
    workspace_root: &Path,
    name: &str,
    description: &str,
    prompt: &str,
    tools: &[String],
    max_rounds: Option<usize>,
) -> Result<PathBuf> {
    let name = validate_name(name)?;
    if prompt.trim().is_empty() {
        return Err(anyhow!("agent prompt is empty"));
    }
    let dir = scope.dir(workspace_root)?;
    std::fs::create_dir_all(&dir).map_err(|e| anyhow!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{name}.md"));
    let tools = normalize_tools(tools);
    let max_rounds = max_rounds.map(clamp_rounds);
    std::fs::write(&path, render_agent_file(&name, description, prompt, &tools, max_rounds))
        .map_err(|e| anyhow!("could not write {}: {e}", path.display()))?;
    Ok(path)
}

/// Delete an agent file for `scope`. Missing file is a no-op error.
pub fn delete_agent(scope: AgentScope, workspace_root: &Path, name: &str) -> Result<()> {
    let name = validate_name(name)?;
    let path = scope.dir(workspace_root)?.join(format!("{name}.md"));
    std::fs::remove_file(&path).map_err(|e| anyhow!("could not delete {}: {e}", path.display()))?;
    Ok(())
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

    // Optional per-agent round budget; a non-numeric or out-of-range value is
    // clamped (invalid text is ignored → env/default applies).
    let max_rounds = front
        .as_ref()
        .and_then(|f| field(f, "max_rounds"))
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .map(clamp_rounds);

    Some(AgentDef {
        name,
        description,
        system_prompt: body.to_string(),
        tools,
        max_rounds,
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
    fn max_rounds_parses_clamps_and_defaults() {
        // A valid in-range value is kept.
        assert_eq!(parse_agent("x", "---\nmax_rounds: 30\n---\nbody").unwrap().max_rounds, Some(30));
        // Out-of-range is clamped to the hard ceiling / floor.
        assert_eq!(parse_agent("x", "---\nmax_rounds: 999\n---\nbody").unwrap().max_rounds,
                   Some(crate::subagent::SUBAGENT_ROUNDS_MAX));
        assert_eq!(parse_agent("x", "---\nmax_rounds: 1\n---\nbody").unwrap().max_rounds,
                   Some(crate::subagent::SUBAGENT_ROUNDS_MIN));
        // Non-numeric or absent → None (env/default applies).
        assert_eq!(parse_agent("x", "---\nmax_rounds: lots\n---\nbody").unwrap().max_rounds, None);
        assert_eq!(parse_agent("x", "---\ntools: read_file\n---\nbody").unwrap().max_rounds, None);
    }

    #[test]
    fn save_delete_and_list_round_trip_with_scope() {
        let dir = std::env::temp_dir().join(format!("locallmos-crud-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let ws = Workspace::new(dir.to_str().unwrap()).unwrap();

        // Save a project agent; write_file is dropped from the tool list. A
        // per-agent max_rounds is persisted and read back.
        let path = save_agent(
            AgentScope::Project, ws.root(), "reviewer",
            "Reviews code.", "You review code.", &["read_file".into(), "write_file".into()],
            Some(30),
        )
        .unwrap();
        assert!(path.starts_with(ws.root().join(".agents")));

        let listed = list_agents(&ws);
        let reviewer = listed.iter().find(|a| a.def.name == "reviewer").unwrap();
        assert_eq!(reviewer.scope, "project");
        assert!(reviewer.editable);
        assert_eq!(reviewer.def.tools, vec!["read_file"]);
        assert_eq!(reviewer.def.max_rounds, Some(30));
        // The built-in is present and not editable.
        assert!(listed.iter().any(|a| a.def.name == EXPLORE_AGENT && !a.editable));

        // Delete it and confirm it's gone from the listing.
        delete_agent(AgentScope::Project, ws.root(), "reviewer").unwrap();
        assert!(!list_agents(&ws).iter().any(|a| a.def.name == "reviewer"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_agent_rejects_reserved_and_empty_prompt() {
        let dir = std::env::temp_dir().join(format!("locallmos-crud-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let ws = Workspace::new(dir.to_str().unwrap()).unwrap();
        assert!(save_agent(AgentScope::Project, ws.root(), "explore", "d", "p", &[], None).is_err());
        assert!(save_agent(AgentScope::Project, ws.root(), "ok", "d", "   ", &[], None).is_err());
        std::fs::remove_dir_all(&dir).ok();
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
