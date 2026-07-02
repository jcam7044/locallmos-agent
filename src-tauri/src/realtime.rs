//! Minimal Supabase Realtime (Phoenix) websocket client. Replaces the old
//! command poll: the agent keeps one outbound websocket, subscribes to
//! `postgres_changes` INSERTs on `commands` and `chat_messages` scoped to its
//! rig, and dispatches them. Reconnects with a fixed backoff; the fallback poll
//! in `worker.rs` covers any gap while disconnected.

use crate::supabase::ChatPending;
use crate::AppState;
use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

const CHANNEL_TOPIC: &str = "realtime:locallmos";

pub async fn run(state: Arc<AppState>) {
    loop {
        if crate::worker::rig_id(&state).await.is_some() {
            if let Err(e) = connect_and_listen(&state).await {
                tracing::warn!("realtime disconnected: {e}");
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn connect_and_listen(state: &Arc<AppState>) -> Result<()> {
    let token = crate::worker::ensure_token(state).await?;
    let rig = crate::worker::rig_id(state)
        .await
        .ok_or_else(|| anyhow!("not enrolled"))?;

    // https://ref.supabase.co → wss://ref.supabase.co/realtime/v1/websocket
    let ws_base = state.supabase.base_url().replacen("http", "ws", 1);
    let url = format!(
        "{ws_base}/realtime/v1/websocket?apikey={}&vsn=1.0.0",
        state.supabase.anon_key()
    );

    let (ws, _) = tokio_tungstenite::connect_async(url.as_str()).await?;
    let (mut write, mut read) = ws.split();

    let join = json!({
        "topic": CHANNEL_TOPIC,
        "event": "phx_join",
        "ref": "1",
        "payload": {
            "config": {
                "postgres_changes": [
                    { "event": "INSERT", "schema": "public", "table": "chat_messages", "filter": format!("rig_id=eq.{rig}") },
                    { "event": "INSERT", "schema": "public", "table": "commands", "filter": format!("rig_id=eq.{rig}") }
                ]
            },
            "access_token": token
        }
    });
    write.send(Message::Text(join.to_string())).await?;
    tracing::info!("realtime connected; subscribed for rig {rig}");

    let mut heartbeat = tokio::time::interval(Duration::from_secs(25));
    heartbeat.tick().await; // consume immediate tick
    let mut ref_counter: u64 = 1;

    loop {
        tokio::select! {
            incoming = read.next() => {
                let Some(msg) = incoming else { return Err(anyhow!("socket closed")); };
                match msg? {
                    Message::Text(txt) => handle_event(state, &txt).await,
                    Message::Ping(p) => { let _ = write.send(Message::Pong(p)).await; }
                    Message::Close(_) => return Err(anyhow!("server closed connection")),
                    _ => {}
                }
            }
            _ = heartbeat.tick() => {
                ref_counter += 1;
                let hb = json!({ "topic": "phoenix", "event": "heartbeat", "payload": {}, "ref": ref_counter.to_string() });
                write.send(Message::Text(hb.to_string())).await?;
                // Keep the socket authorized as the device token rotates.
                if let Ok(tok) = crate::worker::ensure_token(state).await {
                    let at = json!({ "topic": CHANNEL_TOPIC, "event": "access_token", "payload": { "access_token": tok }, "ref": ref_counter.to_string() });
                    let _ = write.send(Message::Text(at.to_string())).await;
                }
            }
        }
    }
}

/// Parse a Phoenix `postgres_changes` frame and dispatch the inserted row.
async fn handle_event(state: &Arc<AppState>, txt: &str) {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return;
    };
    if v.get("event").and_then(Value::as_str) != Some("postgres_changes") {
        return;
    }
    let Some(data) = v.get("payload").and_then(|p| p.get("data")) else {
        return;
    };
    if data.get("type").and_then(Value::as_str) != Some("INSERT") {
        return;
    }
    let table = data.get("table").and_then(Value::as_str).unwrap_or("");
    let Some(record) = data.get("record") else {
        return;
    };

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
