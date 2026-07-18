//! Model-agnostic (prompt-injected) tool calling.
//!
//! Ollama's native tool calling depends on the model's chat template rendering
//! `.Tools` and emitting parseable `tool_calls`. Locally-imported/fine-tuned
//! models frequently ship a stripped template (e.g. a bare `{{ .Prompt }}`) that
//! does neither — the model is never told the tools exist and improvises a call
//! as plain text, which the native path silently drops.
//!
//! For those models the agent falls back to this protocol: it describes the
//! tools in the prompt and parses tool calls out of the model's text output.
//! Models are asked for the `<tool_call>` block form, but they are wildly
//! inconsistent, so the parser accepts several shapes and resolves near-miss
//! tool names (e.g. `search` → `web_search`):
//!
//! ```text
//! <tool_call>{"name": "web_search", "arguments": {"query": "x"}}</tool_call>
//! {"name": "web_search", "query": "x"}
//! [search(query="x", count=10)]
//! ```
//!
//! Everything here is pure and unit-tested; the wiring lives in `chat.rs`.

use super::ollama::ToolCall;
use serde_json::{json, Map, Value};

/// Opening/closing delimiters of a wrapped text tool call. ASCII, so byte
/// offsets from `find` are always valid `str` boundaries.
const OPEN: &str = "<tool_call>";
const CLOSE: &str = "</tool_call>";

/// Top-level keys that are call metadata rather than arguments, when a model
/// inlines arguments alongside `name` instead of nesting them.
const RESERVED_KEYS: [&str; 5] = ["name", "function", "arguments", "type", "id"];

/// Build the prompt section that advertises `defs` (Ollama function definitions:
/// `{"type":"function","function":{name,description,parameters}}`) and specifies
/// the text call contract with a worked example. Returns an empty string if
/// there are no usable tools, so the caller can skip injecting it.
pub fn manifest_system_prompt(defs: &[Value]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut example: Option<String> = None;
    for def in defs {
        let f = def.get("function").unwrap_or(def);
        let Some(name) = f.get("name").and_then(Value::as_str) else {
            continue;
        };
        let desc = f.get("description").and_then(Value::as_str).unwrap_or("");
        let params = f
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" }));
        let schema = serde_json::to_string(&params).unwrap_or_else(|_| "{}".into());
        lines.push(format!("- {name}: {desc}\n  parameters (JSON Schema): {schema}"));
        example.get_or_insert_with(|| example_call(name, &params));
    }
    if lines.is_empty() {
        return String::new();
    }
    let example = example.unwrap_or_else(|| {
        "<tool_call>{\"name\": \"<tool_name>\", \"arguments\": {}}</tool_call>".into()
    });
    format!(
        "You have access to these tools:\n{}\n\n\
To use a tool, output ONLY the following and nothing else:\n\
<tool_call>{{\"name\": \"<exact_tool_name>\", \"arguments\": {{<json arguments>}}}}</tool_call>\n\
Example:\n{example}\n\
Rules:\n\
- Use the exact tool names listed above. Do NOT invent names like \"search\" or \"google_search\".\n\
- Put every argument inside the \"arguments\" object, as JSON.\n\
- Emit the tool call by itself, with no surrounding prose.\n\
- After the tool result comes back, use it to write your answer.\n\
- If no tool is needed, just answer normally with no tool call.",
        lines.join("\n")
    )
}

/// A concrete `<tool_call>` example for `name`, filling the first schema property
/// with a placeholder so the model sees the exact expected shape.
fn example_call(name: &str, params: &Value) -> String {
    let mut args = Map::new();
    if let Some(props) = params.get("properties").and_then(Value::as_object) {
        if let Some((key, _)) = props.iter().next() {
            args.insert(key.clone(), json!("…"));
        }
    }
    let args = Value::Object(args);
    format!(
        "<tool_call>{{\"name\": \"{name}\", \"arguments\": {}}}</tool_call>",
        serde_json::to_string(&args).unwrap_or_else(|_| "{}".into())
    )
}

/// Extract tool calls from model text, trying each recognised shape in
/// confidence order and resolving near-miss names against `known_tools`. The
/// name check also keeps a JSON/DSL-shaped *answer* from being mistaken for a
/// call. Unparseable input yields no calls, so malformed output can never
/// trigger a tool.
pub fn parse_text_tool_calls(content: &str, known_tools: &[String]) -> Vec<ToolCall> {
    // 1. Explicit wrapped blocks — highest confidence, may be several. Names are
    //    resolved but a wrapped call is kept even if unresolved (chat.rs routes
    //    an unknown name to passthrough rather than executing it).
    let mut wrapped = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let (body, next) = match after.find(CLOSE) {
            Some(end) => (&after[..end], &after[end + CLOSE.len()..]),
            None => (after, ""), // unterminated final block (truncated stream)
        };
        if let Some(mut call) = parse_call_body(body) {
            if let Some(name) = resolve_name(&call.name, known_tools) {
                call.name = name;
            }
            wrapped.push(call);
        }
        rest = next;
    }
    if !wrapped.is_empty() {
        return wrapped;
    }

    // 2. `[name(args)]` / `name(args)` function-call DSL, then 3. a bare JSON
    //    object. Both are ambiguous with ordinary text, so require a resolvable
    //    tool name before treating them as a call.
    for candidate in [parse_dsl_call(content), parse_call_body(strip_code_fence(content.trim()))] {
        if let Some(mut call) = candidate {
            if let Some(name) = resolve_name(&call.name, known_tools) {
                call.name = name;
                return vec![call];
            }
        }
    }
    Vec::new()
}

/// Parse one JSON tool-call object into a `ToolCall`. Tolerates a `{"function": …}`
/// wrapper, a stringified `arguments`, and arguments inlined as sibling keys.
fn parse_call_body(body: &str) -> Option<ToolCall> {
    let value: Value = serde_json::from_str(body.trim()).ok()?;
    let obj = value.get("function").unwrap_or(&value);
    let name = obj.get("name").and_then(Value::as_str)?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let arguments = match obj.get("arguments") {
        Some(Value::String(s)) => {
            serde_json::from_str(s).unwrap_or_else(|_| Value::Object(Map::new()))
        }
        Some(v) => v.clone(),
        None => {
            // No `arguments` key: treat remaining top-level fields as the args.
            let mut m = Map::new();
            if let Some(o) = obj.as_object() {
                for (k, v) in o {
                    if !RESERVED_KEYS.contains(&k.as_str()) {
                        m.insert(k.clone(), v.clone());
                    }
                }
            }
            Value::Object(m)
        }
    };
    Some(ToolCall { name, arguments })
}

/// Parse the first `name(key=value, …)` call (optionally bracketed as
/// `[name(...)]`) found in `content`. Values may be quoted strings, numbers, or
/// booleans; positional (unnamed) arguments are dropped since they can't be
/// mapped to a schema.
fn parse_dsl_call(content: &str) -> Option<ToolCall> {
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if is_ident_start(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_ident(bytes[i]) {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'(' {
                if let Some((inner, _end)) = capture_parens(content, i) {
                    return Some(ToolCall {
                        name: content[start..i].to_string(),
                        arguments: parse_dsl_args(inner),
                    });
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}
fn is_ident(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

/// Given the index of an opening `(`, return the substring between it and its
/// matching `)` (respecting quoted strings) plus the close index.
fn capture_parens(s: &str, open: usize) -> Option<(&str, usize)> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut i = open;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((&s[open + 1..i], i));
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Parse `key=value, key=value` DSL argument lists into a JSON object.
fn parse_dsl_args(s: &str) -> Value {
    let mut m = Map::new();
    for part in split_top_commas(s) {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            let key = k.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
            if !key.is_empty() {
                m.insert(key, parse_dsl_value(v.trim()));
            }
        }
    }
    Value::Object(m)
}

/// Split on commas that are not inside quotes or nested parens/brackets.
fn split_top_commas(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    for (i, &c) in bytes.iter().enumerate() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth == 0 => {
                    parts.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
    }
    parts.push(&s[start..]);
    parts
}

fn parse_dsl_value(v: &str) -> Value {
    let v = v.trim();
    if v.len() >= 2 {
        let b = v.as_bytes();
        if (b[0] == b'"' && b[v.len() - 1] == b'"') || (b[0] == b'\'' && b[v.len() - 1] == b'\'') {
            return Value::String(v[1..v.len() - 1].to_string());
        }
    }
    match v {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    if let Ok(n) = v.parse::<i64>() {
        return json!(n);
    }
    if let Ok(f) = v.parse::<f64>() {
        return json!(f);
    }
    Value::String(v.to_string())
}

/// Resolve a model-supplied tool name to one of `known` names. Matches exactly
/// (case-insensitive) or, failing that, when the called name shares a token
/// (split on `_ - .`) with exactly one known tool — so `search`/`google_search`
/// resolve to `web_search`, `fetch` to `web_fetch_page`, while an ambiguous
/// token like `web` (shared by both) resolves to nothing.
fn resolve_name(called: &str, known: &[String]) -> Option<String> {
    let c = called.trim().to_lowercase();
    if c.is_empty() {
        return None;
    }
    if let Some(k) = known.iter().find(|k| k.to_lowercase() == c) {
        return Some(k.clone());
    }
    let ctoks = tokens(&c);
    let mut hit: Option<&String> = None;
    for k in known {
        let ktoks = tokens(&k.to_lowercase());
        if ctoks.iter().any(|t| ktoks.contains(t)) {
            if hit.is_some() {
                return None; // ambiguous: more than one known tool shares a token
            }
            hit = Some(k);
        }
    }
    hit.cloned()
}

fn tokens(s: &str) -> Vec<String> {
    s.split(|c| c == '_' || c == '-' || c == '.')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Remove `<tool_call>` blocks from `content`, returning the trimmed remainder.
/// Used when persisting a turn that hit the tool-round cap, so the stored answer
/// never contains raw call syntax.
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
/// withholds tool-call syntax. A tool-call round's content is usually *only* the
/// call, so once the round opens with a JSON object (`{`) or bracketed DSL (`[`)
/// the whole round is suppressed; `<tool_call>` blocks are suppressed from the
/// tag onward even after prose, with a short held-back tail to catch a tag split
/// across deltas. The full raw content is still accumulated by the runtime for
/// parsing — this only governs what streams by live (the persisted row is
/// authoritative regardless).
#[derive(Default)]
pub struct ToolCallStreamFilter {
    pending: String,
    started: bool,
    suppressing: bool,
}

impl ToolCallStreamFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, delta: &str) -> String {
        if self.suppressing {
            return String::new();
        }
        self.pending.push_str(delta);
        if !self.started {
            let trimmed = self.pending.trim_start();
            if trimmed.is_empty() {
                return String::new(); // only whitespace so far; wait for more
            }
            self.started = true;
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                self.suppressing = true;
                self.pending.clear();
                return String::new();
            }
        }
        if let Some(idx) = self.pending.find(OPEN) {
            let out = self.pending[..idx].to_string();
            self.pending.clear();
            self.suppressing = true;
            return out;
        }
        let hold = partial_open_suffix(&self.pending);
        let split = self.pending.len() - hold;
        let out = self.pending[..split].to_string();
        self.pending.drain(..split);
        out
    }

    pub fn finish(&mut self) -> String {
        if self.suppressing {
            return String::new();
        }
        std::mem::take(&mut self.pending)
    }
}

/// Longest suffix of `s` that is a proper prefix of `OPEN` (so it might complete
/// into an opening tag). `OPEN` is ASCII, so the length is a valid split point.
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

/// Strip a surrounding Markdown code fence if the whole string is one.
fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    let Some(inner) = s.strip_prefix("```") else {
        return s;
    };
    let Some(end) = inner.rfind("```") else {
        return s;
    };
    let body = &inner[..end];
    match body.split_once('\n') {
        Some((first, tail)) if !first.trim().is_empty() && !first.contains('{') => tail.trim(),
        _ => body.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        vec!["web_search".into(), "web_fetch_page".into()]
    }

    #[test]
    fn manifest_lists_tools_and_example() {
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
        assert!(m.contains("Example:"));
        // Example uses the real tool name and first property.
        assert!(m.contains("\"name\": \"web_search\""));
        assert!(m.contains("query"));
    }

    #[test]
    fn manifest_empty_when_no_tools() {
        assert_eq!(manifest_system_prompt(&[]), "");
    }

    #[test]
    fn parses_wrapped_object_arguments() {
        let calls = parse_text_tool_calls(
            "Sure.\n<tool_call>{\"name\": \"web_search\", \"arguments\": {\"query\": \"florida\"}}</tool_call>",
            &known(),
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].arguments["query"], "florida");
    }

    #[test]
    fn parses_bare_json_with_inline_args() {
        let calls = parse_text_tool_calls(
            "{\"name\": \"web_search\", \"query\": \"fishing in the Florida Panhandle\"}",
            &known(),
        );
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].arguments["query"], "fishing in the Florida Panhandle");
    }

    #[test]
    fn parses_dsl_call_and_resolves_name() {
        // The exact shape observed: bracketed DSL with a generic name.
        let calls = parse_text_tool_calls(
            "I'll search now. [search(query=\"fishing Florida Panhandle best spots\", count=10)]",
            &known(),
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "web_search"); // resolved from "search"
        assert_eq!(calls[0].arguments["query"], "fishing Florida Panhandle best spots");
        assert_eq!(calls[0].arguments["count"], 10);
    }

    #[test]
    fn resolves_google_search_to_web_search() {
        let calls = parse_text_tool_calls(
            "{\"name\": \"google_search\", \"query\": \"x\"}",
            &known(),
        );
        assert_eq!(calls[0].name, "web_search");
    }

    #[test]
    fn ambiguous_or_unknown_name_is_not_a_call() {
        // "web" is shared by both tools → ambiguous → not resolved.
        assert!(parse_text_tool_calls("{\"name\": \"web\", \"q\": \"x\"}", &known()).is_empty());
        // Unrelated JSON answer stays an answer.
        assert!(parse_text_tool_calls("{\"name\": \"Bob\", \"age\": 3}", &known()).is_empty());
    }

    #[test]
    fn parses_fenced_bare_json() {
        let calls = parse_text_tool_calls(
            "```json\n{\"name\": \"web_search\", \"query\": \"x\"}\n```",
            &known(),
        );
        assert_eq!(calls[0].name, "web_search");
    }

    #[test]
    fn parses_stringified_arguments_and_function_wrapper() {
        let calls = parse_text_tool_calls(
            "<tool_call>{\"function\": {\"name\": \"web_fetch_page\", \"arguments\": \"{\\\"url\\\": \\\"https://x.com\\\"}\"}}</tool_call>",
            &known(),
        );
        assert_eq!(calls[0].name, "web_fetch_page");
        assert_eq!(calls[0].arguments["url"], "https://x.com");
    }

    #[test]
    fn parses_multiple_wrapped_and_ignores_malformed() {
        let calls = parse_text_tool_calls(
            "<tool_call>{\"name\": \"web_search\", \"arguments\": {}}</tool_call>\
             <tool_call>not json</tool_call>\
             <tool_call>{\"name\": \"web_fetch_page\", \"arguments\": {}}</tool_call>",
            &known(),
        );
        let names: Vec<_> = calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["web_search", "web_fetch_page"]);
    }

    #[test]
    fn no_tool_call_returns_empty() {
        assert!(parse_text_tool_calls("just a normal answer, see item (a) below", &known())
            .is_empty());
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
    fn filter_suppresses_bare_json_and_dsl_start() {
        for start in ["{\"name\": \"web_search\"}", "[search(query=\"x\")]"] {
            let mut f = ToolCallStreamFilter::new();
            let mut seen = String::new();
            seen.push_str(&f.push(start));
            seen.push_str(&f.finish());
            assert_eq!(seen, "", "should suppress: {start}");
        }
    }

    #[test]
    fn filter_suppresses_wrapped_call_even_after_prose() {
        let mut f = ToolCallStreamFilter::new();
        let mut seen = String::new();
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
        seen.push_str(&f.push("a <to"));
        seen.push_str(&f.push("morrow"));
        seen.push_str(&f.finish());
        assert_eq!(seen, "a <tomorrow");
    }
}
