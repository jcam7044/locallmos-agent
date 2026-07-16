//! Model-agnostic (prompt-injected) tool calling.
//!
//! Ollama's native tool calling depends on the model's chat template rendering
//! `.Tools` and emitting parseable `tool_calls`. Locally-imported/fine-tuned
//! models frequently ship a stripped template (e.g. a bare `{{ .Prompt }}`) that
//! does neither — the model is never told the tools exist and improvises a call
//! as plain text, which the native path silently drops.
//!
//! For those models the agent falls back to this protocol: it describes the
//! tools in the system prompt and parses tool calls out of the model's text
//! output. The wire format is the widely-emitted `<tool_call>` block:
//!
//! ```text
//! <tool_call>{"name": "web_search", "arguments": {"query": "..."}}</tool_call>
//! ```
//!
//! Everything here is pure and unit-tested; the wiring lives in `chat.rs`.

use super::ollama::ToolCall;
use serde_json::Value;

/// Opening/closing delimiters of a text tool call. ASCII, so byte offsets from
/// `find` are always valid `str` boundaries.
const OPEN: &str = "<tool_call>";
const CLOSE: &str = "</tool_call>";

/// Build the system-prompt section that advertises `defs` (Ollama function
/// definitions: `{"type":"function","function":{name,description,parameters}}`)
/// and specifies the text call contract. Returns an empty string if there are
/// no usable tools, so the caller can skip injecting a system message.
pub fn manifest_system_prompt(defs: &[Value]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for def in defs {
        let f = def.get("function").unwrap_or(def);
        let Some(name) = f.get("name").and_then(Value::as_str) else {
            continue;
        };
        let desc = f.get("description").and_then(Value::as_str).unwrap_or("");
        let params = f
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
        // Compact one-line schema keeps the manifest small in the context window.
        let schema = serde_json::to_string(&params).unwrap_or_else(|_| "{}".into());
        lines.push(format!("- {name}: {desc}\n  parameters (JSON Schema): {schema}"));
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "You can use tools to help answer. The available tools are:\n{}\n\n\
When you need a tool, reply with ONLY the tool call and no other text, using this exact format:\n\
<tool_call>{{\"name\": \"<tool_name>\", \"arguments\": {{ <json arguments> }}}}</tool_call>\n\
You may emit more than one <tool_call> block if you need several tools. After the tool results \
come back, use them to answer. Only call the tools listed above, and only when they help. If no \
tool is needed, just answer normally without any <tool_call> block.",
        lines.join("\n")
    )
}

/// Extract tool calls from model text. Recognises `<tool_call>…</tool_call>`
/// blocks whose body is a JSON object with a `name` and (optionally) `arguments`;
/// `arguments` may be an object or a JSON-encoded string. Unparseable blocks are
/// skipped rather than executed, so malformed output can never trigger a tool.
pub fn parse_text_tool_calls(content: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let (body, next) = match after.find(CLOSE) {
            Some(end) => (&after[..end], &after[end + CLOSE.len()..]),
            // Unterminated final block (e.g. truncated stream): take the tail.
            None => (after, ""),
        };
        if let Some(call) = parse_call_body(body) {
            calls.push(call);
        }
        rest = next;
    }
    calls
}

/// Parse one `<tool_call>` body into a `ToolCall`. Tolerates a `{"function": …}`
/// wrapper and stringified `arguments`.
fn parse_call_body(body: &str) -> Option<ToolCall> {
    let value: Value = serde_json::from_str(body.trim()).ok()?;
    let obj = value.get("function").unwrap_or(&value);
    let name = obj.get("name").and_then(Value::as_str)?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let arguments = match obj.get("arguments") {
        // Some models encode arguments as a JSON string; decode when possible.
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Object(Default::default())),
        Some(v) => v.clone(),
        None => Value::Object(Default::default()),
    };
    Some(ToolCall { name, arguments })
}

/// Remove every `<tool_call>` block from `content`, returning the trimmed
/// remainder. Used when persisting a turn that hit the tool-round cap, so the
/// stored answer never contains raw call syntax.
pub fn strip_tool_calls(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        match after.find(CLOSE) {
            Some(end) => rest = &after[end + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Streaming suppressor: forwards assistant content to the live UI stream but
/// withholds any `<tool_call>` block (which is bookkeeping, not answer text).
///
/// Because deltas arrive in arbitrary chunks, it holds back a short tail that
/// could be the start of an `OPEN` tag; once a full `OPEN` is seen it suppresses
/// the remainder of the turn (models are instructed to emit only the call when
/// calling a tool). The full raw content is still accumulated separately by the
/// runtime for parsing — this only governs what the user sees stream by.
#[derive(Default)]
pub struct ToolCallStreamFilter {
    pending: String,
    suppressing: bool,
}

impl ToolCallStreamFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one content delta; returns the text safe to forward to the UI now.
    pub fn push(&mut self, delta: &str) -> String {
        if self.suppressing {
            return String::new();
        }
        self.pending.push_str(delta);
        if let Some(idx) = self.pending.find(OPEN) {
            // Forward text before the tag, then suppress everything after.
            let out = self.pending[..idx].to_string();
            self.pending.clear();
            self.suppressing = true;
            return out;
        }
        // No complete tag yet: emit everything except a trailing partial that
        // might grow into `OPEN` on the next delta.
        let hold = partial_open_suffix(&self.pending);
        let split = self.pending.len() - hold;
        let out = self.pending[..split].to_string();
        self.pending.drain(..split);
        out
    }

    /// Flush any held-back tail at end of stream. Empty once suppressing (the
    /// held text was the tool call and must stay hidden).
    pub fn finish(&mut self) -> String {
        if self.suppressing {
            return String::new();
        }
        std::mem::take(&mut self.pending)
    }
}

/// Length of the longest suffix of `s` that is a proper prefix of `OPEN` (so it
/// might complete into an opening tag). `OPEN` is ASCII, so the returned length
/// is a valid byte split point.
fn partial_open_suffix(s: &str) -> usize {
    let bytes = s.as_bytes();
    let open = OPEN.as_bytes();
    let max = open.len().saturating_sub(1).min(bytes.len());
    for k in (1..=max).rev() {
        if bytes[bytes.len() - k..] == open[..k] {
            return k;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn manifest_lists_tools_and_contract() {
        let defs = vec![json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web.",
                "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}
            }
        })];
        let m = manifest_system_prompt(&defs);
        assert!(m.contains("web_search"));
        assert!(m.contains("Search the web."));
        assert!(m.contains("<tool_call>"));
    }

    #[test]
    fn manifest_empty_when_no_tools() {
        assert_eq!(manifest_system_prompt(&[]), "");
    }

    #[test]
    fn parses_object_arguments() {
        let calls = parse_text_tool_calls(
            "Sure.\n<tool_call>{\"name\": \"web_search\", \"arguments\": {\"query\": \"florida\"}}</tool_call>",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].arguments["query"], "florida");
    }

    #[test]
    fn parses_stringified_arguments_and_function_wrapper() {
        let calls = parse_text_tool_calls(
            "<tool_call>{\"function\": {\"name\": \"web_fetch_page\", \"arguments\": \"{\\\"url\\\": \\\"https://x.com\\\"}\"}}</tool_call>",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "web_fetch_page");
        assert_eq!(calls[0].arguments["url"], "https://x.com");
    }

    #[test]
    fn parses_multiple_and_ignores_malformed() {
        let calls = parse_text_tool_calls(
            "<tool_call>{\"name\": \"a\", \"arguments\": {}}</tool_call>\
             <tool_call>not json</tool_call>\
             <tool_call>{\"name\": \"b\", \"arguments\": {}}</tool_call>",
        );
        let names: Vec<_> = calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn no_tool_call_returns_empty() {
        assert!(parse_text_tool_calls("just a normal answer with a < b comparison").is_empty());
    }

    #[test]
    fn strip_removes_blocks() {
        let s = strip_tool_calls("before <tool_call>{\"name\":\"a\"}</tool_call> after");
        assert_eq!(s, "before  after".trim());
    }

    #[test]
    fn filter_passes_plain_text() {
        let mut f = ToolCallStreamFilter::new();
        let mut seen = String::new();
        seen.push_str(&f.push("Hello "));
        seen.push_str(&f.push("world"));
        seen.push_str(&f.finish());
        assert_eq!(seen, "Hello world");
    }

    #[test]
    fn filter_suppresses_tool_call_even_when_split() {
        let mut f = ToolCallStreamFilter::new();
        let mut seen = String::new();
        // Tag arrives split across deltas; text before it still streams.
        for d in ["Let me check. <to", "ol_call>{\"name\":\"web_", "search\"}</tool_call>"] {
            seen.push_str(&f.push(d));
        }
        seen.push_str(&f.finish());
        assert_eq!(seen, "Let me check. ");
    }

    #[test]
    fn filter_holds_then_releases_false_partial() {
        let mut f = ToolCallStreamFilter::new();
        let mut seen = String::new();
        // "<to" looks like a partial open tag, but turns out to be prose.
        seen.push_str(&f.push("a <to"));
        seen.push_str(&f.push("morrow"));
        seen.push_str(&f.finish());
        assert_eq!(seen, "a <tomorrow");
    }
}
