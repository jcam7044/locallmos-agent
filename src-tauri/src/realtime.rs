//! Supabase Realtime (Phoenix) websocket client. The agent keeps one outbound
//! socket and:
//!   * subscribes to `postgres_changes` INSERTs on commands + chat_messages
//!     (scoped to its rig) to receive work, and
//!   * broadcasts chat token deltas by joining the private `chat:{id}` channel
//!     and pushing broadcast frames — so sends are authorized (rig-scoped) by
//!     Realtime, unlike the REST broadcast path.
//!
//! `RealtimeHandle` exposes join/broadcast/leave to `chat.rs` over the shared
//! socket via an outbound mpsc; the read loop resolves join acks.

use crate::supabase::ChatPending;
use crate::AppState;
use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;

const CHANNEL_TOPIC: &str = "realtime:locallmos";

/// Send-side handle to the shared websocket, used by chat streaming.
pub struct RealtimeHandle {
    tx: Mutex<Option<mpsc::UnboundedSender<String>>>,
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    refs: AtomicU64,
}

impl RealtimeHandle {
    pub fn new() -> Self {
        Self {
            tx: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            refs: AtomicU64::new(100),
        }
    }

    async fn set_tx(&self, tx: mpsc::UnboundedSender<String>) {
        *self.tx.lock().await = Some(tx);
    }

    /// Called on disconnect: drop the sender and fail any waiting joins.
    async fn reset(&self) {
        *self.tx.lock().await = None;
        for (_, s) in self.pending.lock().await.drain() {
            let _ = s.send(false);
        }
    }

    fn next_ref(&self) -> String {
        self.refs.fetch_add(1, Ordering::Relaxed).to_string()
    }

    async fn send_frame(&self, frame: String) -> Result<()> {
        let guard = self.tx.lock().await;
        let tx = guard.as_ref().ok_or_else(|| anyhow!("realtime socket not connected"))?;
        tx.send(frame).map_err(|_| anyhow!("realtime send channel closed"))
    }

    async fn on_reply(&self, r: &str, ok: bool) {
        if let Some(s) = self.pending.lock().await.remove(r) {
            let _ = s.send(ok);
        }
    }

    /// Join a private channel and await the server ack (rig-scoped authz).
    pub async fn join(&self, topic: &str, token: &str) -> Result<()> {
        let r = self.next_ref();
        let (ack_tx, ack_rx) = oneshot::channel();
        self.pending.lock().await.insert(r.clone(), ack_tx);

        let frame = json!({
            "topic": format!("realtime:{topic}"),
            "event": "phx_join",
            "ref": r,
            "join_ref": r,
            "payload": {
                "config": { "broadcast": { "self": false }, "private": true },
                "access_token": token
            }
        })
        .to_string();

        if let Err(e) = self.send_frame(frame).await {
            self.pending.lock().await.remove(&r);
            return Err(e);
        }
        match tokio::time::timeout(Duration::from_secs(5), ack_rx).await {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err(anyhow!("join denied for {topic}")),
            _ => {
                self.pending.lock().await.remove(&r);
                Err(anyhow!("join timed out for {topic}"))
            }
        }
    }

    pub async fn broadcast(&self, topic: &str, event: &str, payload: Value) -> Result<()> {
        let frame = json!({
            "topic": format!("realtime:{topic}"),
            "event": "broadcast",
            "ref": self.next_ref(),
            "payload": { "type": "broadcast", "event": event, "payload": payload }
        })
        .to_string();
        self.send_frame(frame).await
    }

    pub async fn leave(&self, topic: &str) {
        let frame = json!({
            "topic": format!("realtime:{topic}"),
            "event": "phx_leave",
            "ref": self.next_ref(),
            "payload": {}
        })
        .to_string();
        let _ = self.send_frame(frame).await;
    }
}

pub async fn run(state: Arc<AppState>) {
    loop {
        if crate::worker::rig_id(&state).await.is_some() {
            if let Err(e) = connect_and_listen(&state).await {
                tracing::warn!("realtime disconnected: {e}");
            }
            state.realtime.reset().await;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn connect_and_listen(state: &Arc<AppState>) -> Result<()> {
    let token = crate::worker::ensure_token(state).await?;
    let rig = crate::worker::rig_id(state)
        .await
        .ok_or_else(|| anyhow!("not enrolled"))?;

    let ws_base = state.supabase.base_url().replacen("http", "ws", 1);
    let url = format!(
        "{ws_base}/realtime/v1/websocket?apikey={}&vsn=1.0.0",
        state.supabase.anon_key()
    );

    let (ws, _) = tokio_tungstenite::connect_async(url.as_str()).await?;
    let (mut write, mut read) = ws.split();

    // Outbound frame channel: chat.rs pushes join/broadcast/leave frames here.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    state.realtime.set_tx(out_tx).await;

    // Subscribe (ref "1") for commands + chat_messages INSERTs on our rig.
    let join = json!({
        "topic": CHANNEL_TOPIC,
        "event": "phx_join",
        "ref": "1",
        "payload": {
            "config": {
                "postgres_changes": [
                    { "event": "INSERT", "schema": "public", "table": "chat_messages", "filter": format!("rig_id=eq.{rig}") },
                    { "event": "UPDATE", "schema": "public", "table": "chat_messages", "filter": format!("rig_id=eq.{rig}") },
                    { "event": "INSERT", "schema": "public", "table": "commands", "filter": format!("rig_id=eq.{rig}") }
                ]
            },
            "access_token": token
        }
    });
    write.send(Message::Text(join.to_string())).await?;
    tracing::info!("realtime connected; subscribed for rig {rig}");

    let mut heartbeat = tokio::time::interval(Duration::from_secs(25));
    heartbeat.tick().await;
    let mut ref_counter: u64 = 1;

    loop {
        tokio::select! {
            incoming = read.next() => {
                let Some(msg) = incoming else { return Err(anyhow!("socket closed")); };
                match msg? {
                    Message::Text(txt) => route(state, &txt).await,
                    Message::Ping(p) => { let _ = write.send(Message::Pong(p)).await; }
                    Message::Close(_) => return Err(anyhow!("server closed connection")),
                    _ => {}
                }
            }
            frame = out_rx.recv() => {
                match frame {
                    Some(f) => write.send(Message::Text(f)).await?,
                    None => return Err(anyhow!("outbound channel closed")),
                }
            }
            _ = heartbeat.tick() => {
                ref_counter += 1;
                let hb = json!({ "topic": "phoenix", "event": "heartbeat", "payload": {}, "ref": ref_counter.to_string() });
                write.send(Message::Text(hb.to_string())).await?;
                if let Ok(tok) = crate::worker::ensure_token(state).await {
                    let at = json!({ "topic": CHANNEL_TOPIC, "event": "access_token", "payload": { "access_token": tok }, "ref": ref_counter.to_string() });
                    let _ = write.send(Message::Text(at.to_string())).await;
                }
            }
        }
    }
}

/// Route an incoming frame: resolve join acks, or dispatch postgres changes.
async fn route(state: &Arc<AppState>, txt: &str) {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return;
    };
    match v.get("event").and_then(Value::as_str) {
        Some("phx_reply") => {
            if let Some(r) = v.get("ref").and_then(Value::as_str) {
                let ok = v
                    .get("payload")
                    .and_then(|p| p.get("status"))
                    .and_then(Value::as_str)
                    == Some("ok");
                state.realtime.on_reply(r, ok).await;
            }
        }
        Some("postgres_changes") => handle_change(state, &v).await,
        _ => {}
    }
}

/// Dispatch a `postgres_changes` event to the command / chat / cancel handlers.
async fn handle_change(state: &Arc<AppState>, v: &Value) {
    let Some(data) = v.get("payload").and_then(|p| p.get("data")) else {
        return;
    };
    let change_type = data.get("type").and_then(Value::as_str).unwrap_or("");
    let table = data.get("table").and_then(Value::as_str).unwrap_or("");
    let Some(record) = data.get("record") else {
        return;
    };

    // Stop-generation: a UPDATE that sets cancel_requested trips the turn's flag.
    if change_type == "UPDATE" && table == "chat_messages" {
        if record.get("cancel_requested").and_then(Value::as_bool) == Some(true) {
            if let Some(id) = record.get("id").and_then(Value::as_str) {
                if let Some(flag) = state.cancels.lock().await.get(id) {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    tracing::info!("chat {id}: cancel requested");
                }
            }
        }
        return;
    }

    if change_type != "INSERT" {
        return;
    }

    match table {
        "commands" => {
            let id = record.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let kind = record.get("type").and_then(Value::as_str).unwrap_or("").to_string();
            let payload = record.get("payload").cloned().unwrap_or_else(|| json!({}));
            if id.is_empty() {
                return;
            }
            let state = state.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::worker::process_command(&state, &id, &kind, &payload).await {
                    tracing::warn!("command {id} failed: {e}");
                }
            });
        }
        "chat_messages" => {
            let role = record.get("role").and_then(Value::as_str).unwrap_or("");
            let status = record.get("status").and_then(Value::as_str).unwrap_or("");
            if role != "assistant" || status != "pending" {
                return;
            }
            let pending = ChatPending {
                id: record.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                conversation_id: record
                    .get("conversation_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                model: record.get("model").and_then(Value::as_str).map(str::to_string),
                think: record.get("think").and_then(Value::as_bool).unwrap_or(false),
                web_search: record.get("web_search").and_then(Value::as_bool).unwrap_or(false),
                request_tools: record.get("request_tools").filter(|v| !v.is_null()).cloned(),
                tool_protocol_version: record
                    .get("tool_protocol_version")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                platform_tools: record.get("platform_tools").filter(|v| !v.is_null()).cloned(),
            };
            if pending.id.is_empty() {
                return;
            }
            let state = state.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::chat::process(&state, pending).await {
                    tracing::warn!("chat turn failed: {e}");
                }
            });
        }
        _ => {}
    }
}
