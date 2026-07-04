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

use crate::runtime::ollama::ChatDelta;
use crate::supabase::ChatPending;
use crate::AppState;
use anyhow::{anyhow, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// A streamed delta batched by the broadcaster into a `token`/`thinking` event.
enum StreamDelta {
    Content(String),
    Thinking(String),
}

/// Broadcast event name on the `chat:{id}` channel (mirrors CHAT_STREAM_EVENT
/// in packages/shared/src/chat.ts).
const CHAT_EVENT: &str = "chunk";
/// Flush cadence / size for batching token deltas into broadcasts.
const FLUSH_INTERVAL: Duration = Duration::from_millis(80);
const FLUSH_CHARS: usize = 24;
/// How often to re-emit the "loading" ping while a cold model loads, so a
/// late-subscribing client still catches it before the first token.
const LOADING_HEARTBEAT: Duration = Duration::from_millis(1500);

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
                .update_chat_message(&token, &pending.id, "error", None, None, Some(msg), None, None)
                .await
                .ok();
            return Err(anyhow!(msg));
        }
    };

    // Build the Ollama chat history from prior completed messages, folding in
    // attachments: images become per-message base64 `images`; documents have
    // their extracted text appended to the message content.
    let context = state
        .supabase
        .fetch_chat_context(&token, &pending.conversation_id)
        .await?;
    let mut messages: Vec<Value> = Vec::with_capacity(context.len());
    for m in &context {
        let mut content = m.content.clone();
        let mut images: Vec<String> = Vec::new();
        for a in &m.attachments {
            match a.kind.as_str() {
                "image" => match state.supabase.download_attachment(&token, &a.storage_path).await {
                    Ok(bytes) => {
                        images.push(base64::engine::general_purpose::STANDARD.encode(bytes))
                    }
                    Err(e) => tracing::warn!("chat {}: image download failed: {e}", pending.id),
                },
                "document" => {
                    if let Some(text) = a.extracted_text.as_deref().filter(|t| !t.is_empty()) {
                        content.push_str(&format!("\n\n[Attached file]\n{text}"));
                    }
                }
                _ => {}
            }
        }
        let mut obj = json!({ "role": m.role, "content": content });
        if !images.is_empty() {
            obj["images"] = json!(images);
        }
        messages.push(obj);
    }

    // Join the private broadcast channel over the websocket. If the socket is
    // down we still generate + persist the reply, just without live streaming.
    let chan = topic(&pending.id);
    let ws_ready = state.realtime.join(&chan, &token).await.is_ok();
    if !ws_ready {
        tracing::warn!("chat {}: realtime unavailable, streaming disabled", pending.id);
    }

    // Async broadcaster: batches content + thinking deltas from the sync stream
    // callback into `token`/`thinking` events on the shared channel.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamDelta>();
    let broadcaster = {
        let state = state.clone();
        let chan = chan.clone();
        tokio::spawn(async move {
            let mut seq: u64 = 0;
            let mut content = String::new();
            let mut thinking = String::new();
            let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
            ticker.tick().await; // consume immediate first tick

            // Flush a non-empty buffer as a typed event, bumping the shared seq.
            macro_rules! emit {
                ($ty:expr, $buf:expr) => {{
                    if !$buf.is_empty() {
                        let payload = json!({ "type": $ty, "seq": seq, "delta": $buf.as_str() });
                        seq += 1;
                        if ws_ready {
                            state.realtime.broadcast(&chan, CHAT_EVENT, payload).await.ok();
                        }
                        $buf.clear();
                    }
                }};
            }

            loop {
                tokio::select! {
                    maybe = rx.recv() => match maybe {
                        Some(StreamDelta::Content(d)) => {
                            content.push_str(&d);
                            if content.len() >= FLUSH_CHARS { emit!("token", content); }
                        }
                        Some(StreamDelta::Thinking(d)) => {
                            thinking.push_str(&d);
                            if thinking.len() >= FLUSH_CHARS { emit!("thinking", thinking); }
                        }
                        None => {
                            emit!("thinking", thinking);
                            emit!("token", content);
                            break;
                        }
                    },
                    _ = ticker.tick() => {
                        emit!("thinking", thinking);
                        emit!("token", content);
                    }
                }
            }
            seq
        })
    };

    // Register a cancel flag so a Stop request can abort this turn.
    let cancel = Arc::new(AtomicBool::new(false));
    state.cancels.lock().await.insert(pending.id.clone(), cancel.clone());

    // If the model isn't resident, Ollama blocks the chat request while it loads
    // (tens of seconds for large models). Heartbeat a "loading" ping so the web
    // shows a loading state instead of looking hung; stop it on the first token.
    let stop_loading = Arc::new(Notify::new());
    let loading_task = if ws_ready && !state.ollama.is_model_loaded(&model).await {
        let state = state.clone();
        let chan = chan.clone();
        let stop = stop_loading.clone();
        let model = model.clone();
        Some(tokio::spawn(async move {
            let payload = json!({ "type": "loading", "model": model });
            loop {
                state.realtime.broadcast(&chan, CHAT_EVENT, payload.clone()).await.ok();
                tokio::select! {
                    _ = stop.notified() => break,
                    _ = tokio::time::sleep(LOADING_HEARTBEAT) => {}
                }
            }
        }))
    } else {
        None
    };

    // Stream tokens from Ollama into the broadcaster.
    let first_token = Arc::new(AtomicBool::new(false));
    let result = {
        let first_token = first_token.clone();
        let stop_loading = stop_loading.clone();
        let tx = tx.clone();
        state
            .ollama
            .chat_stream(&model, Value::Array(messages), pending.think, cancel, move |delta| {
                if !first_token.swap(true, Ordering::Relaxed) {
                    stop_loading.notify_one(); // first delta (thinking or content) → drop loading
                }
                let _ = match delta {
                    ChatDelta::Content(s) => tx.send(StreamDelta::Content(s.to_string())),
                    ChatDelta::Thinking(s) => tx.send(StreamDelta::Thinking(s.to_string())),
                };
            })
            .await
    };
    state.cancels.lock().await.remove(&pending.id);
    drop(tx);
    // Ensure the heartbeat stops even if the turn ended before any token.
    stop_loading.notify_one();
    if let Some(t) = loading_task {
        let _ = t.await;
    }
    let last_seq = broadcaster.await.unwrap_or(0);

    let outcome = match result {
        Ok(out) => {
            if ws_ready {
                state
                    .realtime
                    .broadcast(&chan, CHAT_EVENT, json!({ "type": "done", "seq": last_seq }))
                    .await
                    .ok();
            }
            let thinking = (!out.thinking.is_empty()).then_some(out.thinking.as_str());
            state
                .supabase
                .update_chat_message(
                    &token,
                    &pending.id,
                    "done",
                    Some(&out.content),
                    thinking,
                    None,
                    out.prompt_tokens,
                    out.completion_tokens,
                )
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
                .update_chat_message(&token, &pending.id, "error", None, None, Some(&msg), None, None)
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
