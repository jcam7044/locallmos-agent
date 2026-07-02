//! Chat turn handling: claim a pending assistant message, stream the reply from
//! Ollama, and relay token deltas to the web over the agent's Realtime
//! websocket (rig-scoped broadcast). Safe to call from both the Realtime handler
//! and the fallback poll — the claim makes processing single-shot.
//!
//! Resilience: the *authoritative* result is the persisted `chat_messages` row,
//! never the ephemeral broadcast. So disconnects degrade gracefully rather than
//! losing the answer:
//!   * Socket down when the turn starts → `join` fails, `ws_ready = false`: we
//!     skip live streaming but still generate + persist the reply, which the web
//!     picks up via postgres_changes (appears at completion instead of streaming).
//!   * Socket drops mid-stream → `broadcast` calls no-op (`.ok()`); generation
//!     continues and the final content is still persisted. Only that turn's live
//!     streaming is lost — not the answer.
//!   * The Realtime loop (`realtime::run`) reconnects within ~5s, and the 30s
//!     fallback poll in `worker.rs` picks up any assistant turn whose INSERT
//!     event was missed while the socket was down.
//! A mid-stream socket drop is not retried for the *current* turn; that would
//! need per-turn resumable streaming (out of scope for v1).

use crate::supabase::ChatPending;
use crate::AppState;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
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

    // Join the private broadcast channel over the websocket. If the socket is
    // down we still generate + persist the reply, just without live streaming.
    let chan = topic(&pending.id);
    let ws_ready = state.realtime.join(&chan, &token).await.is_ok();
    if !ws_ready {
        tracing::warn!("chat {}: realtime unavailable, streaming disabled", pending.id);
    }

    // Async broadcaster: batches deltas from the sync stream callback.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let broadcaster = {
        let state = state.clone();
        let chan = chan.clone();
        tokio::spawn(async move {
            let mut seq: u64 = 0;
            let mut buf = String::new();
            let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
            ticker.tick().await; // consume immediate first tick
            let flush = |seq: &mut u64, buf: &mut String| {
                let payload = json!({ "type": "token", "seq": *seq, "delta": buf.as_str() });
                *seq += 1;
                payload
            };
            loop {
                tokio::select! {
                    maybe = rx.recv() => match maybe {
                        Some(delta) => {
                            buf.push_str(&delta);
                            if buf.len() >= FLUSH_CHARS {
                                let p = flush(&mut seq, &mut buf);
                                if ws_ready { state.realtime.broadcast(&chan, CHAT_EVENT, p).await.ok(); }
                                buf.clear();
                            }
                        }
                        None => {
                            if !buf.is_empty() {
                                let p = flush(&mut seq, &mut buf);
                                if ws_ready { state.realtime.broadcast(&chan, CHAT_EVENT, p).await.ok(); }
                            }
                            break;
                        }
                    },
                    _ = ticker.tick() => {
                        if !buf.is_empty() {
                            let p = flush(&mut seq, &mut buf);
                            if ws_ready { state.realtime.broadcast(&chan, CHAT_EVENT, p).await.ok(); }
                            buf.clear();
                        }
                    }
                }
            }
            seq
        })
    };

    // Register a cancel flag so a Stop request can abort this turn.
    let cancel = Arc::new(AtomicBool::new(false));
    state.cancels.lock().await.insert(pending.id.clone(), cancel.clone());

    // Stream tokens from Ollama into the broadcaster.
    let result = state
        .ollama
        .chat_stream(&model, Value::Array(messages), cancel, |delta| {
            let _ = tx.send(delta.to_string());
        })
        .await;
    state.cancels.lock().await.remove(&pending.id);
    drop(tx);
    let last_seq = broadcaster.await.unwrap_or(0);

    let outcome = match result {
        Ok(full) => {
            if ws_ready {
                state
                    .realtime
                    .broadcast(&chan, CHAT_EVENT, json!({ "type": "done", "seq": last_seq }))
                    .await
                    .ok();
            }
            state
                .supabase
                .update_chat_message(&token, &pending.id, "done", Some(&full), None)
                .await?;
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            if ws_ready {
                state
                    .realtime
                    .broadcast(
                        &chan,
                        CHAT_EVENT,
                        json!({ "type": "error", "seq": last_seq, "message": msg }),
                    )
                    .await
                    .ok();
            }
            state
                .supabase
                .update_chat_message(&token, &pending.id, "error", None, Some(&msg))
                .await
                .ok();
            Err(e)
        }
    };

    if ws_ready {
        state.realtime.leave(&chan).await;
    }
    outcome
}
