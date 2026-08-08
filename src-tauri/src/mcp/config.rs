//! User-facing MCP server configuration, persisted inside `AgentConfig`
//! (`config.json`). Secret env values are held out of this record and stored in
//! a sibling `mcp_secrets.json` (added in phase 3), referenced by key, so a
//! config export or bug report never carries tokens.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A configured MCP server. `id` is a user-chosen slug that becomes part of every
/// tool name (`mcp__{id}__{tool}`), so it is constrained to `[a-z0-9_]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub id: String,
    pub label: String,
    pub transport: McpTransport,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub trust: McpTrust,
    /// Tool names (as the server declares them, unqualified) the user has turned
    /// off. Absent from this list ⇒ enabled. The primary context-budget control.
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    /// If installed from the built-in catalog, the entry id it came from.
    #[serde(default)]
    pub catalog_id: Option<String>,
    /// Where this config came from. Cloud servers are web-authored (0048) and
    /// owned by the reconcile loop; Local servers are added on this device and
    /// the reconcile never touches them. Absent in older config.json ⇒ Local.
    #[serde(default)]
    pub origin: McpOrigin,
}

/// The authority for a server's configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpOrigin {
    /// Added on this device (Tools tab). Survives cloud reconcile untouched.
    #[default]
    Local,
    /// Web-authored and pulled via mcp-desired; managed by the reconcile loop.
    Cloud,
}

/// How the agent reaches a server. Only `Stdio` is honored in v1; the
/// `StreamableHttp` variant exists so configs and the UI can carry it forward,
/// and the manager rejects it with a clear message until the transport lands.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        /// Non-secret environment for the child. Secret values live in
        /// `mcp_secrets.json` and are merged in at spawn time.
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        cwd: Option<String>,
    },
    /// v2 seam — not yet implemented.
    StreamableHttp { url: String },
}

/// Whether a server's `readOnlyHint` annotations may be believed. Third-party
/// servers are untrusted by default, so every tool is treated as mutating and
/// gated; the user opts a server into `Trusted` to let read-only tools run
/// without the approval pause. Trust never grants availability in Plan/ReadOnly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTrust {
    #[default]
    Untrusted,
    Trusted,
}

impl McpServerConfig {
    /// Validate the slug and transport. Returns a user-facing message on failure.
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_slug(&self.id) {
            return Err(format!(
                "server id '{}' must be 1-24 characters of lowercase letters, digits, or underscore",
                self.id
            ));
        }
        if self.label.trim().is_empty() {
            return Err("server label must not be empty".into());
        }
        match &self.transport {
            McpTransport::Stdio { command, .. } => {
                if command.trim().is_empty() {
                    return Err("stdio transport requires a command".into());
                }
                Ok(())
            }
            McpTransport::StreamableHttp { .. } => Err(
                "streamable HTTP servers are not supported in this version; use a stdio command"
                    .into(),
            ),
        }
    }

    pub fn is_tool_enabled(&self, tool_name: &str) -> bool {
        !self.disabled_tools.iter().any(|t| t == tool_name)
    }
}

/// A slug usable in a tool name: 1-24 chars, `[a-z0-9_]`.
pub fn is_valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 24
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(id: &str) -> McpServerConfig {
        McpServerConfig {
            id: id.into(),
            label: "Test".into(),
            transport: McpTransport::Stdio {
                command: "echo".into(),
                args: vec![],
                env: BTreeMap::new(),
                cwd: None,
            },
            enabled: true,
            trust: McpTrust::Untrusted,
            disabled_tools: vec![],
            catalog_id: None,
            origin: McpOrigin::Local,
        }
    }

    #[test]
    fn slug_rules() {
        assert!(is_valid_slug("github"));
        assert!(is_valid_slug("my_server_2"));
        assert!(!is_valid_slug("Bad-Dash"));
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug(&"x".repeat(25)));
        assert!(!is_valid_slug("has space"));
    }

    #[test]
    fn validate_rejects_bad_slug_and_empty_command() {
        assert!(stdio("ok").validate().is_ok());
        assert!(stdio("Bad").validate().is_err());

        let mut c = stdio("ok");
        c.transport = McpTransport::Stdio {
            command: "  ".into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_http_transport_in_v1() {
        let mut c = stdio("ok");
        c.transport = McpTransport::StreamableHttp { url: "https://x/mcp".into() };
        assert!(c.validate().is_err());
    }

    #[test]
    fn per_tool_enablement() {
        let mut c = stdio("ok");
        c.disabled_tools = vec!["danger".into()];
        assert!(c.is_tool_enabled("safe"));
        assert!(!c.is_tool_enabled("danger"));
    }

    #[test]
    fn default_trust_is_untrusted() {
        assert_eq!(McpTrust::default(), McpTrust::Untrusted);
    }

    #[test]
    fn transport_tag_round_trips() {
        let c = stdio("ok");
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"kind\":\"stdio\""));
        let back: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.transport, c.transport);
    }
}
