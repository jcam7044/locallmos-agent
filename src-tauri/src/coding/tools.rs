//! Local coding-tool executors. Every path goes through `Workspace::resolve`
//! for confinement; commands run with the working directory pinned to the
//! workspace root. Errors bubble up to `super::execute`, which turns them into
//! model-visible tool content rather than failing the turn.

use super::{CodingContext, CodingHost, ToolRun};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

/// Cap on file bytes returned to the model / considered for editing.
const MAX_READ_BYTES: u64 = 512 * 1024;
/// Cap on command output (stdout+stderr) fed back to the model.
const MAX_OUTPUT_CHARS: usize = 30_000;
/// Cap on a rendered diff preview.
const MAX_DIFF_CHARS: usize = 6_000;
/// Wall-clock limit for a single command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(180);
/// Directories never descended into during search.
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".venv", "__pycache__", ".next"];

/// Dispatch a validated coding tool call.
pub async fn run(cx: &CodingContext, host: Option<&CodingHost>, name: &str, args: &Value) -> Result<ToolRun> {
    match name {
        "read_file" => read_file(cx, args),
        "list_dir" => list_dir(cx, args),
        "search" => search(cx, args),
        "write_file" | "edit_file" => write_change(cx, name, args),
        "run_command" => {
            let cmd = str_arg(args, "command")?;
            run_shell(cx, &cmd).await
        }
        "git" => {
            let a = str_arg(args, "args")?;
            run_shell(cx, &format!("git {a}")).await
        }
        name if name.starts_with("dev_server_") || name.starts_with("web_preview_") => {
            let host = host.ok_or_else(|| anyhow!("web preview tools are available only in local desktop coding sessions"))?;
            run_preview(cx, host, name, args).await
        }
        name if name.starts_with(crate::mcp::MCP_PREFIX) => run_mcp(cx, name, args).await,
        other => Err(anyhow!("unknown coding tool: {other}")),
    }
}

/// Execute an MCP tool via the manager on `CodingContext`. Result text is
/// truncated to the same budget as command output; a server-reported error
/// becomes a failed (but non-aborting) tool result.
async fn run_mcp(cx: &CodingContext, name: &str, args: &Value) -> Result<ToolRun> {
    let manager = cx
        .mcp
        .manager
        .as_ref()
        .ok_or_else(|| anyhow!("MCP tools are not available in this session"))?;
    let outcome = manager.call_tool(name, args).await?;
    let provider = format!("mcp:{}", outcome.server_id);
    let content = truncate(&outcome.text, MAX_OUTPUT_CHARS);
    let (status, summary) = if outcome.is_error {
        ("failed", format!("{} error", outcome.tool_name))
    } else {
        ("succeeded", outcome.tool_name.clone())
    };
    Ok(ToolRun {
        content,
        summary: summary.clone(),
        event: None,
        activity: Some(json!({
            "name": name, "provider": provider, "status": status,
            "summary": summary, "citations": [],
        })),
    })
}

async fn run_preview(cx: &CodingContext, host: &CodingHost, name: &str, args: &Value) -> Result<ToolRun> {
    let result = match name {
        "dev_server_start" => {
            let command = str_arg(args, "command")?;
            let url = str_arg(args, "url")?;
            let timeout = args.get("timeout_seconds").and_then(Value::as_u64).unwrap_or(30).clamp(1, 120);
            host.preview
                .start_server(&host.app, &host.session_id, cx.workspace.root(), &command, &url, Duration::from_secs(timeout))
                .await?
        }
        "dev_server_logs" => host.preview.server_logs(&host.session_id, args.get("clear").and_then(Value::as_bool).unwrap_or(false)).await?,
        "dev_server_stop" => host.preview.stop_server(&host.app, &host.session_id).await?,
        "web_preview_open" => {
            let url = str_arg(args, "url")?;
            let width = args.get("width").and_then(Value::as_u64).unwrap_or(1280).clamp(320, 3840) as u32;
            let height = args.get("height").and_then(Value::as_u64).unwrap_or(800).clamp(240, 2160) as u32;
            host.preview.open(&host.app, &host.session_id, &url, width, height).await?
        }
        "web_preview_snapshot" => host.preview.snapshot(&host.app, &host.session_id, args.get("selector").and_then(Value::as_str)).await?,
        "web_preview_click" => host.preview.click(&host.app, &host.session_id, &str_arg(args, "ref")?).await?,
        "web_preview_fill" => host.preview.fill(
            &host.app,
            &host.session_id,
            &str_arg(args, "ref")?,
            &str_arg_allow_empty(args, "text")?,
            args.get("submit").and_then(Value::as_bool).unwrap_or(false),
        ).await?,
        "web_preview_press" => host.preview.press(&host.app, &host.session_id, &str_arg(args, "ref")?, &str_arg(args, "key")?).await?,
        "web_preview_reload" => host.preview.reload(&host.app, &host.session_id).await?,
        "web_preview_resize" => {
            let width = args.get("width").and_then(Value::as_u64).ok_or_else(|| anyhow!("missing required argument 'width'"))? as u32;
            let height = args.get("height").and_then(Value::as_u64).ok_or_else(|| anyhow!("missing required argument 'height'"))? as u32;
            host.preview.resize(&host.app, &host.session_id, width, height).await?
        }
        "web_preview_console" => host.preview.console(&host.app, &host.session_id, args.get("clear").and_then(Value::as_bool).unwrap_or(false)).await?,
        "web_preview_close" => {
            host.preview.close_session(&host.app, &host.session_id, true).await?;
            "Closed preview and stopped its development server.".into()
        }
        _ => return Err(anyhow!("unknown preview tool: {name}")),
    };
    let content = truncate(&result, MAX_OUTPUT_CHARS);
    Ok(ToolRun {
        summary: preview_summary(name, &content),
        content,
        event: None,
        activity: Some(json!({
            "name": name, "provider": "coding", "status": "succeeded",
            "summary": preview_summary(name, &result), "citations": [],
        })),
    })
}

fn preview_summary(name: &str, result: &str) -> String {
    match name {
        "web_preview_snapshot" => "inspected preview".into(),
        "web_preview_console" => "read browser console".into(),
        "dev_server_logs" => "read server logs".into(),
        _ => result.lines().next().unwrap_or(name).chars().take(120).collect(),
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

fn read_file(cx: &CodingContext, args: &Value) -> Result<ToolRun> {
    let path = cx.workspace.resolve(&str_arg(args, "path")?)?;
    let meta = std::fs::metadata(&path).map_err(|e| anyhow!("cannot stat file: {e}"))?;
    if meta.is_dir() {
        return Err(anyhow!("path is a directory; use list_dir"));
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(anyhow!("file is too large to read ({} bytes)", meta.len()));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| anyhow!("not a UTF-8 text file: {e}"))?;
    let start = args.get("start_line").and_then(Value::as_u64);
    let end = args.get("end_line").and_then(Value::as_u64);
    let content = if start.is_some() || end.is_some() {
        let lines: Vec<&str> = text.lines().collect();
        let s = start.unwrap_or(1).max(1) as usize;
        let e = (end.unwrap_or(lines.len() as u64) as usize).min(lines.len());
        if s > e {
            return Err(anyhow!("start_line is past end_line"));
        }
        lines[s - 1..e].join("\n")
    } else {
        text
    };
    let rel = cx.workspace.display_relative(&path);
    let summary = format!("read {}", rel);
    Ok(ToolRun {
        content,
        summary: summary.clone(),
        event: None,
        activity: activity(&summary),
    })
}

fn list_dir(cx: &CodingContext, args: &Value) -> Result<ToolRun> {
    let rel_arg = args.get("path").and_then(Value::as_str).unwrap_or("");
    let dir = cx.workspace.resolve(rel_arg)?;
    if !dir.is_dir() {
        return Err(anyhow!("not a directory"));
    }
    let mut entries: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| anyhow!("cannot read directory: {e}"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            entries.push(format!("{name}/"));
        } else {
            entries.push(name);
        }
    }
    entries.sort();
    let rel = cx.workspace.display_relative(&dir);
    let summary = format!("{} entries in {}", entries.len(), rel);
    let content = if entries.is_empty() {
        "(empty directory)".to_string()
    } else {
        entries.join("\n")
    };
    Ok(ToolRun { content, summary: summary.clone(), event: None, activity: activity(&summary) })
}

fn search(cx: &CodingContext, args: &Value) -> Result<ToolRun> {
    let query = str_arg(args, "query")?;
    let re = regex::Regex::new(&query).map_err(|e| anyhow!("invalid regex: {e}"))?;
    let base = cx.workspace.resolve(args.get("path").and_then(Value::as_str).unwrap_or(""))?;
    let max = args
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as usize;

    let mut hits: Vec<String> = Vec::new();
    'walk: for entry in walkdir::WalkDir::new(&base)
        .into_iter()
        .filter_entry(|e| !is_skipped_dir(e))
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.metadata().map(|m| m.len() > 1_000_000).unwrap_or(true) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue; // skip binary / non-UTF-8
        };
        let rel = cx.workspace.display_relative(entry.path());
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                hits.push(format!("{rel}:{}: {}", i + 1, line.trim()));
                if hits.len() >= max {
                    break 'walk;
                }
            }
        }
    }
    let summary = format!("{} match{}", hits.len(), if hits.len() == 1 { "" } else { "es" });
    let content = if hits.is_empty() { "No matches.".to_string() } else { hits.join("\n") };
    Ok(ToolRun { content, summary: summary.clone(), event: None, activity: activity(&summary) })
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// Compute the prospective change for write_file/edit_file: (abs path, relative
/// path, old content, new content). Shared by the approval preview and execute.
fn prospective_change(cx: &CodingContext, name: &str, args: &Value) -> Result<(PathBuf, String, String, String)> {
    let path = cx.workspace.resolve(&str_arg(args, "path")?)?;
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let rel = cx.workspace.display_relative(&path);
    let new = match name {
        "write_file" => str_arg(args, "content")?,
        "edit_file" => {
            let old_string = str_arg(args, "old_string")?;
            let new_string = str_arg(args, "new_string")?;
            let replace_all = args.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
            let count = old.matches(&old_string).count();
            if count == 0 {
                return Err(anyhow!("old_string was not found in {rel}"));
            }
            if count > 1 && !replace_all {
                return Err(anyhow!("old_string occurs {count} times in {rel}; set replace_all or make it unique"));
            }
            if replace_all {
                old.replace(&old_string, &new_string)
            } else {
                old.replacen(&old_string, &new_string, 1)
            }
        }
        _ => unreachable!("prospective_change called for {name}"),
    };
    Ok((path, rel, old, new))
}

/// Render the diff a write/edit would produce, for the approval prompt.
pub fn change_preview(cx: &CodingContext, name: &str, args: &Value) -> Result<String> {
    let (_, rel, old, new) = prospective_change(cx, name, args)?;
    let (diff, added, removed) = line_diff(&old, &new);
    Ok(format!("{rel}  (+{added} -{removed})\n{diff}"))
}

fn write_change(cx: &CodingContext, name: &str, args: &Value) -> Result<ToolRun> {
    let (path, rel, old, new) = prospective_change(cx, name, args)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, &new).map_err(|e| anyhow!("could not write {rel}: {e}"))?;
    let (diff, added, removed) = line_diff(&old, &new);
    let summary = format!("+{added} -{removed}");
    Ok(ToolRun {
        content: format!("Wrote {rel} ({summary})."),
        summary: summary.clone(),
        event: Some(json!({ "type": "file_edit", "path": rel, "diff": diff, "summary": summary })),
        activity: Some(json!({
            "name": name, "provider": "coding", "status": "succeeded",
            "summary": format!("edited {rel}"), "citations": [],
        })),
    })
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn run_shell(cx: &CodingContext, command: &str) -> Result<ToolRun> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-c", command]);
        c
    };
    cmd.current_dir(cx.workspace.root());
    cmd.kill_on_drop(true);

    let output = match tokio::time::timeout(COMMAND_TIMEOUT, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(anyhow!("failed to run command: {e}")),
        Err(_) => return Err(anyhow!("command timed out after {}s", COMMAND_TIMEOUT.as_secs())),
    };

    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    let combined = truncate(&combined, MAX_OUTPUT_CHARS);
    let code = output.status.code();
    let code_label = code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
    let summary = format!("exit {code_label}");
    let content = format!("$ {command}\n{combined}\n[exit {code_label}]");
    Ok(ToolRun {
        content,
        summary: summary.clone(),
        event: Some(json!({
            "type": "command", "command": command, "chunk": combined, "exitCode": code,
        })),
        activity: Some(json!({
            "name": "run_command", "provider": "coding",
            "status": if code == Some(0) { "succeeded" } else { "failed" },
            "summary": summary, "citations": [],
        })),
    })
}

/// A git subcommand that only reads state — safe to run without approval.
pub fn git_is_readonly(args: &str) -> bool {
    const READONLY: &[&str] = &[
        "status", "diff", "log", "show", "branch", "rev-parse", "ls-files", "blame",
        "describe", "remote", "config", "cat-file", "shortlog", "reflog", "tag",
    ];
    let sub = args.split_whitespace().next().unwrap_or("");
    // `branch`/`tag` mutate only with extra args; treat bare/`-l`/`--list` as reads.
    match sub {
        "branch" | "tag" => args
            .split_whitespace()
            .skip(1)
            .all(|a| a.starts_with('-') && (a == "-l" || a == "--list" || a == "-a" || a == "-r" || a == "-v" || a == "-vv")),
        s => READONLY.contains(&s),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn str_arg(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("missing required argument '{key}'"))
}

fn str_arg_allow_empty(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("missing required argument '{key}'"))
}

fn is_skipped_dir(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_dir()
        && entry
            .file_name()
            .to_str()
            .map(|n| SKIP_DIRS.contains(&n))
            .unwrap_or(false)
}

fn activity(summary: &str) -> Option<Value> {
    Some(json!({
        "name": "coding", "provider": "coding", "status": "succeeded",
        "summary": summary, "citations": [],
    }))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…[truncated]");
    out
}

/// A compact line diff: common prefix/suffix are collapsed; the changed middle
/// is shown as `- old` / `+ new` with a little surrounding context. Good enough
/// for approval previews and the file_edit event (not a full Myers diff).
fn line_diff(old: &str, new: &str) -> (String, usize, usize) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut start = 0;
    while start < old_lines.len() && start < new_lines.len() && old_lines[start] == new_lines[start] {
        start += 1;
    }
    let mut end_old = old_lines.len();
    let mut end_new = new_lines.len();
    while end_old > start && end_new > start && old_lines[end_old - 1] == new_lines[end_new - 1] {
        end_old -= 1;
        end_new -= 1;
    }

    let removed = &old_lines[start..end_old];
    let added = &new_lines[start..end_new];

    let mut out = String::new();
    let ctx_start = start.saturating_sub(2);
    for l in &old_lines[ctx_start..start] {
        out.push_str("  ");
        out.push_str(l);
        out.push('\n');
    }
    for l in removed {
        out.push_str("- ");
        out.push_str(l);
        out.push('\n');
    }
    for l in added {
        out.push_str("+ ");
        out.push_str(l);
        out.push('\n');
    }
    let ctx_end = (end_old + 2).min(old_lines.len());
    for l in &old_lines[end_old..ctx_end] {
        out.push_str("  ");
        out.push_str(l);
        out.push('\n');
    }
    (truncate(&out, MAX_DIFF_CHARS), added.len(), removed.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_readonly_classification() {
        assert!(git_is_readonly("status"));
        assert!(git_is_readonly("diff --staged"));
        assert!(git_is_readonly("branch --list"));
        assert!(!git_is_readonly("commit -m x"));
        assert!(!git_is_readonly("branch new-feature"));
        assert!(!git_is_readonly("push"));
        assert!(!git_is_readonly("add -A"));
    }

    #[test]
    fn diff_counts_changes() {
        let (_d, added, removed) = line_diff("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!((added, removed), (1, 1));
        let (_d, added, removed) = line_diff("a\n", "a\nb\nc\n");
        assert_eq!((added, removed), (2, 0));
    }
}
