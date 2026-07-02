//! Chat turn handling: claim a pending assistant message, stream the reply from
//! Ollama, and relay token deltas to the web over Realtime broadcast. Safe to
//! call from both the Realtime handler and the fallback poll — the claim makes
//! processing single-shot.

use crate::supabase::ChatPending;
use crate::AppState;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Broadcast event name on the `chat:{id}` channel (mirrors CHAT_STREAM_EVENT
/// in packages/shared/src/chat.ts).
const CHAT_EVENT: &str = "chunk";
/// Flush cadence / size for batching token deltas into broadcasts.
const FLUSH_INTERVAL: Duration = Duration::from_millis(80);
const FLUSH_CHARS: usize = 24;

fn topic(message_id: &str) -> String {
    format!("chat:{message_id}")
}

/// Process one pending assistant turn end-to-end.
pub async fn process(state: &Arc<AppState>, pending: ChatPending) -> Result<()> {
    let token = crate::worker::ensure_token(state).await?;

    // Single-shot claim (pending → streaming); losers just return.
    if !state.supabase.claim_chat_message(&token, &pending.id).await? {
        return Ok(());
    }

    let model = match pending.model.clone() {
        Some(m) if !m.is_empty() => m,
        _ => {
            let msg = "no model specified for chat turn";
            state
                .supabase
                .update_chat_message(&token, &pending.id, "error", None, Some(msg))
                .await
                .ok();
            return Err(anyhow!(msg));
        }
    };

    // Build the Ollama chat history from prior completed messages.
    let context = state
        .supabase
        .fetch_chat_context(&token, &pending.conversation_id)
        .await?;
    let messages: Vec<Value> = context
        .iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect();

    // Async broadcaster: batches deltas from the sync stream callback.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let topic = topic(&pending.id);
    let broadcaster = {
        let state = state.clone();
        let token = token.clone();
        let topic = topic.clone();
        tokio::spawn(async move {
            let mut seq: u64 = 0;
            let mut buf = String::new();
            let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
            ticker.tick().await; // consume immediate first tick
            loop {
                tokio::select! {
                    maybe = rx.recv() => match maybe {
                        Some(delta) => {
                            buf.push_str(&delta);
                            if buf.len() >= FLUSH_CHARS {
                                let payload = json!({ "type": "token", "seq": seq, "delta": buf.as_str() });
                                state.supabase.broadcast(&token, &topic, CHAT_EVENT, payload).await.ok();
                                seq += 1;
                                buf.clear();
                            }
                        }
                        None => {
                            // Sender dropped: final flush and stop.
                            if !buf.is_empty() {
                                let payload = json!({ "type": "token", "seq": seq, "delta": buf.as_str() });
                                state.supabase.broadcast(&token, &topic, CHAT_EVENT, payload).await.ok();
                                seq += 1;
                            }
                            break;
                        }
                    },
                    _ = ticker.tick() => {
                        if !buf.is_empty() {
                            let payload = json!({ "type": "token", "seq": seq, "delta": buf.as_str() });
                            state.supabase.broadcast(&token, &topic, CHAT_EVENT, payload).await.ok();
                            seq += 1;
                            buf.clear();
                        }
                    }
                }
            }
            seq
        })
    };

    // Stream tokens from Ollama into the broadcaster.
    let result = state
        .ollama
        .chat_stream(&model, Value::Array(messages), |delta| {
            let _ = tx.send(delta.to_string());
        })
        .await;
    drop(tx);
    let last_seq = broadcaster.await.unwrap_or(0);

    match result {
        Ok(full) => {
            state
                .supabase
                .broadcast(&token, &topic, CHAT_EVENT, json!({ "type": "done", "seq": last_seq }))
                .await
                .ok();
            state
                .supabase
                .update_chat_message(&token, &pending.id, "done", Some(&full), None)
                .await?;
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            state
                .supabase
                .broadcast(
                    &token,
                    &topic,
                    CHAT_EVENT,
                    json!({ "type": "error", "seq": last_seq, "message": msg }),
                )
                .await
                .ok();
            state
                .supabase
                .update_chat_message(&token, &pending.id, "error", None, Some(&msg))
                .await
                .ok();
            Err(e)
        }
    }
}
