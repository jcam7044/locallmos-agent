//! Model Context Protocol (MCP) client support.
//!
//! Users connect stdio MCP servers (curated catalog or bring-your-own); the
//! local models in both the chat and coding surfaces gain those servers' tools.
//! This module owns the server processes and exposes two things to the rest of
//! the app: an immutable per-turn [`McpSnapshot`] of tool definitions (for the
//! prompt and token accounting) and [`McpManager::call_tool`] to execute one.
//!
//! Design invariants:
//! - The snapshot is frozen for the duration of a turn. A server dying or
//!   restarting mid-turn never changes the tool list the model was shown; a call
//!   to a dead server returns a normal failed result, and the turn continues.
//! - Third-party tools are mutating by default. Only a server the user marked
//!   `Trusted` may have its `readOnlyHint` believed, and even then trust only
//!   downgrades the approval pause — it never grants availability in Plan or
//!   ReadOnly mode. That gating lives in `coding::mod` (phase 2); the snapshot
//!   carries the precomputed `mutating` flag it needs.

pub mod catalog;
pub mod config;
mod protocol;
mod secrets;
mod stdio;

pub use secrets::{keys_for as secret_keys_for, remove_server as remove_secrets, set as set_secret};

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub use config::{McpOrigin, McpServerConfig, McpTransport, McpTrust};

use protocol::{CallToolResult, McpTool};
use stdio::StdioClient;

/// Every MCP tool name is `mcp__{server_id}__{tool_name}`. The prefix is the
/// dispatch and gating discriminator throughout the tool loops.
pub const MCP_PREFIX: &str = "mcp__";

/// The user-facing default is deliberately conservative, but can be raised for
/// large-context models. The hard ceilings are denial-of-service protection for
/// malformed servers and are not presented as normal product limits.
pub const DEFAULT_MCP_TOOL_LIMIT: usize = 48;
pub const MAX_MCP_TOOL_LIMIT: usize = 256;
const EMERGENCY_MAX_TOOLS_PER_SERVER: usize = 128;

const CALL_TIMEOUT: Duration = Duration::from_secs(120);
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// Minimum delay between reconnect attempts after a failure (exponential to a cap).
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Build the qualified tool name exposed to the model.
pub fn qualified_name(server_id: &str, tool: &str) -> String {
    format!("{MCP_PREFIX}{server_id}__{tool}")
}

/// One tool as offered to the model, already qualified and policy-annotated.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDef {
    pub server_id: String,
    pub tool_name: String,
    pub qualified: String,
    pub description: String,
    pub parameters: Value,
    /// The server's (untrusted) hint, surfaced for the UI only.
    pub read_only_hint: bool,
    /// Effective classification used by the approval gate: `true` unless the
    /// server is `Trusted` *and* declared the tool read-only.
    pub mutating: bool,
}

impl McpToolDef {
    /// The Ollama/OpenAI `{type:"function",function:{…}}` schema passed to the runtime.
    pub fn to_ollama_def(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.qualified,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

/// Immutable per-turn view of all enabled MCP tools across all running servers.
#[derive(Debug, Default)]
pub struct McpSnapshot {
    pub tools: Vec<McpToolDef>,
    /// How many tools were dropped by the caps, for the UI to report.
    pub truncated: usize,
}

impl McpSnapshot {
    pub fn empty() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn find(&self, qualified: &str) -> Option<&McpToolDef> {
        self.tools.iter().find(|t| t.qualified == qualified)
    }

    /// Ollama-shaped tool defs for the runtime `tools` array.
    pub fn ollama_defs(&self) -> Vec<Value> {
        self.tools.iter().map(McpToolDef::to_ollama_def).collect()
    }
}

/// The result of one `tools/call`, decoupled from `coding::ToolRun` so this
/// module has no dependency on the coding harness.
#[derive(Debug)]
pub struct McpCallOutcome {
    pub server_id: String,
    pub tool_name: String,
    pub text: String,
    pub is_error: bool,
}

/// Live status of one configured server, for commands and the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpStatus {
    Stopped,
    Starting,
    Running,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub trust: McpTrust,
    pub status: McpStatus,
    pub tool_count: usize,
    pub last_error: Option<String>,
}

/// Internal per-server runtime state.
struct ServerHandle {
    config: McpServerConfig,
    status: McpStatus,
    client: Option<Arc<StdioClient>>,
    /// Last successful `tools/list`.
    tools: Vec<McpTool>,
    last_error: Option<String>,
    /// Reconnect bookkeeping: consecutive failures and the earliest next attempt.
    failures: u32,
    next_attempt: Option<Instant>,
}

impl ServerHandle {
    fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            status: McpStatus::Stopped,
            client: None,
            tools: Vec::new(),
            last_error: None,
            failures: 0,
            next_attempt: None,
        }
    }

    fn status_view(&self) -> McpServerStatus {
        McpServerStatus {
            id: self.config.id.clone(),
            label: self.config.label.clone(),
            enabled: self.config.enabled,
            trust: self.config.trust,
            status: self.status,
            tool_count: self.enabled_tool_count(),
            last_error: self.last_error.clone(),
        }
    }

    fn enabled_tool_count(&self) -> usize {
        self.tools.iter().filter(|t| self.config.is_tool_enabled(&t.name)).count()
    }
}

/// Owns MCP server processes and the current tool snapshot. One instance lives on
/// `AppState`.
pub struct McpManager {
    servers: Mutex<HashMap<String, ServerHandle>>,
    /// Read by `build_context` and `calculate_context` so the preflight token
    /// count and the turn itself see the same tools.
    current: RwLock<Arc<McpSnapshot>>,
    tool_limit: AtomicUsize,
    client_version: String,
}

impl McpManager {
    pub fn new(client_version: impl Into<String>) -> Arc<Self> {
        Self::new_with_tool_limit(client_version, DEFAULT_MCP_TOOL_LIMIT)
    }

    pub fn new_with_tool_limit(client_version: impl Into<String>, tool_limit: usize) -> Arc<Self> {
        Arc::new(Self {
            servers: Mutex::new(HashMap::new()),
            current: RwLock::new(McpSnapshot::empty()),
            tool_limit: AtomicUsize::new(tool_limit.clamp(1, MAX_MCP_TOOL_LIMIT)),
            client_version: client_version.into(),
        })
    }

    pub fn tool_limit(&self) -> usize {
        self.tool_limit.load(Ordering::Relaxed)
    }

    pub async fn set_tool_limit(&self, limit: usize) {
        self.tool_limit.store(limit.clamp(1, MAX_MCP_TOOL_LIMIT), Ordering::Relaxed);
        self.rebuild_snapshot().await;
    }

    /// The current frozen snapshot. Cheap and lock-light; safe to call per turn.
    pub fn snapshot(&self) -> Arc<McpSnapshot> {
        self.current.read().expect("snapshot lock poisoned").clone()
    }

    /// Replace the configured server set (e.g. loaded from `config.json` at
    /// startup, or after a CRUD edit). Servers whose config is unchanged keep
    /// their running client; removed servers are stopped.
    pub async fn set_configs(&self, configs: Vec<McpServerConfig>) {
        let mut servers = self.servers.lock().await;
        let new_ids: std::collections::HashSet<&str> =
            configs.iter().map(|c| c.id.as_str()).collect();

        // Stop and drop servers no longer configured.
        let removed: Vec<String> =
            servers.keys().filter(|id| !new_ids.contains(id.as_str())).cloned().collect();
        for id in removed {
            if let Some(mut handle) = servers.remove(&id) {
                if let Some(client) = handle.client.take() {
                    client.shutdown().await;
                }
            }
        }

        for config in configs {
            match servers.get_mut(&config.id) {
                Some(handle) => {
                    // Reuse the running client only if the transport is identical;
                    // otherwise a config change should relaunch.
                    if handle.config.transport != config.transport {
                        if let Some(client) = handle.client.take() {
                            client.shutdown().await;
                        }
                        handle.status = McpStatus::Stopped;
                        handle.tools.clear();
                    }
                    handle.config = config;
                }
                None => {
                    servers.insert(config.id.clone(), ServerHandle::new(config));
                }
            }
        }
        drop(servers);
        self.rebuild_snapshot().await;
    }

    pub async fn statuses(&self) -> Vec<McpServerStatus> {
        let servers = self.servers.lock().await;
        // Deterministic order for the UI.
        let mut list: Vec<McpServerStatus> = servers.values().map(ServerHandle::status_view).collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }

    /// Every tool currently reported by running servers, before user disablement
    /// and the model-facing limit. Used by the Tools tab so excluded tools remain
    /// inspectable and can be enabled/disabled deliberately.
    pub async fn inventory(&self) -> Vec<McpToolDef> {
        let servers = self.servers.lock().await;
        let mut ids: Vec<&String> = servers.keys().collect();
        ids.sort();
        let mut tools = Vec::new();
        for id in ids {
            let handle = &servers[id];
            if !handle.config.enabled || handle.status != McpStatus::Running {
                continue;
            }
            tools.extend(
                handle
                    .tools
                    .iter()
                    .take(EMERGENCY_MAX_TOOLS_PER_SERVER)
                    .map(|tool| tool_def(handle, tool)),
            );
        }
        tools
    }

    /// Build the `{ servers: [...] }` payload for the `mcp-sync` edge function:
    /// every configured server with its status and its currently-known enabled
    /// tools (from the frozen snapshot, i.e. running servers). Tools are keyed by
    /// their qualified name so they match what the model will call.
    pub async fn advertise_payload(&self) -> Value {
        let statuses = self.statuses().await;
        let snapshot = self.snapshot();
        let servers: Vec<Value> = statuses
            .iter()
            .map(|s| {
                let tools: Vec<Value> = snapshot
                    .tools
                    .iter()
                    .filter(|t| t.server_id == s.id)
                    .map(|t| {
                        serde_json::json!({
                            "name": t.qualified,
                            "description": t.description,
                            "parameters": t.parameters,
                            "mutating": t.mutating,
                            "enabled": true,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "serverId": s.id,
                    "label": s.label,
                    "status": serde_json::to_value(s.status).unwrap_or(Value::Null),
                    "tools": tools,
                })
            })
            .collect();
        serde_json::json!({ "servers": servers })
    }

    pub async fn server_logs(&self, id: &str) -> Result<String> {
        let servers = self.servers.lock().await;
        let handle = servers.get(id).ok_or_else(|| anyhow!("no such server: {id}"))?;
        Ok(handle
            .client
            .as_ref()
            .map(|c| c.log_tail())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "(no server output)".into()))
    }

    /// Start (or restart) a server: connect, list its tools, rebuild the snapshot.
    pub async fn start(&self, id: &str) -> Result<()> {
        self.connect_locked(id).await?;
        self.rebuild_snapshot().await;
        Ok(())
    }

    pub async fn stop(&self, id: &str) -> Result<()> {
        {
            let mut servers = self.servers.lock().await;
            let handle = servers.get_mut(id).ok_or_else(|| anyhow!("no such server: {id}"))?;
            if let Some(client) = handle.client.take() {
                client.shutdown().await;
            }
            handle.status = McpStatus::Stopped;
            handle.tools.clear();
            handle.failures = 0;
            handle.next_attempt = None;
        }
        self.rebuild_snapshot().await;
        Ok(())
    }

    /// Stop every server. Wired to app exit.
    pub async fn stop_all(&self) {
        let mut servers = self.servers.lock().await;
        for handle in servers.values_mut() {
            if let Some(client) = handle.client.take() {
                client.shutdown().await;
            }
            handle.status = McpStatus::Stopped;
        }
    }

    /// Ensure every enabled server is running (lazy start before a turn). Errors
    /// are recorded per-server, never propagated — one dead server must not block
    /// a turn that can still use the others.
    pub async fn ensure_enabled_started(&self) {
        let ids: Vec<String> = {
            let servers = self.servers.lock().await;
            servers
                .values()
                .filter(|h| h.config.enabled && h.status != McpStatus::Running)
                .map(|h| h.config.id.clone())
                .collect()
        };
        let mut changed = false;
        for id in ids {
            if self.connect_locked(&id).await.is_ok() {
                changed = true;
            }
        }
        // Honor any `tools/list_changed` a running server sent since the last
        // check, re-listing its tools so the next snapshot reflects them.
        if self.refresh_changed_tools().await {
            changed = true;
        }
        if changed {
            self.rebuild_snapshot().await;
        }
    }

    /// Re-list tools for any running server that signaled `tools/list_changed`.
    /// Returns whether any server's tool set was refreshed.
    async fn refresh_changed_tools(&self) -> bool {
        let dirty: Vec<(String, Arc<StdioClient>)> = {
            let servers = self.servers.lock().await;
            servers
                .values()
                .filter_map(|h| {
                    let client = h.client.as_ref()?;
                    client.take_tools_changed().then(|| (h.config.id.clone(), client.clone()))
                })
                .collect()
        };
        if dirty.is_empty() {
            return false;
        }
        for (id, client) in dirty {
            if let Ok(tools) = stdio::list_all_tools(&client, LIST_TIMEOUT).await {
                if let Some(handle) = self.servers.lock().await.get_mut(&id) {
                    handle.tools = tools;
                }
            }
        }
        true
    }

    /// Execute one tool by its qualified name. The owning server is resolved by
    /// matching a configured server-id prefix (`mcp__<id>__`), so this works both
    /// for a locally-offered tool and for a cloud-driven turn whose tools come
    /// from the control-plane snapshot rather than the local one.
    pub async fn call_tool(&self, qualified: &str, args: &Value) -> Result<McpCallOutcome> {
        let (server_id, tool_name) = self.resolve_server(qualified).await?;
        let client = self.live_client(&server_id).await?;
        let params = protocol::tools_call_params(&tool_name, args);
        let value = client.request("tools/call", Some(params), CALL_TIMEOUT).await?;
        let result: CallToolResult =
            serde_json::from_value(value).map_err(|e| anyhow!("malformed tool result: {e}"))?;
        Ok(McpCallOutcome {
            server_id,
            tool_name,
            text: result.to_model_text(),
            is_error: result.is_error,
        })
    }

    /// Split a qualified name (`mcp__<server>__<tool>`) into (server_id, tool)
    /// by matching a configured server's id prefix. Matching against the real
    /// configured ids handles server slugs that themselves contain underscores.
    async fn resolve_server(&self, qualified: &str) -> Result<(String, String)> {
        let servers = self.servers.lock().await;
        // Prefer the longest matching id so "a" never shadows "a_b".
        let mut best: Option<(String, String)> = None;
        for id in servers.keys() {
            let prefix = format!("{MCP_PREFIX}{id}__");
            if let Some(tool) = qualified.strip_prefix(&prefix) {
                let better = best.as_ref().map(|(cur, _)| id.len() > cur.len()).unwrap_or(true);
                if better {
                    best = Some((id.clone(), tool.to_string()));
                }
            }
        }
        best.ok_or_else(|| anyhow!("{qualified} does not map to a configured MCP server"))
    }

    /// Fetch a running client for `server_id`, reconnecting if it died. Returns an
    /// error (never panics) if the server can't be reached — the caller turns
    /// that into a failed tool result.
    async fn live_client(&self, server_id: &str) -> Result<Arc<StdioClient>> {
        {
            let servers = self.servers.lock().await;
            if let Some(handle) = servers.get(server_id) {
                if let Some(client) = &handle.client {
                    if client.is_alive().await {
                        return Ok(client.clone());
                    }
                }
            } else {
                return Err(anyhow!("server '{server_id}' is not configured"));
            }
        }
        // Not alive — attempt a reconnect (respects backoff).
        self.connect_locked(server_id).await?;
        let servers = self.servers.lock().await;
        servers
            .get(server_id)
            .and_then(|h| h.client.clone())
            .ok_or_else(|| anyhow!("server '{server_id}' is not running"))
    }

    /// Connect a single server and list its tools, updating its handle. Honors the
    /// reconnect backoff. Holds no lock across the (slow) spawn/handshake.
    async fn connect_locked(&self, id: &str) -> Result<()> {
        // Snapshot the config and check backoff under the lock.
        let config = {
            let mut servers = self.servers.lock().await;
            let handle = servers.get_mut(id).ok_or_else(|| anyhow!("no such server: {id}"))?;
            if let Some(client) = &handle.client {
                if client.is_alive().await {
                    return Ok(()); // already running
                }
                handle.client = None;
            }
            if let Some(at) = handle.next_attempt {
                if Instant::now() < at {
                    return Err(anyhow!(
                        "server '{id}' is backing off after {} failed attempt(s)",
                        handle.failures
                    ));
                }
            }
            handle.status = McpStatus::Starting;
            handle.config.clone()
        };

        let (command, args, mut env, cwd) = match &config.transport {
            McpTransport::Stdio { command, args, env, cwd } => {
                (command.clone(), args.clone(), env.clone(), cwd.clone())
            }
            McpTransport::StreamableHttp { .. } => {
                self.record_failure(id, "streamable HTTP transport is not supported yet").await;
                return Err(anyhow!("streamable HTTP transport is not supported yet"));
            }
        };
        // Merge the protected secret env values over the non-secret config env.
        env.extend(secrets::env_for(id));

        // Spawn + handshake without holding the servers lock.
        let connect = StdioClient::connect(
            &command,
            &args,
            &env,
            cwd.as_deref(),
            &self.client_version,
        )
        .await;

        match connect {
            Ok(client) => {
                let tools = stdio::list_all_tools(&client, LIST_TIMEOUT).await.unwrap_or_default();
                let mut servers = self.servers.lock().await;
                if let Some(handle) = servers.get_mut(id) {
                    handle.client = Some(client);
                    handle.tools = tools;
                    handle.status = McpStatus::Running;
                    handle.last_error = None;
                    handle.failures = 0;
                    handle.next_attempt = None;
                }
                Ok(())
            }
            Err(e) => {
                self.record_failure(id, &e.to_string()).await;
                Err(e)
            }
        }
    }

    async fn record_failure(&self, id: &str, message: &str) {
        let mut servers = self.servers.lock().await;
        if let Some(handle) = servers.get_mut(id) {
            handle.status = McpStatus::Failed;
            handle.last_error = Some(message.to_string());
            handle.failures = handle.failures.saturating_add(1);
            // Exponential backoff, capped. 1s, 2s, 4s … up to 60s.
            let shift = handle.failures.saturating_sub(1).min(6);
            let delay = BACKOFF_BASE.saturating_mul(1u32 << shift).min(BACKOFF_MAX);
            handle.next_attempt = Some(Instant::now() + delay);
        }
    }

    /// Recompute the model-facing snapshot. Tools are selected round-robin by
    /// server so one large catalog cannot starve every other connected server.
    async fn rebuild_snapshot(&self) {
        let servers = self.servers.lock().await;
        let mut ids: Vec<&String> = servers.keys().collect();
        ids.sort();
        let mut candidates: Vec<Vec<McpToolDef>> = Vec::new();
        for id in ids {
            let handle = &servers[id];
            if !handle.config.enabled || handle.status != McpStatus::Running {
                continue;
            }
            candidates.push(
                handle
                    .tools
                    .iter()
                    .filter(|tool| handle.config.is_tool_enabled(&tool.name))
                    .take(EMERGENCY_MAX_TOOLS_PER_SERVER)
                    .map(|tool| tool_def(handle, tool))
                    .collect(),
            );
        }
        let available: usize = candidates.iter().map(Vec::len).sum();
        let limit = self.tool_limit().min(MAX_MCP_TOOL_LIMIT);
        let mut tools = Vec::with_capacity(available.min(limit));
        let mut index = 0usize;
        while tools.len() < limit {
            let mut added = false;
            for server in &candidates {
                if let Some(tool) = server.get(index) {
                    tools.push(tool.clone());
                    added = true;
                    if tools.len() == limit {
                        break;
                    }
                }
            }
            if !added {
                break;
            }
            index += 1;
        }
        let truncated = available.saturating_sub(tools.len());
        let snapshot = Arc::new(McpSnapshot { tools, truncated });
        *self.current.write().expect("snapshot lock poisoned") = snapshot;
    }
}

fn tool_def(handle: &ServerHandle, tool: &McpTool) -> McpToolDef {
    let read_only = tool.annotations.read_only_hint;
    McpToolDef {
        server_id: handle.config.id.clone(),
        tool_name: tool.name.clone(),
        qualified: qualified_name(&handle.config.id, &tool.name),
        description: tool.description.clone(),
        parameters: normalize_schema(&tool.input_schema),
        read_only_hint: read_only,
        mutating: !(handle.config.trust == McpTrust::Trusted && read_only),
    }
}

/// Conservative schema cost shown in the UI. This mirrors the runtime fallback
/// estimator: structured JSON averages roughly three UTF-8 bytes per token.
pub fn estimated_schema_tokens(tool: &McpToolDef) -> u32 {
    serde_json::to_vec(&tool.to_ollama_def())
        .map(|bytes| (bytes.len() as u64).div_ceil(3).min(u32::MAX as u64) as u32)
        .unwrap_or(0)
}

/// Ensure the parameters are a JSON Schema object; some servers omit it. The
/// runtime and prompt manifest both expect an object.
fn normalize_schema(schema: &Value) -> Value {
    if schema.is_object() {
        schema.clone()
    } else {
        serde_json::json!({ "type": "object" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn tool(name: &str, read_only: bool) -> McpTool {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "description": format!("the {name} tool"),
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": read_only}
        }))
        .unwrap()
    }

    fn cfg(id: &str, trust: McpTrust) -> McpServerConfig {
        McpServerConfig {
            id: id.into(),
            label: id.into(),
            transport: McpTransport::Stdio {
                command: "true".into(),
                args: vec![],
                env: BTreeMap::new(),
                cwd: None,
            },
            enabled: true,
            trust,
            disabled_tools: vec![],
            catalog_id: None,
            origin: McpOrigin::Local,
        }
    }

    /// Directly drive rebuild_snapshot by planting a running handle, since we
    /// don't spawn a real process in unit tests.
    async fn manager_with(handles: Vec<ServerHandle>) -> Arc<McpManager> {
        let m = McpManager::new("test");
        {
            let mut servers = m.servers.lock().await;
            for h in handles {
                servers.insert(h.config.id.clone(), h);
            }
        }
        m.rebuild_snapshot().await;
        m
    }

    fn running(config: McpServerConfig, tools: Vec<McpTool>) -> ServerHandle {
        let mut h = ServerHandle::new(config);
        h.status = McpStatus::Running;
        h.tools = tools;
        h
    }

    #[tokio::test]
    async fn qualified_names_and_untrusted_is_mutating() {
        let m = manager_with(vec![running(
            cfg("db", McpTrust::Untrusted),
            vec![tool("read_query", true), tool("write_query", false)],
        )])
        .await;
        let snap = m.snapshot();
        assert_eq!(snap.tools.len(), 2);
        let read = snap.find("mcp__db__read_query").unwrap();
        // Untrusted: even a read-only-hinted tool is treated as mutating.
        assert!(read.read_only_hint);
        assert!(read.mutating);
    }

    #[tokio::test]
    async fn trusted_read_only_is_not_mutating() {
        let m = manager_with(vec![running(
            cfg("db", McpTrust::Trusted),
            vec![tool("read_query", true), tool("write_query", false)],
        )])
        .await;
        let snap = m.snapshot();
        assert!(!snap.find("mcp__db__read_query").unwrap().mutating);
        // A non-read-only tool stays mutating even on a trusted server.
        assert!(snap.find("mcp__db__write_query").unwrap().mutating);
    }

    #[tokio::test]
    async fn disabled_and_not_running_servers_contribute_nothing() {
        let mut disabled = running(cfg("a", McpTrust::Untrusted), vec![tool("x", false)]);
        disabled.config.enabled = false;
        let mut stopped = ServerHandle::new(cfg("b", McpTrust::Untrusted));
        stopped.tools = vec![tool("y", false)]; // present but status Stopped
        let m = manager_with(vec![disabled, stopped]).await;
        assert!(m.snapshot().is_empty());
    }

    #[tokio::test]
    async fn per_tool_disable_removes_only_that_tool() {
        let mut c = cfg("db", McpTrust::Untrusted);
        c.disabled_tools = vec!["write_query".into()];
        let m = manager_with(vec![running(c, vec![tool("read_query", true), tool("write_query", false)])]).await;
        let snap = m.snapshot();
        assert_eq!(snap.tools.len(), 1);
        assert!(snap.find("mcp__db__read_query").is_some());
        assert!(snap.find("mcp__db__write_query").is_none());
    }

    #[tokio::test]
    async fn total_cap_truncates_deterministically() {
        let many: Vec<McpTool> = (0..34)
            .map(|i| tool(&format!("t{i:02}"), false))
            .collect();
        let a = running(cfg("aaa", McpTrust::Untrusted), many.clone());
        let b = running(cfg("bbb", McpTrust::Untrusted), many);
        let m = manager_with(vec![a, b]).await;
        let snap = m.snapshot();
        assert_eq!(snap.tools.len(), DEFAULT_MCP_TOOL_LIMIT);
        assert!(snap.truncated > 0);
        // Round-robin selection gives equally sized servers an equal share.
        let from_a = snap.tools.iter().filter(|t| t.server_id == "aaa").count();
        let from_b = snap.tools.iter().filter(|t| t.server_id == "bbb").count();
        assert_eq!(from_a, DEFAULT_MCP_TOOL_LIMIT / 2);
        assert_eq!(from_b, DEFAULT_MCP_TOOL_LIMIT / 2);
    }

    #[tokio::test]
    async fn total_cap_across_many_small_servers() {
        // Three servers of 20 tools each = 60 declared; the default clamps to 48.
        let handles: Vec<ServerHandle> = ["s1", "s2", "s3"]
            .iter()
            .map(|id| {
                let tools: Vec<McpTool> =
                    (0..20).map(|i| tool(&format!("t{i:02}"), false)).collect();
                running(cfg(id, McpTrust::Untrusted), tools)
            })
            .collect();
        let m = manager_with(handles).await;
        let snap = m.snapshot();
        assert_eq!(snap.tools.len(), DEFAULT_MCP_TOOL_LIMIT);
        assert_eq!(snap.truncated, 60 - DEFAULT_MCP_TOOL_LIMIT);
        for id in ["s1", "s2", "s3"] {
            assert_eq!(snap.tools.iter().filter(|tool| tool.server_id == id).count(), 16);
        }
    }

    #[tokio::test]
    async fn user_limit_applies_to_one_large_server() {
        let many: Vec<McpTool> =
            (0..(DEFAULT_MCP_TOOL_LIMIT + 3)).map(|i| tool(&format!("t{i:02}"), false)).collect();
        let m = manager_with(vec![running(cfg("srv", McpTrust::Untrusted), many)]).await;
        let snap = m.snapshot();
        assert_eq!(snap.tools.len(), DEFAULT_MCP_TOOL_LIMIT);
        assert_eq!(snap.truncated, 3);
    }

    #[tokio::test]
    async fn changing_user_limit_rebuilds_the_snapshot() {
        let many: Vec<McpTool> = (0..80).map(|i| tool(&format!("t{i:02}"), false)).collect();
        let m = manager_with(vec![running(cfg("srv", McpTrust::Untrusted), many)]).await;
        m.set_tool_limit(72).await;
        assert_eq!(m.snapshot().tools.len(), 72);
        assert_eq!(m.snapshot().truncated, 8);
    }

    #[tokio::test]
    async fn resolve_server_matches_longest_id_prefix() {
        let m = McpManager::new("test");
        m.set_configs(vec![cfg("a", McpTrust::Untrusted), cfg("a_b", McpTrust::Untrusted)]).await;
        // "a_b" must win over "a" so the tool name isn't mangled into "b__do".
        let (id, tool) = m.resolve_server("mcp__a_b__do").await.unwrap();
        assert_eq!(id, "a_b");
        assert_eq!(tool, "do");
        let (id, tool) = m.resolve_server("mcp__a__thing").await.unwrap();
        assert_eq!(id, "a");
        assert_eq!(tool, "thing");
        assert!(m.resolve_server("mcp__zzz__x").await.is_err());
    }

    #[tokio::test]
    async fn ollama_def_shape() {
        let m = manager_with(vec![running(cfg("db", McpTrust::Untrusted), vec![tool("q", false)])]).await;
        let defs = m.snapshot().ollama_defs();
        assert_eq!(defs[0]["type"], "function");
        assert_eq!(defs[0]["function"]["name"], "mcp__db__q");
    }

    // ---- Process-based integration test against a real stdio fixture ----

    /// A tiny MCP server in Python: handshake, two-page tools/list, an `echo`
    /// tool, and a `boom` tool that exits to simulate a crash. Skipped (not
    /// failed) when no python3 is on PATH, so CI without it stays green.
    const FIXTURE: &str = r#"
import sys, json
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    msg = json.loads(line)
    method = msg.get("method"); mid = msg.get("id")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":mid,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":True}},"serverInfo":{"name":"fixture","version":"0"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        cursor = (msg.get("params") or {}).get("cursor")
        if cursor is None:
            send({"jsonrpc":"2.0","id":mid,"result":{"tools":[{"name":"echo","description":"echo args","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":True}}],"nextCursor":"p2"}})
        else:
            send({"jsonrpc":"2.0","id":mid,"result":{"tools":[{"name":"boom","description":"crashes","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        params = msg.get("params") or {}
        name = params.get("name")
        if name == "boom":
            sys.exit(1)
        send({"jsonrpc":"2.0","id":mid,"result":{"content":[{"type":"text","text":"echoed: "+json.dumps(params.get("arguments"))}]}})
    else:
        send({"jsonrpc":"2.0","id":mid,"error":{"code":-32601,"message":"unknown"}})
"#;

    fn python() -> Option<&'static str> {
        for candidate in ["python3", "python"] {
            if std::process::Command::new(candidate)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                return Some(candidate);
            }
        }
        None
    }

    fn write_fixture() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("mcp-fixture-{}.py", uuid::Uuid::new_v4()));
        std::fs::write(&path, FIXTURE).unwrap();
        path
    }

    #[tokio::test]
    async fn stdio_server_handshake_pagination_call_and_crash() {
        let Some(py) = python() else {
            eprintln!("skipping: no python3 on PATH");
            return;
        };
        let script = write_fixture();

        let manager = McpManager::new("test");
        let config = McpServerConfig {
            id: "fx".into(),
            label: "Fixture".into(),
            transport: McpTransport::Stdio {
                command: py.into(),
                args: vec![script.to_string_lossy().into_owned()],
                env: BTreeMap::new(),
                cwd: None,
            },
            enabled: true,
            trust: McpTrust::Untrusted,
            disabled_tools: vec![],
            catalog_id: None,
            origin: McpOrigin::Local,
        };
        manager.set_configs(vec![config]).await;
        manager.start("fx").await.expect("server should start");

        // Both pages of tools/list were merged.
        let snap = manager.snapshot();
        assert_eq!(snap.tools.len(), 2, "expected echo + boom");
        assert!(snap.find("mcp__fx__echo").is_some());
        assert!(snap.find("mcp__fx__boom").is_some());
        // Untrusted server: the read-only-hinted echo is still mutating.
        assert!(snap.find("mcp__fx__echo").unwrap().mutating);

        // A successful call round-trips content.
        let out = manager
            .call_tool("mcp__fx__echo", &serde_json::json!({"hi": 1}))
            .await
            .expect("echo call should succeed");
        assert!(out.text.contains("echoed"));
        assert!(!out.is_error);

        // Calling `boom` crashes the process; the call surfaces an error rather
        // than hanging, and the manager marks the server not-running.
        let boom = manager.call_tool("mcp__fx__boom", &serde_json::json!({})).await;
        assert!(boom.is_err(), "crash should produce an error, got {boom:?}");

        manager.stop("fx").await.ok();
        std::fs::remove_file(&script).ok();
    }
}
