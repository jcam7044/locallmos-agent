//! Relay-backed inference: a `RelayLlama` runs a single chat completion on a
//! *serving peer* in this rig's group by inserting an `inference_jobs` row and
//! awaiting the result over Supabase — no LAN, no ports. It satisfies the same
//! `chat_stream` shape the sub-agent loop expects (see
//! `subagent::SubagentRuntime`), so offloading a sub-agent's inference changes
//! only which runtime it is handed.
//!
//! Inference-only: the workspace and every file tool stay on the requester; only
//! the LLM forward pass travels. The peer resolves its own model load settings,
//! so we forward just messages / tools / think.

use crate::runtime::{ChatOutput, ToolCall};
use crate::AppState;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// How long the requester waits for a peer before giving up (and falling back to
/// local). Bounded so a stuck peer can't hang a sub-agent indefinitely.
fn job_timeout() -> Duration {
    let secs = std::env::var("LOCALLMOS_INFERENCE_JOB_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(240);
    Duration::from_secs(secs.clamp(30, 1800))
}

/// A handle that runs one completion on a specific serving peer via the relay.
/// Cheap to construct per sub-agent dispatch; holds `AppState` for the fresh
/// device token, the Supabase client, and this rig's id.
pub struct RelayLlama {
    state: Arc<AppState>,
    target_rig_id: String,
    group_id: String,
}

impl RelayLlama {
    pub fn new(state: Arc<AppState>, target_rig_id: String, group_id: String) -> Self {
        Self { state, target_rig_id, group_id }
    }

    /// Run one completion on the peer. `on_delta` is intentionally absent from
    /// the signature here: the relay returns the whole message at once and
    /// sub-agents don't stream tokens to the user (the loop consumes them
    /// internally), so there is nothing to stream.
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: Value,
        think: bool,
        tools: Option<&Value>,
        cancel: Arc<AtomicBool>,
    ) -> Result<ChatOutput> {
        let token = crate::worker::ensure_token(&self.state).await?;
        let requester = crate::worker::rig_id(&self.state)
            .await
            .ok_or_else(|| anyhow!("not enrolled"))?;

        let request = json!({
            "messages": messages,
            "tools": tools.cloned(),
            "think": think,
        });

        let job_id = self
            .state
            .supabase
            .insert_inference_job(
                &token,
                &self.group_id,
                &requester,
                &self.target_rig_id,
                model,
                request,
            )
            .await?;

        let result = self.await_result(&token, &job_id, cancel).await;
        // Best-effort cleanup regardless of outcome (a periodic purge reaps the
        // rest — see purge_stale_inference_jobs in 0051).
        self.state
            .supabase
            .delete_inference_job(&token, &job_id)
            .await
            .ok();
        result
    }

    async fn await_result(
        &self,
        token: &str,
        job_id: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<ChatOutput> {
        let deadline = Instant::now() + job_timeout();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!("cancelled"));
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("remote inference job timed out"));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
            let Some(job) = self.state.supabase.get_inference_job(token, job_id).await? else {
                // Row vanished (purged/deleted) — treat as failure so the caller
                // falls back to local inference.
                return Err(anyhow!("remote inference job disappeared"));
            };
            match job.status.as_str() {
                "done" => {
                    let resp = job
                        .response
                        .ok_or_else(|| anyhow!("completed job carried no response"))?;
                    return Ok(parse_response(&resp));
                }
                "error" => {
                    return Err(anyhow!(
                        "peer reported an error: {}",
                        job.error.unwrap_or_default()
                    ));
                }
                _ => {} // pending / claimed → keep polling
            }
        }
    }
}

/// Serialize a `ChatOutput` (the peer's completion) into the job `response`
/// jsonb. Used by the serving side.
pub fn encode_response(out: &ChatOutput) -> Value {
    let tool_calls: Vec<Value> = out
        .tool_calls
        .iter()
        .map(|c| json!({ "name": c.name, "arguments": c.arguments }))
        .collect();
    json!({
        "content": out.content,
        "thinking": out.thinking,
        "tool_calls": tool_calls,
        "prompt_tokens": out.prompt_tokens,
        "completion_tokens": out.completion_tokens,
    })
}

/// Rebuild a `ChatOutput` from a job `response` jsonb (requester side).
fn parse_response(v: &Value) -> ChatOutput {
    let tool_calls = v
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let name = c.get("name").and_then(Value::as_str)?.to_string();
                    let arguments = c.get("arguments").cloned().unwrap_or_else(|| json!({}));
                    Some(ToolCall { name, arguments })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ChatOutput {
        content: v.get("content").and_then(Value::as_str).unwrap_or_default().to_string(),
        thinking: v.get("thinking").and_then(Value::as_str).unwrap_or_default().to_string(),
        prompt_tokens: v.get("prompt_tokens").and_then(Value::as_u64).map(|n| n as u32),
        completion_tokens: v.get("completion_tokens").and_then(Value::as_u64).map(|n| n as u32),
        generation_metrics: None,
        tool_calls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_round_trips_through_the_job_jsonb() {
        let out = ChatOutput {
            content: "the fix is on line 4".into(),
            thinking: "reasoned about it".into(),
            prompt_tokens: Some(128),
            completion_tokens: Some(9),
            generation_metrics: None,
            tool_calls: vec![ToolCall {
                name: "read_file".into(),
                arguments: json!({ "path": "sum.py" }),
            }],
        };
        let encoded = encode_response(&out);
        let decoded = parse_response(&encoded);
        assert_eq!(decoded.content, out.content);
        assert_eq!(decoded.thinking, out.thinking);
        assert_eq!(decoded.prompt_tokens, Some(128));
        assert_eq!(decoded.completion_tokens, Some(9));
        assert_eq!(decoded.tool_calls.len(), 1);
        assert_eq!(decoded.tool_calls[0].name, "read_file");
        assert_eq!(decoded.tool_calls[0].arguments, json!({ "path": "sum.py" }));
    }

    #[test]
    fn parse_tolerates_a_bare_content_response() {
        let decoded = parse_response(&json!({ "content": "hi" }));
        assert_eq!(decoded.content, "hi");
        assert!(decoded.tool_calls.is_empty());
        assert!(decoded.prompt_tokens.is_none());
    }
}
