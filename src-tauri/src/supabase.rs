//! Thin Supabase client for the agent: enrollment + token refresh via edge
//! functions, and telemetry / state / command IO via PostgREST using the
//! device JWT. Commands are pulled by polling (a simple, NAT-friendly MVP;
//! Realtime is a future optimization).

use crate::runtime::RuntimeSnapshot;
use anyhow::{anyhow, Result};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};

/// The server rejected our refresh secret: the rig was deleted from the
/// dashboard (its credentials cascade-deleted) or otherwise revoked. Callers
/// treat this as "de-enroll and return to the pairing screen", distinct from a
/// transient network/5xx failure which should just be retried.
#[derive(Debug, thiserror::Error)]
#[error("this rig was removed from the dashboard; enroll again to reconnect")]
pub struct CredentialsRevoked;

#[derive(Deserialize)]
pub struct EnrollResp {
    #[serde(rename = "rigId")]
    pub rig_id: String,
    pub token: String,
    #[serde(rename = "refreshSecret")]
    pub refresh_secret: String,
    #[serde(rename = "expiresIn")]
    pub expires_in: i64,
}

#[derive(Deserialize)]
pub struct TokenResp {
    pub token: String,
    #[serde(rename = "expiresIn")]
    pub expires_in: i64,
}

#[derive(Deserialize, Clone)]
pub struct CommandRow {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Deserialize, Default, Clone)]
pub struct DesiredState {
    pub desired_runtime: Option<String>,
    pub desired_model: Option<String>,
    #[serde(default)]
    pub desired_runtime_state: Option<String>,
}

/// A published agent release. `artifacts` is a `{ "os-arch": { url, sha256, sig } }`
/// map — the agent picks the entry matching its own platform.
#[derive(Deserialize, Clone)]
pub struct ReleaseRow {
    pub version: String,
    #[serde(default)]
    pub artifacts: Value,
}

/// A single platform artifact within a release.
#[derive(Deserialize, Clone)]
pub struct ReleaseArtifact {
    pub url: String,
    pub sha256: String,
    pub sig: String,
}

/// Owner-set auto-update preferences for a rig.
#[derive(Deserialize, Default, Clone)]
pub struct UpdatePrefs {
    #[serde(default)]
    pub auto_update: bool,
    #[serde(default)]
    pub update_channel: Option<String>,
    #[serde(default)]
    pub target_agent_version: Option<String>,
}

/// A pending assistant turn the agent must fulfil.
#[derive(Deserialize, Clone)]
pub struct ChatPending {
    pub id: String,
    pub conversation_id: String,
    #[serde(default)]
    pub model: Option<String>,
    /// Whether the turn requested reasoning output (reasoning models only).
    #[serde(default)]
    pub think: bool,
}

/// A file attached to a chat message (embedded via PostgREST).
#[derive(Deserialize, Clone)]
pub struct ChatAttachment {
    pub kind: String,
    pub storage_path: String,
    #[serde(default)]
    pub extracted_text: Option<String>,
}

/// A prior message used as chat context, with any attachments it carried.
#[derive(Deserialize, Clone)]
pub struct ChatContextMsg {
    pub role: String,
    pub content: String,
    #[serde(default, rename = "chat_attachments")]
    pub attachments: Vec<ChatAttachment>,
}

pub struct Supabase {
    http: Client,
    rest: String,
    functions: String,
    base: String,
    anon: String,
}

impl Supabase {
    pub fn new(http: Client, base_url: &str, anon: &str) -> Self {
        Self {
            http,
            rest: format!("{base_url}/rest/v1"),
            functions: format!("{base_url}/functions/v1"),
            base: base_url.to_string(),
            anon: anon.to_string(),
        }
    }

    /// Base URL (e.g. https://ref.supabase.co) for building the Realtime WS URL.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn anon_key(&self) -> &str {
        &self.anon
    }

    // ---- edge functions -------------------------------------------------
    pub async fn enroll(
        &self,
        code: &str,
        name: &str,
        os: &str,
        arch: &str,
        version: &str,
    ) -> Result<EnrollResp> {
        let resp = self
            .http
            .post(format!("{}/enroll", self.functions))
            .header("apikey", &self.anon)
            .json(&json!({
                "code": code, "name": name,
                "os": os, "arch": arch, "agentVersion": version,
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("enroll failed: HTTP {}", resp.status()));
        }
        Ok(resp.json().await?)
    }

    pub async fn refresh_token(&self, rig_id: &str, refresh_secret: &str) -> Result<TokenResp> {
        let resp = self
            .http
            .post(format!("{}/device-token", self.functions))
            .header("apikey", &self.anon)
            .json(&json!({ "rigId": rig_id, "refreshSecret": refresh_secret }))
            .send()
            .await?;
        // A 401 means the stored refresh secret is no longer valid — the rig
        // was deleted/revoked. Surface it as a typed error so the worker can
        // wipe local credentials and drop back to pairing.
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(CredentialsRevoked.into());
        }
        if !resp.status().is_success() {
            return Err(anyhow!("token refresh failed: HTTP {}", resp.status()));
        }
        Ok(resp.json().await?)
    }

    // ---- PostgREST (device JWT) ----------------------------------------
    fn auth(&self, req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
        req.header("apikey", &self.anon)
            .header("Authorization", format!("Bearer {token}"))
    }

    pub async fn post_metrics(&self, token: &str, row: Value) -> Result<()> {
        let resp = self
            .auth(self.http.post(format!("{}/rig_metrics", self.rest)), token)
            .header("Prefer", "return=minimal")
            .json(&row)
            .send()
            .await?;
        ensure_ok(resp, "post_metrics").await
    }

    /// Update `last_seen`/host info and, crucially, tell the caller whether the
    /// rig is still live. We filter on `deleted_at is null` and request the
    /// updated row back: with a valid JWT a live rig returns one row, but a rig
    /// the owner soft-deleted from the dashboard returns zero rows. That doubles
    /// as a cheap "am I still enrolled?" probe every telemetry tick — see
    /// `worker::telemetry_tick`. Returns `Ok(true)` if live, `Ok(false)` if
    /// deleted.
    pub async fn heartbeat(
        &self,
        token: &str,
        rig_id: &str,
        os: &str,
        arch: &str,
        version: &str,
    ) -> Result<bool> {
        let ts = chrono::Utc::now().to_rfc3339();
        let resp = self
            .auth(
                self.http.patch(format!(
                    "{}/rigs?id=eq.{rig_id}&deleted_at=is.null&select=id",
                    self.rest
                )),
                token,
            )
            .header("Prefer", "return=representation")
            .json(&json!({
                "last_seen": ts, "os": os, "arch": arch, "agent_version": version,
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("heartbeat failed: HTTP {status}: {body}"));
        }
        let rows: Vec<Value> = resp.json().await.unwrap_or_default();
        Ok(!rows.is_empty())
    }

    pub async fn upsert_runtime(
        &self,
        token: &str,
        rig_id: &str,
        snap: &RuntimeSnapshot,
    ) -> Result<()> {
        let resp = self
            .auth(
                self.http
                    .post(format!("{}/runtimes?on_conflict=rig_id,kind", self.rest)),
                token,
            )
            .header("Prefer", "resolution=merge-duplicates,return=minimal")
            .json(&json!({
                "rig_id": rig_id,
                "kind": snap.kind,
                "version": snap.version,
                "state": snap.state,
                "endpoint": snap.endpoint,
            }))
            .send()
            .await?;
        ensure_ok(resp, "upsert_runtime").await
    }

    pub async fn upsert_models(
        &self,
        token: &str,
        rig_id: &str,
        snap: &RuntimeSnapshot,
    ) -> Result<()> {
        if snap.models.is_empty() {
            return Ok(());
        }
        let rows: Vec<Value> = snap
            .models
            .iter()
            .map(|m| {
                json!({
                    "rig_id": rig_id,
                    "runtime_kind": snap.kind,
                    "name": m.name,
                    "size_bytes": m.size_bytes,
                    "quantization": m.quantization,
                    "loaded": m.loaded,
                    "capabilities": m.capabilities,
                })
            })
            .collect();
        let resp = self
            .auth(
                self.http.post(format!(
                    "{}/models?on_conflict=rig_id,runtime_kind,name",
                    self.rest
                )),
                token,
            )
            .header("Prefer", "resolution=merge-duplicates,return=minimal")
            .json(&rows)
            .send()
            .await?;
        ensure_ok(resp, "upsert_models").await
    }

    pub async fn fetch_desired(&self, token: &str, rig_id: &str) -> Result<DesiredState> {
        let resp = self
            .auth(
                self.http.get(format!(
                    "{}/rigs?id=eq.{rig_id}&select=desired_runtime,desired_model,desired_runtime_state",
                    self.rest
                )),
                token,
            )
            .send()
            .await?;
        let rows: Vec<DesiredState> = resp.json().await?;
        Ok(rows.into_iter().next().unwrap_or_default())
    }

    // ---- self-update ----------------------------------------------------
    /// The newest non-yanked release on a channel (via the `latest_agent_release`
    /// view). `None` when the channel has no releases yet.
    pub async fn fetch_latest_release(
        &self,
        token: &str,
        channel: &str,
    ) -> Result<Option<ReleaseRow>> {
        let resp = self
            .auth(
                self.http.get(format!(
                    "{}/latest_agent_release?channel=eq.{channel}&select=version,artifacts",
                    self.rest
                )),
                token,
            )
            .send()
            .await?;
        let rows: Vec<ReleaseRow> = resp.json().await?;
        Ok(rows.into_iter().next())
    }

    /// A specific release by channel + version (for a pinned/manual update).
    pub async fn fetch_release(
        &self,
        token: &str,
        channel: &str,
        version: &str,
    ) -> Result<Option<ReleaseRow>> {
        let resp = self
            .auth(
                self.http.get(format!(
                    "{}/agent_releases?channel=eq.{channel}&version=eq.{version}&yanked=is.false&select=version,artifacts",
                    self.rest
                )),
                token,
            )
            .send()
            .await?;
        let rows: Vec<ReleaseRow> = resp.json().await?;
        Ok(rows.into_iter().next())
    }

    /// This rig's owner-set auto-update preferences.
    pub async fn fetch_update_prefs(&self, token: &str, rig_id: &str) -> Result<UpdatePrefs> {
        let resp = self
            .auth(
                self.http.get(format!(
                    "{}/rigs?id=eq.{rig_id}&select=auto_update,update_channel,target_agent_version",
                    self.rest
                )),
                token,
            )
            .send()
            .await?;
        let rows: Vec<UpdatePrefs> = resp.json().await?;
        Ok(rows.into_iter().next().unwrap_or_default())
    }

    /// Report update progress back to the rig row (`update_status`/`update_error`).
    /// These are device-writable facts (see the rigs_device_scope_guard trigger).
    pub async fn set_update_status(
        &self,
        token: &str,
        rig_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let resp = self
            .auth(
                self.http
                    .patch(format!("{}/rigs?id=eq.{rig_id}", self.rest)),
                token,
            )
            .header("Prefer", "return=minimal")
            .json(&json!({ "update_status": status, "update_error": error }))
            .send()
            .await?;
        ensure_ok(resp, "set_update_status").await
    }

    pub async fn fetch_pending_commands(
        &self,
        token: &str,
        rig_id: &str,
    ) -> Result<Vec<CommandRow>> {
        let resp = self
            .auth(
                self.http.get(format!(
                    "{}/commands?rig_id=eq.{rig_id}&status=eq.pending&select=id,type,payload&order=created_at.asc",
                    self.rest
                )),
                token,
            )
            .send()
            .await?;
        Ok(resp.json().await?)
    }

    pub async fn update_command(
        &self,
        token: &str,
        id: &str,
        status: &str,
        result: Option<Value>,
    ) -> Result<()> {
        let mut body = json!({ "status": status });
        if let Some(r) = result {
            body["result"] = r;
        }
        let resp = self
            .auth(
                self.http
                    .patch(format!("{}/commands?id=eq.{id}", self.rest)),
                token,
            )
            .header("Prefer", "return=minimal")
            .json(&body)
            .send()
            .await?;
        ensure_ok(resp, "update_command").await
    }

    /// Atomically claim a pending command (pending → running). Returns true if
    /// *this* caller won the claim (safe against the WS + fallback-poll race).
    pub async fn claim_command(&self, token: &str, id: &str) -> Result<bool> {
        self.claim(token, "commands", id, "running").await
    }

    /// Atomically claim a pending assistant message (pending → streaming).
    pub async fn claim_chat_message(&self, token: &str, id: &str) -> Result<bool> {
        self.claim(token, "chat_messages", id, "streaming").await
    }

    async fn claim(&self, token: &str, table: &str, id: &str, to: &str) -> Result<bool> {
        let resp = self
            .auth(
                self.http.patch(format!(
                    "{}/{table}?id=eq.{id}&status=eq.pending",
                    self.rest
                )),
                token,
            )
            .header("Prefer", "return=representation")
            .json(&serde_json::json!({ "status": to }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let s = resp.status();
            return Err(anyhow!("claim {table} failed: HTTP {s}"));
        }
        let rows: Vec<Value> = resp.json().await.unwrap_or_default();
        Ok(!rows.is_empty())
    }

    // ---- chat -----------------------------------------------------------
    pub async fn fetch_pending_chat(
        &self,
        token: &str,
        rig_id: &str,
    ) -> Result<Vec<ChatPending>> {
        let resp = self
            .auth(
                self.http.get(format!(
                    "{}/chat_messages?rig_id=eq.{rig_id}&status=eq.pending&role=eq.assistant&select=id,conversation_id,model,think&order=created_at.asc",
                    self.rest
                )),
                token,
            )
            .send()
            .await?;
        Ok(resp.json().await?)
    }

    /// Prior completed messages in a conversation, oldest first, for context.
    pub async fn fetch_chat_context(
        &self,
        token: &str,
        conversation_id: &str,
    ) -> Result<Vec<ChatContextMsg>> {
        let resp = self
            .auth(
                self.http.get(format!(
                    "{}/chat_messages?conversation_id=eq.{conversation_id}&status=eq.done&select=role,content,chat_attachments(kind,storage_path,extracted_text)&order=created_at.asc",
                    self.rest
                )),
                token,
            )
            .send()
            .await?;
        Ok(resp.json().await?)
    }

    /// Download an attachment's bytes from the private Storage bucket. Storage
    /// RLS authorizes the device via `device_serves_conversation` (see 0019).
    pub async fn download_attachment(&self, token: &str, storage_path: &str) -> Result<Vec<u8>> {
        let url = format!(
            "{}/storage/v1/object/chat-attachments/{}",
            self.base, storage_path
        );
        let resp = self.auth(self.http.get(url), token).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "download_attachment failed: HTTP {}",
                resp.status()
            ));
        }
        Ok(resp.bytes().await?.to_vec())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_chat_message(
        &self,
        token: &str,
        id: &str,
        status: &str,
        content: Option<&str>,
        thinking: Option<&str>,
        error: Option<&str>,
        prompt_tokens: Option<u32>,
        completion_tokens: Option<u32>,
    ) -> Result<()> {
        let mut body = json!({ "status": status });
        if let Some(c) = content {
            body["content"] = json!(c);
        }
        if let Some(t) = thinking {
            body["thinking"] = json!(t);
        }
        if let Some(e) = error {
            body["error"] = json!(e);
        }
        if let Some(p) = prompt_tokens {
            body["prompt_tokens"] = json!(p);
        }
        if let Some(c) = completion_tokens {
            body["completion_tokens"] = json!(c);
        }
        let resp = self
            .auth(
                self.http
                    .patch(format!("{}/chat_messages?id=eq.{id}", self.rest)),
                token,
            )
            .header("Prefer", "return=minimal")
            .json(&body)
            .send()
            .await?;
        ensure_ok(resp, "update_chat_message").await
    }

}

async fn ensure_ok(resp: reqwest::Response, ctx: &str) -> Result<()> {
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(anyhow!("{ctx} failed: HTTP {status}: {body}"))
    }
}
