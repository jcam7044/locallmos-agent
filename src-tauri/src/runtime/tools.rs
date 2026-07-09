//! Built-in tools the agent executes for a chat turn: `web_search` (relayed
//! through the cloud `web-search` edge function, so the per-user Brave key never
//! reaches the rig) and `web_fetch` (a direct GET from the rig, no key needed).
//!
//! The JSON-schema definitions here are sent to Ollama as `tools`; when the model
//! calls one, `chat.rs` executes it and feeds the result back.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

pub const WEB_SEARCH: &str = "web_search";
pub const WEB_FETCH: &str = "web_fetch";

/// True for tools the agent executes itself (vs. passthrough/caller tools).
pub fn is_builtin(name: &str) -> bool {
    name == WEB_SEARCH || name == WEB_FETCH
}

fn web_search_def() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": WEB_SEARCH,
            "description": "Search the web for up-to-date information. Returns a list of results with titles, URLs, and snippets.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "The search query." },
                    "count": { "type": "integer", "description": "Number of results (1-10).", "default": 5 }
                },
                "required": ["query"]
            }
        }
    })
}

fn web_fetch_def() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": WEB_FETCH,
            "description": "Fetch a web page by URL and return its readable text content. Use after web_search to read a promising result.",
            "parameters": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "The absolute URL to fetch." }
                },
                "required": ["url"]
            }
        }
    })
}

/// Ollama `tools` array for the built-in web tools.
pub fn builtin_defs() -> Value {
    json!([web_search_def(), web_fetch_def()])
}

/// Just `web_fetch` — the tool set for unenrolled rigs, where `web_search`
/// (which relays through the cloud edge function) is unavailable.
pub fn fetch_only_defs() -> Value {
    json!([web_fetch_def()])
}

/// Max characters of extracted page text handed back to the model.
const FETCH_CHAR_BUDGET: usize = 8000;

/// GET a URL and extract readable text. Best-effort: strips scripts/styles/tags,
/// decodes a few common entities, collapses whitespace, and truncates. Not a full
/// readability pass — enough to feed the model without a heavy dependency.
pub async fn web_fetch(http: &reqwest::Client, url: &str) -> Result<String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(anyhow!("web_fetch: url must be http(s)"));
    }
    let resp = http
        .get(url)
        .header("User-Agent", "LocalLMOS-Agent/1.0 (+https://locallmos.com)")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(anyhow!("web_fetch: HTTP {}", resp.status()));
    }
    let html = resp.text().await?;
    Ok(html_to_text(&html))
}

/// Very small HTML→text reduction. Removes `<script>`/`<style>` blocks and all
/// tags, decodes a handful of entities, collapses whitespace, and truncates.
pub fn html_to_text(html: &str) -> String {
    let without_blocks = strip_block(&strip_block(html, "script"), "style");

    let mut out = String::with_capacity(without_blocks.len() / 2);
    let mut in_tag = false;
    for ch in without_blocks.chars() {
        match ch {
            // Replace each tag with a space so adjacent block elements don't glue
            // their text together (e.g. "…welcome</h1><p>Rust…"). Collapsed later.
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }

    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");

    let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() > FETCH_CHAR_BUDGET {
        // Truncate on a char boundary.
        let mut end = FETCH_CHAR_BUDGET;
        while !collapsed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &collapsed[..end])
    } else {
        collapsed
    }
}

/// ASCII-case-insensitive byte search, so indices always line up with `s` (unlike
/// lowercasing the whole string, which can change byte length on Unicode input).
fn find_ci(hay: &[u8], needle_lower: &[u8], from: usize) -> Option<usize> {
    if needle_lower.is_empty() || from >= hay.len() {
        return None;
    }
    (from..=hay.len().saturating_sub(needle_lower.len())).find(|&i| {
        hay[i..i + needle_lower.len()]
            .iter()
            .zip(needle_lower)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

/// Remove `<tag>…</tag>` blocks (case-insensitive) entirely, including contents.
fn strip_block(s: &str, tag: &str) -> String {
    let bytes = s.as_bytes();
    let open = format!("<{tag}").into_bytes();
    let close = format!("</{tag}>").into_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if let Some(abs_start) = find_ci(bytes, &open, i) {
            out.push_str(&s[i..abs_start]);
            match find_ci(bytes, &close, abs_start) {
                Some(end) => i = end + close.len(),
                None => break, // unterminated block: drop the rest
            }
        } else {
            out.push_str(&s[i..]);
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_scripts_and_entities() {
        let html = "<html><head><style>.a{color:red}</style></head><body>\
            <script>var x = 1 < 2;</script>\
            <h1>Hello &amp; welcome</h1><p>Rust&nbsp;is   great</p></body></html>";
        let text = html_to_text(html);
        assert!(!text.contains("color:red"), "style block leaked: {text}");
        assert!(!text.contains("var x"), "script block leaked: {text}");
        assert_eq!(text, "Hello & welcome Rust is great");
    }

    #[test]
    fn handles_unicode_before_blocks_without_panic() {
        // Multibyte chars before a <script> must not misalign byte offsets.
        let html = "<p>café — résumé</p><SCRIPT>var π = 3.14;</SCRIPT><p>naïve</p>";
        let text = html_to_text(html);
        assert!(!text.contains("var"), "script leaked: {text}");
        assert_eq!(text, "café — résumé naïve");
    }

    #[test]
    fn builtins_recognized() {
        assert!(is_builtin(WEB_SEARCH));
        assert!(is_builtin(WEB_FETCH));
        assert!(!is_builtin("calculator"));
    }
}
