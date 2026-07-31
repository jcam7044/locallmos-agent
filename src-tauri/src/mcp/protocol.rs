//! Minimal JSON-RPC 2.0 + Model Context Protocol wire types.
//!
//! We hand-roll the small slice of MCP the agent needs — `initialize`,
//! `notifications/initialized`, `tools/list` (cursor-paginated), `tools/call`,
//! and the `notifications/tools/list_changed` signal — rather than take a heavy,
//! very new SDK dependency into a signed, self-updating binary. The surface is
//! spec-frozen (MCP 2025-06-18) and small enough to test exhaustively here.
//!
//! Transport framing is newline-delimited JSON: each message is one compact JSON
//! object on its own line, with no embedded newlines (guaranteed by
//! `serde_json::to_string`, which never emits raw control characters).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The MCP protocol revision this client speaks. Sent in `initialize`; the
/// server echoes a (possibly older) version it agrees to.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub const CLIENT_NAME: &str = "locallmos-agent";

/// A JSON-RPC request or notification we send. `id: None` makes it a
/// notification (no response expected), per the JSON-RPC 2.0 spec.
#[derive(Debug, Clone, Serialize)]
pub struct Outgoing {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Outgoing {
    pub fn request(id: i64, method: &str, params: Option<Value>) -> Self {
        Self { jsonrpc: "2.0", id: Some(id), method: method.to_string(), params }
    }

    pub fn notification(method: &str, params: Option<Value>) -> Self {
        Self { jsonrpc: "2.0", id: None, method: method.to_string(), params }
    }

    /// Serialize to a single newline-terminated frame ready to write to stdin.
    pub fn to_frame(&self) -> String {
        // to_string never emits a raw newline, so the frame invariant holds.
        format!("{}\n", serde_json::to_string(self).expect("Outgoing is always serializable"))
    }
}

/// A decoded inbound line from the server. Responses carry our request id;
/// notifications and server-initiated requests do not. Some carried fields
/// (notification/request params) are retained for completeness and diagnostics
/// even though the current client only branches on `method`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Incoming {
    /// A response to a request we sent, keyed by the id we chose.
    Response { id: i64, result: Result<Value, RpcError> },
    /// A server notification (e.g. `notifications/tools/list_changed`).
    Notification { method: String, params: Value },
    /// A server-initiated request (e.g. `sampling/createMessage`). We do not
    /// implement these in v1; the transport replies method-not-found by id.
    ServerRequest { id: Value, method: String, params: Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

/// JSON-RPC "method not found", used to reject unsupported server requests.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// Parse one inbound frame. A malformed line yields `None` (the caller logs and
/// skips it) rather than tearing down an otherwise-healthy connection.
pub fn parse_incoming(line: &str) -> Option<Incoming> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    let obj = v.as_object()?;
    let has_method = obj.contains_key("method");
    let id = obj.get("id");

    match (has_method, id) {
        // Response: no method, has id (numeric — our ids are always i64).
        (false, Some(id)) => {
            let id = id.as_i64()?;
            if let Some(err) = obj.get("error") {
                let error: RpcError = serde_json::from_value(err.clone()).ok()?;
                Some(Incoming::Response { id, result: Err(error) })
            } else {
                let result = obj.get("result").cloned().unwrap_or(Value::Null);
                Some(Incoming::Response { id, result: Ok(result) })
            }
        }
        // Server-initiated request: has method and id.
        (true, Some(id)) => Some(Incoming::ServerRequest {
            id: id.clone(),
            method: obj.get("method")?.as_str()?.to_string(),
            params: obj.get("params").cloned().unwrap_or(Value::Null),
        }),
        // Notification: has method, no id.
        (true, None) => Some(Incoming::Notification {
            method: obj.get("method")?.as_str()?.to_string(),
            params: obj.get("params").cloned().unwrap_or(Value::Null),
        }),
        (false, None) => None,
    }
}

/// Build the `initialize` request params advertising our (empty) capabilities.
pub fn initialize_params(client_version: &str) -> Value {
    serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": CLIENT_NAME, "version": client_version },
    })
}

// ---- tools/list ----

/// One tool as declared by a server. `input_schema` is passed through opaquely
/// to the model; `annotations` inform (but never solely decide) the approval
/// gate — the spec marks them untrusted hints.
#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(default)]
    pub annotations: ToolAnnotations,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolAnnotations {
    #[serde(default, rename = "readOnlyHint")]
    pub read_only_hint: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolsListResult {
    #[serde(default)]
    pub tools: Vec<McpTool>,
    #[serde(default, rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

/// Params for a paginated `tools/list` call.
pub fn tools_list_params(cursor: Option<&str>) -> Option<Value> {
    cursor.map(|c| serde_json::json!({ "cursor": c }))
}

// ---- tools/call ----

pub fn tools_call_params(name: &str, arguments: &Value) -> Value {
    // MCP requires an object for arguments; coerce a null/absent to `{}`.
    let arguments = if arguments.is_object() { arguments.clone() } else { serde_json::json!({}) };
    serde_json::json!({ "name": name, "arguments": arguments })
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallToolResult {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

/// A content block in a tool result. We render text natively and summarize
/// other block kinds — local models consume text, not inline images. Non-text
/// payloads are captured but intentionally not surfaced to the model.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
#[allow(dead_code)]
pub enum ContentBlock {
    Text { text: String },
    Image { #[serde(default, rename = "mimeType")] mime_type: String },
    Audio { #[serde(default, rename = "mimeType")] mime_type: String },
    Resource(#[serde(default)] Value),
    #[serde(other)]
    Other,
}

impl CallToolResult {
    /// Flatten the content blocks into the model-visible string.
    pub fn to_model_text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            match block {
                ContentBlock::Text { text } => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
                ContentBlock::Image { mime_type } => {
                    out.push_str(&format!("\n[image content: {mime_type}]"));
                }
                ContentBlock::Audio { mime_type } => {
                    out.push_str(&format!("\n[audio content: {mime_type}]"));
                }
                ContentBlock::Resource(_) => out.push_str("\n[embedded resource]"),
                ContentBlock::Other => out.push_str("\n[unsupported content block]"),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frame_is_single_line_and_newline_terminated() {
        let frame = Outgoing::request(1, "tools/list", None).to_frame();
        assert!(frame.ends_with('\n'));
        assert_eq!(frame.matches('\n').count(), 1, "frame must not contain embedded newlines");
    }

    #[test]
    fn notification_omits_id() {
        let frame = Outgoing::notification("notifications/initialized", None).to_frame();
        assert!(!frame.contains("\"id\""), "notifications must not carry an id: {frame}");
    }

    #[test]
    fn parses_ok_response() {
        let line = r#"{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}"#;
        match parse_incoming(line).unwrap() {
            Incoming::Response { id, result } => {
                assert_eq!(id, 7);
                assert!(result.is_ok());
            }
            other => panic!("expected response, got {other:?}"),
        }
    }

    #[test]
    fn parses_error_response() {
        let line = r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32601,"message":"nope"}}"#;
        match parse_incoming(line).unwrap() {
            Incoming::Response { id, result: Err(e) } => {
                assert_eq!(id, 3);
                assert_eq!(e.code, METHOD_NOT_FOUND);
            }
            other => panic!("expected error response, got {other:?}"),
        }
    }

    #[test]
    fn parses_notification_and_server_request() {
        let n = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;
        assert!(matches!(parse_incoming(n).unwrap(), Incoming::Notification { .. }));

        let r = r#"{"jsonrpc":"2.0","id":"abc","method":"sampling/createMessage","params":{}}"#;
        match parse_incoming(r).unwrap() {
            Incoming::ServerRequest { method, .. } => assert_eq!(method, "sampling/createMessage"),
            other => panic!("expected server request, got {other:?}"),
        }
    }

    #[test]
    fn malformed_line_is_none_not_panic() {
        assert!(parse_incoming("this is not json").is_none());
        assert!(parse_incoming("").is_none());
        assert!(parse_incoming("{}").is_none());
    }

    #[test]
    fn tools_list_deserializes_with_pagination_and_annotations() {
        let v = json!({
            "tools": [
                {
                    "name": "read_query",
                    "description": "run a select",
                    "inputSchema": {"type": "object"},
                    "annotations": {"readOnlyHint": true}
                },
                { "name": "write_query" }
            ],
            "nextCursor": "page2"
        });
        let parsed: ToolsListResult = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.tools.len(), 2);
        assert!(parsed.tools[0].annotations.read_only_hint);
        assert!(!parsed.tools[1].annotations.read_only_hint);
        assert_eq!(parsed.next_cursor.as_deref(), Some("page2"));
    }

    #[test]
    fn call_result_flattens_text_and_labels_other_blocks() {
        let v = json!({
            "content": [
                {"type": "text", "text": "line one"},
                {"type": "text", "text": "line two"},
                {"type": "image", "mimeType": "image/png"}
            ]
        });
        let result: CallToolResult = serde_json::from_value(v).unwrap();
        assert!(!result.is_error);
        let text = result.to_model_text();
        assert!(text.contains("line one"));
        assert!(text.contains("line two"));
        assert!(text.contains("[image content: image/png]"));
    }

    #[test]
    fn call_params_coerce_non_object_arguments() {
        let p = tools_call_params("x", &Value::Null);
        assert!(p["arguments"].is_object());
    }
}
