//! Signed, user-initiated llama.cpp desktop updates.
//!
//! The rolling catalog is promoted only after LocalLMOS has built Linux CUDA
//! and verified all referenced upstream assets. The install marker is the
//! ownership boundary: unmarked/PATH installations are never changed.

use crate::runtime::llama_server::{managed_installation, ManagedInstallation};
use crate::runtime::Runtime;
use crate::updater::verify_release_signature;
use crate::AppState;
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

const DEFAULT_CATALOG_URL: &str = "https://github.com/jcam7044/locallmos-agent/releases/download/llamacpp-stable/llamacpp-release.json";
const UPDATE_EVENT: &str = "llamacpp-update";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Catalog {
    schema_version: u32,
    channel: String,
    tag: String,
    artifacts: Vec<CatalogArtifact>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogArtifact {
    os: String,
    arch: String,
    backend: String,
    variant: Option<String>,
    archive: String,
    name: String,
    url: String,
    sha256: String,
    size_bytes: u64,
    #[serde(default)]
    companions: Vec<CompanionArtifact>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompanionArtifact {
    name: String,
    url: String,
    sha256: String,
    size_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlamaCppUpdateInfo {
    pub current_tag: Option<String>,
    pub latest_tag: String,
    pub backend: String,
    pub variant: Option<String>,
    pub size_bytes: u64,
    pub installable: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProgress {
    phase: &'static str,
    tag: String,
    downloaded_bytes: u64,
    total_bytes: u64,
    message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransactionJournal {
    root: PathBuf,
    backup: PathBuf,
    tag: String,
}

pub async fn check(state: &Arc<AppState>) -> Result<Option<LlamaCppUpdateInfo>> {
    let install = installation(state)?;
    let catalog = fetch_catalog().await?;
    validate_catalog(&catalog)?;
    if install
        .tag
        .as_deref()
        .is_some_and(|tag| !is_newer_build(&catalog.tag, tag))
    {
        return Ok(None);
    }
    let artifact = select_artifact(&catalog, &install)?;
    let (installable, reason) = installability(&install);
    Ok(Some(LlamaCppUpdateInfo {
        current_tag: install.tag,
        latest_tag: catalog.tag.clone(),
        backend: artifact.backend.clone(),
        variant: artifact.variant.clone(),
        size_bytes: artifact.size_bytes
            + artifact.companions.iter().map(|item| item.size_bytes).sum::<u64>(),
        installable,
        reason,
    }))
}

pub async fn install(app: AppHandle, state: Arc<AppState>, requested_tag: String) -> Result<()> {
    if state
        .llamacpp_update_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        bail!("a llama.cpp update is already running");
    }

    let result = install_inner(&app, &state, &requested_tag).await;
    state.llamacpp_update_running.store(false, Ordering::SeqCst);
    if let Err(error) = &result {
        emit(&app, "error", &requested_tag, 0, 0, Some(error.to_string()));
    }
    result
}

async fn install_inner(app: &AppHandle, state: &Arc<AppState>, requested_tag: &str) -> Result<()> {
    if !state.cancels.lock().await.is_empty() {
        bail!("wait for active chat or coding requests to finish before updating llama.cpp");
    }
    let _lifecycle = state.runtime_lifecycle.lock().await;
    let install = installation(state)?;
    let (installable, reason) = installability(&install);
    if !installable {
        bail!("{}", reason.unwrap_or_else(|| "this installation is managed externally".into()));
    }

    emit(app, "checking", requested_tag, 0, 0, None);
    let catalog = fetch_catalog().await?;
    validate_catalog(&catalog)?;
    if catalog.tag != requested_tag {
        bail!(
            "the supported llama.cpp release changed from {requested_tag} to {}; check again",
            catalog.tag
        );
    }
    if install
        .tag
        .as_deref()
        .is_some_and(|tag| !is_newer_build(&catalog.tag, tag))
    {
        bail!("llama.cpp is already up to date");
    }
    let artifact = select_artifact(&catalog, &install)?.clone();
    let total = artifact.size_bytes
        + artifact.companions.iter().map(|item| item.size_bytes).sum::<u64>();

    let target_root = desktop_target_root(&install)?;
    let migrating = target_root != install.root;
    let replacing_migration_target = migrating && target_root.exists();
    let parent = target_root.parent().context("llama.cpp install has no parent directory")?;
    fs::create_dir_all(parent).context("create llama.cpp desktop install parent")?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let stage = parent.join(format!(".locallmos-llamacpp-stage-{token}"));
    let payload = stage.join("payload");
    fs::create_dir_all(&payload).context("create llama.cpp update staging directory")?;

    let staged = async {
        let mut downloaded = 0u64;
        let main = stage.join(&artifact.name);
        download(app, requested_tag, &artifact.url, &main, downloaded, total).await?;
        verify_sha256(&main, &artifact.sha256)?;
        downloaded += artifact.size_bytes;
        emit(app, "verifying", requested_tag, downloaded, total, None);
        extract(&main, &payload, &artifact.archive)?;

        for companion in &artifact.companions {
            let path = stage.join(&companion.name);
            download(app, requested_tag, &companion.url, &path, downloaded, total).await?;
            verify_sha256(&path, &companion.sha256)?;
            downloaded += companion.size_bytes;
            extract(&path, &payload, archive_kind(&companion.name))?;
        }

        let staged_bin = find_server(&payload).context("download did not contain llama-server")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staged_bin, fs::Permissions::from_mode(0o755))?;
        }
        smoke_test(&staged_bin).await?;
        write_marker(&payload, &artifact, &catalog.tag)?;
        Ok::<PathBuf, anyhow::Error>(staged_bin)
    }
    .await;

    let staged_bin = match staged {
        Ok(path) => path,
        Err(error) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(error);
        }
    };
    let new_bin_rel = staged_bin.strip_prefix(&payload)?.to_path_buf();

    let loaded = state
        .runtime
        .snapshot()
        .await
        .models
        .into_iter()
        .find(|model| model.loaded)
        .map(|model| model.id);

    emit(app, "installing", requested_tag, total, total, None);
    if let Some(model) = loaded.as_deref() {
        state
            .runtime
            .unload_model(model)
            .await
            .context("stop current llama-server")?;
    } else {
        state.runtime.restart().await.context("stop current llama-server")?;
    }

    let backup = parent.join(".locallmos-llamacpp-backup");
    if backup.exists() {
        fs::remove_dir_all(&backup).context("remove previous update backup")?;
    }
    let journal_path = parent.join(".locallmos-llamacpp-update.json");
    let journal = TransactionJournal {
        root: target_root.clone(),
        backup: backup.clone(),
        tag: catalog.tag.clone(),
    };
    fs::write(&journal_path, serde_json::to_vec_pretty(&journal)?)?;

    if !migrating {
        fs::rename(&install.root, &backup).context("move current llama.cpp install to backup")?;
    } else if replacing_migration_target {
        fs::rename(&target_root, &backup).context("back up existing desktop llama.cpp target")?;
    }
    if let Err(error) = fs::rename(&payload, &target_root) {
        if !migrating {
            let _ = fs::rename(&backup, &install.root);
        } else if replacing_migration_target {
            let _ = fs::rename(&backup, &target_root);
        }
        let _ = fs::remove_file(&journal_path);
        let _ = fs::remove_dir_all(&stage);
        return Err(error).context("commit staged llama.cpp install");
    }
    let new_bin = target_root.join(&new_bin_rel);
    set_runtime_bin(&state.runtime, new_bin.to_string_lossy().into_owned())?;

    if let Some(model) = loaded.as_deref() {
        emit(app, "reloading", requested_tag, total, total, None);
        let (_, settings) = state.model_settings(model).await?;
        if let Err(load_error) = state.runtime.load_model_configured(model, &settings).await {
            state.runtime.restart().await.ok();
            let failed = parent.join(format!(".locallmos-llamacpp-failed-{token}"));
            let _ = fs::rename(&target_root, &failed);
            if !migrating {
                fs::rename(&backup, &install.root).context("restore previous llama.cpp install")?;
            } else if replacing_migration_target {
                fs::rename(&backup, &target_root).context("restore previous desktop llama.cpp target")?;
            }
            set_runtime_bin(&state.runtime, install.bin.to_string_lossy().into_owned())?;
            let _ = state.runtime.load_model_configured(model, &settings).await;
            let _ = fs::remove_dir_all(&failed);
            let _ = fs::remove_file(&journal_path);
            let _ = fs::remove_dir_all(&stage);
            bail!("new llama.cpp could not reload {model}; restored the previous version: {load_error}");
        }
    }

    if backup.exists() {
        if let Err(error) = fs::remove_dir_all(&backup) {
            tracing::warn!("could not remove llama.cpp update backup: {error}");
        }
    }
    let _ = fs::remove_file(&journal_path);
    let _ = fs::remove_dir_all(&stage);
    emit(app, "complete", requested_tag, total, total, None);
    Ok(())
}

fn installation(state: &AppState) -> Result<ManagedInstallation> {
    let Runtime::LlamaCpp(adapter) = &state.runtime else {
        bail!("llama.cpp is not the active runtime");
    };
    managed_installation(&adapter.bin_path()).ok_or_else(|| {
        anyhow!("this llama-server is not managed by LocalLMOS; update it with its package manager")
    })
}

fn set_runtime_bin(runtime: &Runtime, bin: String) -> Result<()> {
    let Runtime::LlamaCpp(adapter) = runtime else {
        bail!("llama.cpp is not the active runtime");
    };
    adapter.set_bin_path(bin);
    Ok(())
}

fn installability(install: &ManagedInstallation) -> (bool, Option<String>) {
    #[cfg(unix)]
    if install.root.starts_with("/opt") || install.root.starts_with("/usr") {
        return (
            false,
            Some("This is a system/service llama.cpp install; update it with the service installer.".into()),
        );
    }
    #[cfg(windows)]
    if std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .is_some_and(|root| install.root.starts_with(root))
    {
        return (
            true,
            Some("This update will migrate the legacy runtime to your user-owned LocalAppData folder.".into()),
        );
    }
    (true, None)
}

fn desktop_target_root(install: &ManagedInstallation) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        if std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .is_some_and(|root| install.root.starts_with(root))
        {
            let local = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .context("LOCALAPPDATA is unavailable")?;
            return Ok(local.join("LocalLMOS").join("llama"));
        }
    }
    Ok(install.root.clone())
}

async fn fetch_catalog() -> Result<Catalog> {
    let url = std::env::var("LOCALLMOS_LLAMACPP_CATALOG_URL")
        .unwrap_or_else(|_| DEFAULT_CATALOG_URL.into());
    let signature_url = format!("{url}.minisig");
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let get = |target: String| {
        let client = client.clone();
        async move {
            client
                .get(target)
                .header("User-Agent", concat!("locallmos-agent/", env!("CARGO_PKG_VERSION")))
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(anyhow::Error::from)
        }
    };
    let (bytes, signature) = tokio::try_join!(get(url), get(signature_url))?;
    let signature = String::from_utf8(signature).context("catalog signature is not UTF-8")?;
    verify_release_signature(&bytes, &signature)?;
    serde_json::from_slice(&bytes).context("parse signed llama.cpp release catalog")
}

fn validate_catalog(catalog: &Catalog) -> Result<()> {
    if catalog.schema_version != 1 || catalog.channel != "stable" {
        bail!("unsupported llama.cpp release catalog");
    }
    parse_build(&catalog.tag).context("catalog contains an invalid release tag")?;
    if catalog.artifacts.is_empty() {
        bail!("llama.cpp release catalog contains no artifacts");
    }
    for artifact in &catalog.artifacts {
        if !valid_sha256(&artifact.sha256)
            || !artifact.url.starts_with("https://")
            || !matches!(artifact.archive.as_str(), "tar.gz" | "zip")
            || artifact.size_bytes == 0
        {
            bail!("llama.cpp release catalog contains an invalid artifact");
        }
        if artifact.companions.iter().any(|item| {
            !valid_sha256(&item.sha256)
                || !item.url.starts_with("https://")
                || item.size_bytes == 0
        }) {
            bail!("llama.cpp release catalog contains an invalid companion artifact");
        }
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn select_artifact<'a>(catalog: &'a Catalog, install: &ManagedInstallation) -> Result<&'a CatalogArtifact> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let variant = (install.backend == "cuda").then(cuda_variant).flatten();
    catalog
        .artifacts
        .iter()
        .find(|item| {
            item.os == os
                && item.arch == arch
                && item.backend == install.backend
                && (item.backend != "cuda" || item.variant == variant)
        })
        .ok_or_else(|| {
            anyhow!(
                "release {} has no {} artifact for {os}-{arch}{}",
                catalog.tag,
                install.backend,
                variant.as_deref().map(|v| format!(" ({v})")).unwrap_or_default()
            )
        })
}

fn cuda_variant() -> Option<String> {
    let output = std::process::Command::new("nvidia-smi").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let marker = "CUDA Version:";
    let version = text.split(marker).nth(1)?.split_whitespace().next()?;
    let (major, minor) = version.split_once('.')?;
    let major: u32 = major.parse().ok()?;
    let minor: u32 = minor.parse().ok()?;
    if major >= 13 {
        Some("13.3".into())
    } else if major == 12 && minor >= 4 {
        Some("12.4".into())
    } else {
        None
    }
}

async fn download(
    app: &AppHandle,
    tag: &str,
    url: &str,
    path: &Path,
    offset: u64,
    total: u64,
) -> Result<()> {
    let response = reqwest::Client::new().get(url).send().await?.error_for_status()?;
    let mut stream = response.bytes_stream();
    let mut file = File::create(path)?;
    let mut current = offset;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)?;
        current += chunk.len() as u64;
        emit(app, "downloading", tag, current, total, None);
    }
    file.sync_all()?;
    Ok(())
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected.trim()) {
        bail!("sha256 mismatch for {}", path.display());
    }
    Ok(())
}

fn archive_kind(name: &str) -> &str {
    if name.ends_with(".zip") { "zip" } else { "tar.gz" }
}

fn extract(archive: &Path, destination: &Path, kind: &str) -> Result<()> {
    match kind {
        "tar.gz" => {
            let file = File::open(archive)?;
            let decoder = flate2::read::GzDecoder::new(file);
            let mut archive = tar::Archive::new(decoder);
            for entry in archive.entries()? {
                let mut entry = entry?;
                if !entry.unpack_in(destination)? {
                    bail!("archive contains a path outside the staging directory");
                }
            }
        }
        "zip" => {
            let file = File::open(archive)?;
            let mut archive = zip::ZipArchive::new(file)?;
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index)?;
                let relative = entry
                    .enclosed_name()
                    .ok_or_else(|| anyhow!("zip contains an unsafe path"))?
                    .to_path_buf();
                if relative.components().any(|c| !matches!(c, Component::Normal(_))) {
                    bail!("zip contains an unsafe path");
                }
                let out = destination.join(relative);
                if entry.is_dir() {
                    fs::create_dir_all(&out)?;
                } else {
                    if let Some(parent) = out.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let mut target = File::create(out)?;
                    std::io::copy(&mut entry, &mut target)?;
                }
            }
        }
        other => bail!("unsupported llama.cpp archive type: {other}"),
    }
    Ok(())
}

fn find_server(root: &Path) -> Option<PathBuf> {
    let wanted = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
    walkdir::WalkDir::new(root)
        .max_depth(4)
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| entry.file_type().is_file() && entry.file_name() == wanted)
        .map(|entry| entry.into_path())
}

async fn smoke_test(bin: &Path) -> Result<()> {
    let status = tokio::process::Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .with_context(|| format!("run {} --version", bin.display()))?;
    if !status.success() {
        bail!("downloaded llama-server failed its smoke test");
    }
    Ok(())
}

fn write_marker(root: &Path, artifact: &CatalogArtifact, tag: &str) -> Result<()> {
    fs::write(
        root.join(".locallmos-llamacpp"),
        format!(
            "schema=1\nbackend={}\ntag={}\nasset={}\nsha256={}\n",
            artifact.backend, tag, artifact.name, artifact.sha256
        ),
    )?;
    Ok(())
}

fn emit(
    app: &AppHandle,
    phase: &'static str,
    tag: &str,
    downloaded_bytes: u64,
    total_bytes: u64,
    message: Option<String>,
) {
    app.emit(
        UPDATE_EVENT,
        UpdateProgress {
            phase,
            tag: tag.into(),
            downloaded_bytes,
            total_bytes,
            message,
        },
    )
    .ok();
}

fn parse_build(tag: &str) -> Option<u64> {
    tag.strip_prefix('b')?.parse().ok()
}

fn is_newer_build(candidate: &str, current: &str) -> bool {
    matches!((parse_build(candidate), parse_build(current)), (Some(a), Some(b)) if a > b)
}

/// Recover the only unsafe crash window: after the current tree was renamed but
/// before the staged tree took its place. Completed swaps keep the new tree.
pub fn recover_interrupted_update() {
    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/opt/locallmos/llama"));
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join("LocalLMOS").join("llama"));
    }
    for root in candidates {
        let Some(parent) = root.parent() else { continue };
        let journal_path = parent.join(".locallmos-llamacpp-update.json");
        let Ok(bytes) = fs::read(&journal_path) else { continue };
        let Ok(journal) = serde_json::from_slice::<TransactionJournal>(&bytes) else { continue };
        if !journal.root.exists() && journal.backup.exists() {
            if fs::rename(&journal.backup, &journal.root).is_ok() {
                tracing::warn!("restored interrupted llama.cpp update for {}", journal.tag);
            }
        } else if journal.root.exists() && journal.backup.exists() {
            let failed = parent.join(".locallmos-llamacpp-interrupted");
            let _ = fs::rename(&journal.root, &failed);
            if fs::rename(&journal.backup, &journal.root).is_ok() {
                tracing::warn!("rolled back interrupted llama.cpp update for {}", journal.tag);
                let _ = fs::remove_dir_all(failed);
            }
        }
        let _ = fs::remove_file(journal_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_numeric_build_tags() {
        assert!(is_newer_build("b10344", "b9999"));
        assert!(!is_newer_build("b100", "b100"));
        assert!(!is_newer_build("latest", "b100"));
    }

    #[test]
    fn rejects_parent_paths_in_zip_components() {
        let unsafe_path = Path::new("../llama-server");
        assert!(unsafe_path.components().any(|c| !matches!(c, Component::Normal(_))));
    }

    #[test]
    fn selects_only_the_installed_backend_for_this_platform() {
        let catalog = Catalog {
            schema_version: 1,
            channel: "stable".into(),
            tag: "b10353".into(),
            artifacts: vec![CatalogArtifact {
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                backend: "cpu".into(),
                variant: None,
                archive: "tar.gz".into(),
                name: "llama.tar.gz".into(),
                url: "https://example.invalid/llama.tar.gz".into(),
                sha256: "00".repeat(32),
                size_bytes: 1,
                companions: Vec::new(),
            }],
        };
        let install = ManagedInstallation {
            root: PathBuf::from("/tmp/llama"),
            bin: PathBuf::from("/tmp/llama/llama-server"),
            backend: "cpu".into(),
            tag: Some("b10087".into()),
        };
        assert_eq!(select_artifact(&catalog, &install).unwrap().backend, "cpu");
        let wrong = ManagedInstallation { backend: "vulkan".into(), ..install };
        assert!(select_artifact(&catalog, &wrong).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn system_install_is_notification_only() {
        let install = ManagedInstallation {
            root: PathBuf::from("/opt/locallmos/llama"),
            bin: PathBuf::from("/opt/locallmos/llama/llama-server"),
            backend: "cuda".into(),
            tag: Some("b10087".into()),
        };
        let (installable, reason) = installability(&install);
        assert!(!installable);
        assert!(reason.unwrap().contains("service installer"));
    }
}
