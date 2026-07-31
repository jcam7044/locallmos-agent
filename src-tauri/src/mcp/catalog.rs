//! A curated, compiled-in catalog of vetted MCP servers for one-click install.
//!
//! Entries are version-pinned (never `@latest`) — installing one runs
//! network-fetched code in the user's account, so the exact command is fixed
//! here and shown to the user before it runs. Chosen to complement, not
//! duplicate, the built-in coding tools (file/search/edit, shell, git, web
//! fetch/search, loopback preview).

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::collections::BTreeMap;

use super::config::{McpServerConfig, McpTransport, McpTrust};

/// The launcher a catalog entry needs on PATH.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    /// Node's `npx` (implies `node`).
    Npx,
    /// Astral's `uvx` (implies `uv`).
    Uvx,
}

/// One value the user must supply to install an entry. If the entry's `args`
/// template references `{key}`, the value is substituted there (e.g. a path);
/// otherwise it is passed as an environment variable. `secret` values are stored
/// in `mcp_secrets.json`, never in `config.json`.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub required: bool,
    pub secret: bool,
    pub placeholder: &'static str,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub runtime: RuntimeKind,
    /// Full argument list after the launcher, with `{key}` placeholders.
    #[serde(skip)]
    pub args: &'static [&'static str],
    pub inputs: &'static [InputSpec],
    pub default_trust: McpTrust,
    pub caveat: Option<&'static str>,
}

impl CatalogEntry {
    pub fn launcher(&self) -> &'static str {
        match self.runtime {
            RuntimeKind::Npx => "npx",
            RuntimeKind::Uvx => "uvx",
        }
    }

    /// The exact command line that will run, for the confirmation dialog. Secret
    /// values are elided.
    pub fn preview_command(&self, inputs: &BTreeMap<String, String>) -> String {
        let mut parts = vec![self.launcher().to_string()];
        for arg in self.args {
            parts.push(substitute(arg, inputs, self.inputs));
        }
        parts.join(" ")
    }

    /// Build a runnable server config plus the map of secret env values to store
    /// separately. Fails if a required input is missing.
    pub fn to_config(
        &self,
        server_id: &str,
        inputs: &BTreeMap<String, String>,
    ) -> Result<(McpServerConfig, BTreeMap<String, String>)> {
        for spec in self.inputs {
            if spec.required && inputs.get(spec.key).map(String::as_str).unwrap_or("").is_empty() {
                return Err(anyhow!("'{}' is required", spec.label));
            }
        }

        let args: Vec<String> =
            self.args.iter().map(|a| substitute(a, inputs, self.inputs)).collect();

        let mut env = BTreeMap::new();
        let mut secrets = BTreeMap::new();
        for spec in self.inputs {
            // Placeholder inputs are consumed by the args template, not env.
            if self.args.iter().any(|a| a.contains(&format!("{{{}}}", spec.key))) {
                continue;
            }
            let Some(value) = inputs.get(spec.key).filter(|v| !v.is_empty()) else { continue };
            if spec.secret {
                secrets.insert(spec.key.to_string(), value.clone());
            } else {
                env.insert(spec.key.to_string(), value.clone());
            }
        }

        let config = McpServerConfig {
            id: server_id.to_string(),
            label: self.label.to_string(),
            transport: McpTransport::Stdio { command: self.launcher().to_string(), args, env, cwd: None },
            enabled: true,
            trust: self.default_trust,
            disabled_tools: vec![],
            catalog_id: Some(self.id.to_string()),
        };
        Ok((config, secrets))
    }
}

fn substitute(arg: &str, inputs: &BTreeMap<String, String>, specs: &[InputSpec]) -> String {
    let mut out = arg.to_string();
    for spec in specs {
        let token = format!("{{{}}}", spec.key);
        if out.contains(&token) {
            let value = inputs.get(spec.key).cloned().unwrap_or_default();
            out = out.replace(&token, &value);
        }
    }
    out
}

pub fn find(id: &str) -> Option<&'static CatalogEntry> {
    CATALOG.iter().find(|e| e.id == id)
}

/// Which launchers are available on this machine, probed once per Tools-tab open.
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeAvailability {
    pub node: bool,
    pub uv: bool,
}

pub fn detect_runtimes() -> RuntimeAvailability {
    RuntimeAvailability { node: has("npx"), uv: has("uvx") }
}

fn has(bin: &str) -> bool {
    // On Windows the launcher is a `.cmd` shim; probe through the shell.
    let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
        ("cmd", vec!["/C", bin, "--version"])
    } else {
        (bin, vec!["--version"])
    };
    std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// Version-pinned. Bump deliberately on release. `{…}` tokens map to `inputs`.
pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "context7",
        label: "Context7 (library docs)",
        description: "Up-to-date documentation and code examples for thousands of libraries — counters stale training data.",
        runtime: RuntimeKind::Npx,
        args: &["-y", "@upstash/context7-mcp@3.2.5"],
        inputs: &[],
        default_trust: McpTrust::Trusted,
        caveat: None,
    },
    CatalogEntry {
        id: "github",
        label: "GitHub",
        description: "Issues, pull requests, and cross-repository code search beyond the local workspace.",
        runtime: RuntimeKind::Npx,
        args: &["-y", "@modelcontextprotocol/server-github@2025.4.8"],
        inputs: &[InputSpec {
            key: "GITHUB_PERSONAL_ACCESS_TOKEN",
            label: "GitHub personal access token",
            required: true,
            secret: true,
            placeholder: "ghp_…",
        }],
        default_trust: McpTrust::Untrusted,
        caveat: Some("Can act on your GitHub account with the token's scopes. Prefer a fine-grained, read-only token."),
    },
    CatalogEntry {
        id: "sqlite",
        label: "SQLite",
        description: "Inspect schema and run read queries against a local SQLite database file.",
        runtime: RuntimeKind::Uvx,
        args: &["mcp-server-sqlite@2025.4.25", "--db-path", "{db_path}"],
        inputs: &[InputSpec {
            key: "db_path",
            label: "Database file path",
            required: true,
            secret: false,
            placeholder: "/path/to/app.db",
        }],
        default_trust: McpTrust::Untrusted,
        caveat: None,
    },
    CatalogEntry {
        id: "postgres",
        label: "Postgres",
        description: "Schema introspection and read-only SQL against a Postgres database.",
        runtime: RuntimeKind::Npx,
        args: &["-y", "@modelcontextprotocol/server-postgres@0.6.2", "{connection}"],
        inputs: &[InputSpec {
            key: "connection",
            label: "Connection string",
            required: true,
            secret: true,
            placeholder: "postgresql://user:pass@host/db",
        }],
        default_trust: McpTrust::Untrusted,
        caveat: Some("The connection string usually contains a password; it is stored in the protected secrets file."),
    },
    CatalogEntry {
        id: "playwright",
        label: "Playwright (browser)",
        description: "Real cross-browser automation and genuine screenshots — beyond the built-in loopback preview.",
        runtime: RuntimeKind::Npx,
        args: &["-y", "@executeautomation/playwright-mcp-server@1.0.12"],
        inputs: &[],
        default_trust: McpTrust::Untrusted,
        caveat: Some("Can navigate to arbitrary sites and download content. First run installs browser binaries."),
    },
    CatalogEntry {
        id: "memory",
        label: "Memory (knowledge graph)",
        description: "Persistent facts and relationships that survive across sessions.",
        runtime: RuntimeKind::Npx,
        args: &["-y", "@modelcontextprotocol/server-memory@2026.7.4"],
        inputs: &[],
        default_trust: McpTrust::Trusted,
        caveat: None,
    },
    CatalogEntry {
        id: "fetch",
        label: "Fetch (web pages)",
        description: "Fetch a URL and return clean Markdown — more robust extraction than the built-in web_fetch.",
        runtime: RuntimeKind::Uvx,
        args: &["mcp-server-fetch@2026.7.10"],
        inputs: &[],
        default_trust: McpTrust::Untrusted,
        caveat: None,
    },
    CatalogEntry {
        id: "filesystem",
        label: "Filesystem (outside workspace)",
        description: "Read and write files under a directory you choose — reaches beyond the coding workspace root.",
        runtime: RuntimeKind::Npx,
        args: &["-y", "@modelcontextprotocol/server-filesystem@2026.7.10", "{root}"],
        inputs: &[InputSpec {
            key: "root",
            label: "Allowed directory",
            required: true,
            secret: false,
            placeholder: "/home/you/projects",
        }],
        default_trust: McpTrust::Untrusted,
        caveat: Some("Grants file access outside the workspace confinement. Point it at a specific directory, and keep it Untrusted so writes still require approval."),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_has_unique_slug_and_pinned_package() {
        let mut seen = std::collections::HashSet::new();
        for e in CATALOG {
            assert!(seen.insert(e.id), "duplicate catalog id: {}", e.id);
            assert!(super::super::config::is_valid_slug(e.id), "bad slug: {}", e.id);
            // A pinned version means an `@x.y` somewhere in the args (uvx or npx).
            let has_pin = e.args.iter().any(|a| a.contains('@') && a.chars().any(|c| c.is_ascii_digit()));
            assert!(has_pin, "entry {} is not version-pinned: {:?}", e.id, e.args);
        }
    }

    #[test]
    fn placeholder_inputs_go_to_args_secrets_go_aside() {
        let entry = find("sqlite").unwrap();
        let mut inputs = BTreeMap::new();
        inputs.insert("db_path".to_string(), "/tmp/app.db".to_string());
        let (config, secrets) = entry.to_config("sqlite", &inputs).unwrap();
        assert!(secrets.is_empty());
        match config.transport {
            McpTransport::Stdio { command, args, env, .. } => {
                assert_eq!(command, "uvx");
                assert!(args.contains(&"/tmp/app.db".to_string()));
                assert!(env.is_empty());
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn secret_inputs_are_separated_from_config() {
        let entry = find("github").unwrap();
        let mut inputs = BTreeMap::new();
        inputs.insert("GITHUB_PERSONAL_ACCESS_TOKEN".to_string(), "ghp_secret".to_string());
        let (config, secrets) = entry.to_config("gh", &inputs).unwrap();
        assert_eq!(secrets.get("GITHUB_PERSONAL_ACCESS_TOKEN").map(String::as_str), Some("ghp_secret"));
        // The token must NOT be embedded in the persisted config.
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("ghp_secret"), "secret leaked into config: {json}");
    }

    #[test]
    fn missing_required_input_is_rejected() {
        let entry = find("github").unwrap();
        assert!(entry.to_config("gh", &BTreeMap::new()).is_err());
    }

    #[test]
    fn preview_command_shows_launcher_and_package() {
        let entry = find("context7").unwrap();
        let cmd = entry.preview_command(&BTreeMap::new());
        assert!(cmd.starts_with("npx "));
        assert!(cmd.contains("@upstash/context7-mcp@3.2.5"));
    }
}
