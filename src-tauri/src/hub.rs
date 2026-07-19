//! Public Hugging Face GGUF discovery and revision-pinned downloads.

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use uuid::Uuid;

const HF: &str = "https://huggingface.co";
const PAGE_SIZE: u32 = 30;
const CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubModelSummary {
    pub id: String,
    pub author: String,
    pub name: String,
    pub revision: String,
    pub downloads: u64,
    pub likes: u64,
    pub last_modified: Option<String>,
    pub pipeline_tag: Option<String>,
    pub tags: Vec<String>,
    pub avatar_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GgufFile {
    pub path: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEstimate {
    pub weights_bytes: u64,
    pub kv_cache_bytes: u64,
    pub overhead_bytes: u64,
    pub total_bytes: u64,
    pub confidence: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GgufVariant {
    pub id: String,
    pub quantization: String,
    pub size_bytes: u64,
    pub files: Vec<GgufFile>,
    pub companions: Vec<GgufFile>,
    pub memory: MemoryEstimate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubModelDetail {
    #[serde(flatten)]
    pub summary: HubModelSummary,
    pub license: Option<String>,
    pub base_models: Vec<String>,
    pub readme_markdown: String,
    pub variants: Vec<GgufVariant>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubModelPage {
    pub items: Vec<HubModelSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadState {
    pub id: String,
    pub repo_id: String,
    pub revision: String,
    pub variant_id: String,
    pub status: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawSibling {
    #[serde(rename = "rfilename")]
    path: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    lfs: Option<RawLfs>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawLfs {
    size: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawModel {
    id: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    sha: Option<String>,
    #[serde(default, rename = "lastModified")]
    last_modified: Option<String>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default, rename = "pipeline_tag")]
    pipeline_tag: Option<String>,
    #[serde(default)]
    siblings: Vec<RawSibling>,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    gated: Value,
    #[serde(default, rename = "cardData")]
    card_data: Option<Value>,
}

enum Cached {
    Page(HubModelPage),
    Detail(HubModelDetail),
}

pub struct HubState {
    client: reqwest::Client,
    models_dir: PathBuf,
    cache: Mutex<HashMap<String, (Instant, Cached)>>,
    downloads: Mutex<HashMap<String, DownloadState>>,
    cancellations: Mutex<HashMap<String, Arc<AtomicBool>>>,
    avatars: Mutex<HashMap<String, Option<String>>>,
    context_size: u64,
}

impl HubState {
    pub fn new(client: reqwest::Client, models_dir: String) -> Self {
        Self {
            client,
            models_dir: PathBuf::from(models_dir),
            cache: Mutex::new(HashMap::new()),
            downloads: Mutex::new(HashMap::new()),
            cancellations: Mutex::new(HashMap::new()),
            avatars: Mutex::new(HashMap::new()),
            context_size: std::env::var("LOCALLMOS_LLAMACPP_CTX").ok().and_then(|v| v.parse().ok()).unwrap_or(8192),
        }
    }

    pub async fn search(
        &self,
        query: &str,
        capability: &str,
        sort: &str,
        cursor: Option<&str>,
    ) -> Result<HubModelPage> {
        let url = if let Some(next) = cursor.filter(|s| !s.is_empty()) {
            validate_next_url(next)?;
            next.to_string()
        } else {
            let sort = match sort {
                "downloads" => "downloads",
                "likes" => "likes",
                "newest" => "lastModified",
                _ => "trendingScore",
            };
            let mut url = reqwest::Url::parse(&format!("{HF}/api/models"))?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("filter", "gguf")
                    .append_pair("gated", "false")
                    .append_pair("full", "true")
                    .append_pair("sort", sort)
                    .append_pair("direction", "-1")
                    .append_pair("limit", &PAGE_SIZE.to_string());
                if !query.trim().is_empty() {
                    q.append_pair("search", query.trim());
                }
                match capability {
                    "vision" => { q.append_pair("pipeline_tag", "image-text-to-text"); }
                    "text" => { q.append_pair("pipeline_tag", "text-generation"); }
                    _ => {}
                }
            }
            url.into()
        };
        if let Some((at, Cached::Page(page))) = self.cache.lock().await.get(&url) {
            if at.elapsed() < CACHE_TTL { return Ok(page.clone()); }
        }
        let response = self.client.get(&url).send().await.context("Hugging Face is unreachable")?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(anyhow!("Hugging Face rate limit reached. Try again in a moment."));
        }
        let response = response.error_for_status().context("Hugging Face search failed")?;
        let next_cursor = response
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_next_link);
        let raw: Vec<RawModel> = response
            .json()
            .await
            .map_err(|e| anyhow!("invalid Hugging Face response: {e}"))?;
        let page = HubModelPage {
            items: raw.into_iter().filter(|m| !m.private && !is_gated(&m.gated)).map(summary).collect(),
            next_cursor,
        };
        self.cache.lock().await.insert(url, (Instant::now(), Cached::Page(page.clone())));
        self.prune_cache().await;
        Ok(page)
    }

    pub async fn detail(&self, repo_id: &str) -> Result<HubModelDetail> {
        validate_repo(repo_id)?;
        let key = format!("detail:{repo_id}");
        if let Some((at, Cached::Detail(detail))) = self.cache.lock().await.get(&key) {
            if at.elapsed() < CACHE_TTL { return Ok(detail.clone()); }
        }
        let mut url = reqwest::Url::parse(&format!("{HF}/api/models/{repo_id}"))?;
        url.query_pairs_mut().append_pair("blobs", "true");
        let response = self.client.get(url).send().await.context("Hugging Face is unreachable")?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(anyhow!("Hugging Face rate limit reached. Try again in a moment."));
        }
        let raw: RawModel = response
            .error_for_status()?
            .json()
            .await
            .map_err(|e| anyhow!("invalid Hugging Face model response: {e}"))?;
        if raw.private || is_gated(&raw.gated) {
            return Err(anyhow!("This model requires Hugging Face authentication and is not available in the public catalog."));
        }
        let summary = summary(raw.clone());
        let revision = summary.revision.clone();
        let readme_url = format!("{HF}/{repo_id}/resolve/{revision}/README.md");
        let readme_markdown = match self.client.get(readme_url).send().await {
            Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
            _ => String::new(),
        };
        let card = raw.card_data.as_ref();
        let license = card.and_then(|v| v.get("license")).and_then(Value::as_str).map(str::to_string)
            .or_else(|| raw.tags.iter().find_map(|t| t.strip_prefix("license:").map(str::to_string)));
        let base_models = card.and_then(|v| v.get("base_model")).map(string_list).unwrap_or_default();
        let detail = HubModelDetail {
            summary,
            license,
            base_models,
            readme_markdown,
            variants: group_variants(&raw.siblings, self.context_size),
        };
        self.cache.lock().await.insert(key, (Instant::now(), Cached::Detail(detail.clone())));
        self.prune_cache().await;
        Ok(detail)
    }

    async fn prune_cache(&self) {
        self.cache.lock().await.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
    }

    pub async fn list_downloads(&self) -> Vec<DownloadState> {
        self.downloads.lock().await.values().cloned().collect()
    }

    pub async fn author_avatar(&self, author: &str) -> Result<Option<String>> {
        validate_author(author)?;
        if let Some(cached) = self.avatars.lock().await.get(author) {
            return Ok(cached.clone());
        }
        let mut avatar = None;
        for kind in ["organizations", "users"] {
            let url = format!("{HF}/api/{kind}/{author}/overview");
            let Ok(response) = self.client.get(url).send().await else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let value: Value = response.json().await.unwrap_or(Value::Null);
            avatar = value
                .get("avatarUrl")
                .and_then(Value::as_str)
                .filter(|url| url.starts_with("https://"))
                .map(str::to_string);
            if avatar.is_some() {
                break;
            }
        }
        self.avatars
            .lock()
            .await
            .insert(author.to_string(), avatar.clone());
        Ok(avatar)
    }

    pub async fn start_download(
        self: &Arc<Self>,
        app: AppHandle,
        repo_id: String,
        revision: String,
        variant_id: String,
    ) -> Result<DownloadState> {
        let detail = self.detail(&repo_id).await?;
        if detail.summary.revision != revision {
            return Err(anyhow!("model revision changed; refresh the model before downloading"));
        }
        let variant = detail.variants.into_iter().find(|v| v.id == variant_id)
            .ok_or_else(|| anyhow!("unknown GGUF variant"))?;
        if let Some(available) = available_space(&self.models_dir) {
            if available < variant.size_bytes {
                return Err(anyhow!("not enough free disk space for this GGUF variant"));
            }
        }
        let state = DownloadState {
            id: Uuid::new_v4().to_string(),
            repo_id: repo_id.clone(),
            revision: revision.clone(),
            variant_id: variant_id.clone(),
            status: "queued".into(),
            downloaded_bytes: 0,
            total_bytes: variant.size_bytes,
            error: None,
        };
        self.downloads.lock().await.insert(state.id.clone(), state.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancellations.lock().await.insert(state.id.clone(), cancel.clone());
        let hub = self.clone();
        let id = state.id.clone();
        tauri::async_runtime::spawn(async move {
            match hub.download(&app, &id, &repo_id, &revision, &variant, &cancel).await {
                Ok(DownloadOutcome::Complete) => {}
                Ok(DownloadOutcome::Cancelled) => {
                    hub.patch_download(&app, &id, |s| {
                        s.status = "cancelled".into();
                        s.error = None;
                    }).await;
                }
                Err(error) => {
                    hub.patch_download(&app, &id, |s| {
                        s.status = "error".into();
                        s.error = Some(error.to_string());
                    }).await;
                }
            }
            hub.cancellations.lock().await.remove(&id);
        });
        Ok(state)
    }

    pub async fn cancel_download(&self, app: &AppHandle, id: &str) -> Result<DownloadState> {
        let cancel = self.cancellations.lock().await.get(id).cloned()
            .ok_or_else(|| anyhow!("download is no longer active"))?;
        let state = self.downloads.lock().await.get(id).cloned()
            .ok_or_else(|| anyhow!("download was not found"))?;
        if state.status != "queued" && state.status != "downloading" {
            return Err(anyhow!("download is no longer active"));
        }
        cancel.store(true, Ordering::Relaxed);
        Ok(self.patch_download(app, id, |s| s.status = "canceling".into()).await
            .ok_or_else(|| anyhow!("download was not found"))?)
    }

    async fn download(&self, app: &AppHandle, id: &str, repo: &str, revision: &str, variant: &GgufVariant, cancel: &AtomicBool) -> Result<DownloadOutcome> {
        self.patch_download(app, id, |s| s.status = "downloading".into()).await;
        let (owner, model) = repo.split_once('/').ok_or_else(|| anyhow!("invalid repository"))?;
        let target_dir = self.models_dir.join("huggingface").join(owner).join(model);
        std::fs::create_dir_all(&target_dir)?;
        let mut completed = 0u64;
        let mut created = Vec::new();
        for file in &variant.files {
            if cancel.load(Ordering::Relaxed) {
                cleanup_download_files(&created);
                return Ok(DownloadOutcome::Cancelled);
            }
            validate_file(&file.path)?;
            let file_name = Path::new(&file.path).file_name().ok_or_else(|| anyhow!("invalid filename"))?;
            let target = target_dir.join(file_name);
            if let Ok(meta) = std::fs::metadata(&target) {
                if meta.len() == file.size_bytes {
                    completed += file.size_bytes;
                    self.set_progress(app, id, completed).await;
                    continue;
                }
                return Err(anyhow!("{} already exists with a different size", target.display()));
            }
            let mut part_name = target.as_os_str().to_os_string();
            part_name.push(".part");
            let part = PathBuf::from(part_name);
            let _ = std::fs::remove_file(&part);
            let mut output = tokio::fs::File::create(&part).await?;
            let mut url = reqwest::Url::parse(HF)?;
            url.path_segments_mut()
                .map_err(|_| anyhow!("invalid download URL"))?
                .clear()
                .push(owner)
                .push(model)
                .push("resolve")
                .push(revision)
                .extend(file.path.split('/'));
            let response = self.client.get(url).send().await?.error_for_status()?;
            let mut stream = response.bytes_stream();
            let mut file_done = 0u64;
            while let Some(chunk) = stream.next().await {
                if cancel.load(Ordering::Relaxed) {
                    drop(output);
                    let _ = tokio::fs::remove_file(&part).await;
                    cleanup_download_files(&created);
                    return Ok(DownloadOutcome::Cancelled);
                }
                let bytes = chunk?;
                output.write_all(&bytes).await?;
                file_done += bytes.len() as u64;
                self.set_progress(app, id, completed + file_done).await;
            }
            if cancel.load(Ordering::Relaxed) {
                drop(output);
                let _ = tokio::fs::remove_file(&part).await;
                cleanup_download_files(&created);
                return Ok(DownloadOutcome::Cancelled);
            }
            output.flush().await?;
            if file.size_bytes > 0 && file_done != file.size_bytes {
                return Err(anyhow!("downloaded size did not match Hub metadata"));
            }
            tokio::fs::rename(&part, &target).await?;
            created.push(target);
            completed += file_done;
        }
        if cancel.load(Ordering::Relaxed) {
            cleanup_download_files(&created);
            return Ok(DownloadOutcome::Cancelled);
        }
        let manifest = target_dir.join(format!("{}.locallmos.json", safe_manifest_name(&variant.id)));
        std::fs::write(manifest, serde_json::to_vec_pretty(&json!({
            "repoId": repo,
            "revision": revision,
            "variantId": variant.id,
            "files": variant.files,
        }))?)?;
        self.patch_download(app, id, |s| {
            s.status = "complete".into();
            s.downloaded_bytes = s.total_bytes;
        }).await;
        Ok(DownloadOutcome::Complete)
    }

    async fn set_progress(&self, app: &AppHandle, id: &str, bytes: u64) {
        self.patch_download(app, id, |s| s.downloaded_bytes = bytes.min(s.total_bytes)).await;
    }

    async fn patch_download<F: FnOnce(&mut DownloadState)>(&self, app: &AppHandle, id: &str, patch: F) -> Option<DownloadState> {
        let next = {
            let mut all = self.downloads.lock().await;
            let Some(state) = all.get_mut(id) else { return None };
            patch(state);
            state.clone()
        };
        let _ = app.emit("model-download", &next);
        Some(next)
    }
}

enum DownloadOutcome { Complete, Cancelled }

fn cleanup_download_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

fn summary(raw: RawModel) -> HubModelSummary {
    let author = raw.author.unwrap_or_else(|| raw.id.split('/').next().unwrap_or("community").to_string());
    let name = raw.id.split('/').next_back().unwrap_or(&raw.id).to_string();
    HubModelSummary {
        id: raw.id,
        avatar_url: String::new(),
        author,
        name,
        revision: raw.sha.unwrap_or_else(|| "main".into()),
        downloads: raw.downloads,
        likes: raw.likes,
        last_modified: raw.last_modified,
        pipeline_tag: raw.pipeline_tag,
        tags: raw.tags,
    }
}

fn group_variants(files: &[RawSibling], context_size: u64) -> Vec<GgufVariant> {
    let companions: Vec<GgufFile> = files.iter().filter(|f| f.path.to_lowercase().ends_with(".gguf") && f.path.to_lowercase().contains("mmproj"))
        .map(file_info).collect();
    let mut groups: BTreeMap<String, Vec<GgufFile>> = BTreeMap::new();
    for file in files.iter().filter(|f| f.path.to_lowercase().ends_with(".gguf") && !f.path.to_lowercase().contains("mmproj")) {
        let stem = Path::new(&file.path).file_stem().and_then(|s| s.to_str()).unwrap_or(&file.path);
        let key = shard_base(stem);
        groups.entry(key).or_default().push(file_info(file));
    }
    let mut variants: Vec<_> = groups.into_iter().map(|(id, mut files)| {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let size_bytes: u64 = files.iter().map(|f| f.size_bytes).sum();
        let overhead = (size_bytes / 20).max(512 * 1024 * 1024);
        let base_kv = (size_bytes / 10).max(1024 * 1024 * 1024);
        let kv = base_kv.saturating_mul(context_size.max(1024)) / 8192;
        GgufVariant {
            quantization: quantization(&id),
            id,
            size_bytes,
            files,
            companions: companions.clone(),
            memory: MemoryEstimate {
                weights_bytes: size_bytes,
                kv_cache_bytes: kv,
                overhead_bytes: overhead,
                total_bytes: size_bytes.saturating_add(kv).saturating_add(overhead),
                confidence: "low".into(),
            },
        }
    }).collect();
    variants.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    variants
}

fn file_info(raw: &RawSibling) -> GgufFile {
    GgufFile { path: raw.path.clone(), size_bytes: raw.size.or_else(|| raw.lfs.as_ref().and_then(|l| l.size)).unwrap_or(0) }
}

fn shard_base(stem: &str) -> String {
    let Some((left, total)) = stem.rsplit_once("-of-") else { return stem.to_string() };
    let Some((base, part)) = left.rsplit_once('-') else { return stem.to_string() };
    if total.len() == 5 && part.len() == 5 && total.chars().all(|c| c.is_ascii_digit()) && part.chars().all(|c| c.is_ascii_digit()) {
        base.to_string()
    } else { stem.to_string() }
}

fn quantization(name: &str) -> String {
    let upper = name.to_uppercase();
    for marker in ["UD-Q", "IQ", "PQ", "Q", "BF16", "F16", "F32"] {
        if let Some(i) = upper.rfind(marker) {
            return upper[i..].trim_matches(|c: char| c == '-' || c == '_' || c == '.').to_string();
        }
    }
    "GGUF".into()
}

fn string_list(value: &Value) -> Vec<String> {
    if let Some(s) = value.as_str() { vec![s.to_string()] }
    else { value.as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default() }
}

fn is_gated(value: &Value) -> bool {
    value.as_bool().unwrap_or(false) || value.as_str().map(|s| s != "false" && !s.is_empty()).unwrap_or(false)
}

fn parse_next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        if !part.contains("rel=\"next\"") { return None; }
        let start = part.find('<')? + 1;
        let end = part[start..].find('>')? + start;
        Some(part[start..end].to_string())
    })
}

fn validate_next_url(url: &str) -> Result<()> {
    if url.starts_with(&format!("{HF}/api/models?")) { Ok(()) } else { Err(anyhow!("invalid pagination cursor")) }
}

fn validate_repo(repo: &str) -> Result<()> {
    let parts: Vec<_> = repo.split('/').collect();
    if parts.len() == 2 && parts.iter().all(|p| {
        !p.is_empty() && *p != "." && *p != ".." &&
            p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    }) {
        Ok(())
    } else { Err(anyhow!("invalid repository id")) }
}

fn validate_author(author: &str) -> Result<()> {
    if !author.is_empty()
        && author != "."
        && author != ".."
        && author
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        Ok(())
    } else {
        Err(anyhow!("invalid Hugging Face author"))
    }
}

fn validate_file(file: &str) -> Result<()> {
    let path = Path::new(file);
    if path.is_absolute() || path.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(anyhow!("invalid repository filename"));
    }
    Ok(())
}

fn safe_manifest_name(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || "-_".contains(c) { c } else { '_' }).collect()
}

fn available_space(path: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .iter()
        .filter(|disk| path.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().components().count())
        .map(|disk| disk.available_space())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_shards_and_ignores_mmproj() {
        let raw = vec![
            RawSibling { path: "model-Q4_K_M-00001-of-00002.gguf".into(), size: Some(10), lfs: None },
            RawSibling { path: "model-Q4_K_M-00002-of-00002.gguf".into(), size: Some(12), lfs: None },
            RawSibling { path: "mmproj-model-f16.gguf".into(), size: Some(3), lfs: None },
        ];
        let variants = group_variants(&raw, 8192);
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].files.len(), 2);
        assert_eq!(variants[0].size_bytes, 22);
        assert_eq!(variants[0].quantization, "Q4_K_M");
        assert_eq!(variants[0].companions.len(), 1);
    }

    #[test]
    fn rejects_traversal() {
        assert!(validate_repo("../evil").is_err());
        assert!(validate_file("../model.gguf").is_err());
        assert!(validate_file("ok/model.gguf").is_ok());
        assert!(validate_author("../evil").is_err());
        assert!(validate_author("unsloth").is_ok());
    }

    #[test]
    fn parses_next_page() {
        let link = "<https://huggingface.co/api/models?cursor=abc>; rel=\"next\"";
        assert_eq!(parse_next_link(link).as_deref(), Some("https://huggingface.co/api/models?cursor=abc"));
    }

    #[test]
    fn catalog_accepts_hugging_face_id_and_model_id_together() {
        let json = r#"[{
            "id":"owner/model-GGUF",
            "modelId":"owner/model-GGUF",
            "author":"owner",
            "sha":"abc123",
            "lastModified":"2026-07-18T06:05:31.000Z",
            "likes":12,
            "downloads":34,
            "private":false,
            "gated":false,
            "pipeline_tag":"text-generation",
            "tags":["gguf"],
            "siblings":[{"rfilename":"model-Q4_K_M.gguf"}]
        }]"#;
        let models: Vec<RawModel> = serde_json::from_str(json).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "owner/model-GGUF");
    }
}
