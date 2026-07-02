//! Ollama adapter over its local HTTP API (default http://127.0.0.1:11434).

use super::{ModelInfo, RuntimeAdapter, RuntimeSnapshot};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::collections::HashSet;

pub struct OllamaAdapter {
    base: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct VersionResp {
    version: String,
}

#[derive(Deserialize)]
struct TagsResp {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    details: Option<ModelDetails>,
}

#[derive(Deserialize)]
struct ModelDetails {
    #[serde(default)]
    quantization_level: Option<String>,
}

#[derive(Deserialize)]
struct PsResp {
    #[serde(default)]
    models: Vec<PsModel>,
}

#[derive(Deserialize)]
struct PsModel {
    name: String,
}

impl OllamaAdapter {
    pub fn new(http: reqwest::Client) -> Self {
        let base = std::env::var("LOCALLMOS_OLLAMA_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
        Self { base: base.trim_end_matches('/').to_string(), http }
    }

    pub fn endpoint(&self) -> &str {
        &self.base
    }

    async fn version(&self) -> Option<String> {
        let resp = self
            .http
            .get(format!("{}/api/version", self.base))
            .send()
            .await
            .ok()?;
        resp.json::<VersionResp>().await.ok().map(|v| v.version)
    }

    async fn loaded_names(&self) -> HashSet<String> {
        let mut set = HashSet::new();
        if let Ok(resp) = self.http.get(format!("{}/api/ps", self.base)).send().await {
            if let Ok(ps) = resp.json::<PsResp>().await {
                for m in ps.models {
                    set.insert(m.name);
                }
            }
        }
        set
    }
}

impl RuntimeAdapter for OllamaAdapter {
    fn kind(&self) -> &'static str {
        "ollama"
    }

    async fn snapshot(&self) -> RuntimeSnapshot {
        let version = self.version().await;
        // If /api/version doesn't answer, treat the runtime as stopped.
        if version.is_none() {
            return RuntimeSnapshot {
                kind: "ollama".into(),
                version: None,
                state: "stopped".into(),
                endpoint: Some(self.base.clone()),
                models: vec![],
            };
        }

        let loaded = self.loaded_names().await;
        let mut models = Vec::new();
        if let Ok(resp) = self.http.get(format!("{}/api/tags", self.base)).send().await {
            if let Ok(tags) = resp.json::<TagsResp>().await {
                for m in tags.models {
                    let is_loaded = loaded.contains(&m.name);
                    models.push(ModelInfo {
                        quantization: m.details.and_then(|d| d.quantization_level),
                        size_bytes: m.size,
                        loaded: is_loaded,
                        name: m.name,
                    });
                }
            }
        }

        RuntimeSnapshot {
            kind: "ollama".into(),
            version,
            state: "running".into(),
            endpoint: Some(self.base.clone()),
            models,
        }
    }

    async fn load_model(&self, model: &str) -> Result<()> {
        // Empty prompt + keep_alive keeps the model resident without generating.
        let resp = self
            .http
            .post(format!("{}/api/generate", self.base))
            .json(&serde_json::json!({ "model": model, "keep_alive": "30m" }))
            .send()
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("ollama load_model failed: HTTP {}", resp.status()))
        }
    }

    async fn restart(&self) -> Result<()> {
        crate::worker::restart_service("ollama").await
    }
}
