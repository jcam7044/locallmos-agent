import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import rehypeRaw from "rehype-raw";
import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import remarkGfm from "remark-gfm";
import { deleteLocalModel, getModelLoadSettings, hubCancelDownload, hubGetAuthorAvatars, hubGetModel, hubListDownloads, hubSearchModels, hubStartDownload, listGpuDevices, loadModel, ollamaPullModel, saveModelLoadSettings, setGpuDefault, unloadModel } from "../api";
import type {
  DownloadState,
  GgufVariant,
  GpuDevice,
  GpuDeviceList,
  GpuPlan,
  SplitMode,
  HubModelDetail,
  HubModelSummary,
  LocalModel,
  LocalStatus,
  ModelLoadSettings,
  OllamaPullProgress,
} from "../types";
import { assessFit, chooseVariant, formatBytes } from "./fit";
import "./models.css";

type Mode = "discover" | "device";
type Sort = "trending" | "downloads" | "likes" | "newest";
type Capability = "all" | "text" | "vision";
type LocalModelAction = "load" | "eject" | "remove";

export const modelCardSchema = {
  ...defaultSchema,
  attributes: {
    ...defaultSchema.attributes,
    a: [...(defaultSchema.attributes?.a ?? []), "href", "title"],
    img: [...(defaultSchema.attributes?.img ?? []), "src", "alt", "title", "width", "height"],
    p: [...(defaultSchema.attributes?.p ?? []), "align"],
    div: [...(defaultSchema.attributes?.div ?? []), "align"],
  },
};

export function ModelsView({ local, onChanged }: { local: LocalStatus | null; onChanged: () => void }) {
  const [mode, setMode] = useState<Mode>("discover");
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [sort, setSort] = useState<Sort>("trending");
  const [capability, setCapability] = useState<Capability>("all");
  const [models, setModels] = useState<HubModelSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<HubModelDetail | null>(null);
  const [variantId, setVariantId] = useState<string | null>(null);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loadingList, setLoadingList] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [loadingDetail, setLoadingDetail] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [downloads, setDownloads] = useState<Record<string, DownloadState>>({});
  const [authorAvatars, setAuthorAvatars] = useState<Record<string, string>>({});
  const [modelAction, setModelAction] = useState<{ id: string; type: LocalModelAction } | null>(null);
  const [settingsModel, setSettingsModel] = useState<LocalModel | null>(null);
  const [removalModel, setRemovalModel] = useState<LocalModel | null>(null);
  const [gpuDefaults, setGpuDefaults] = useState(false);
  const request = useRef(0);
  const isOllama = local?.runtime.kind === "ollama";

  useEffect(() => {
    const timer = window.setTimeout(() => setDebounced(query.trim()), 320);
    return () => window.clearTimeout(timer);
  }, [query]);

  useEffect(() => {
    let disposed = false;
    hubListDownloads().then((items) => {
      if (!disposed) setDownloads(Object.fromEntries(items.map((d) => [`${d.repoId}:${d.variantId}`, d])));
    }).catch(() => undefined);
    let unlisten: (() => void) | undefined;
    void listen<DownloadState>("model-download", ({ payload }) => {
      setDownloads((all) => ({ ...all, [`${payload.repoId}:${payload.variantId}`]: payload }));
      if (payload.status === "complete") void onChanged();
    }).then((fn) => { unlisten = fn; });
    return () => { disposed = true; unlisten?.(); };
  }, [onChanged]);

  const loadPage = async (cursor?: string | null) => {
    const token = ++request.current;
    cursor ? setLoadingMore(true) : setLoadingList(true);
    setError(null);
    try {
      const page = isWebPreview()
        ? mockPage()
        : await hubSearchModels({ query: debounced, capability, sort, cursor });
      if (token !== request.current) return;
      setModels((current) => cursor ? [...current, ...page.items] : page.items);
      setNextCursor(page.nextCursor);
      if (!cursor) setSelectedId((current) => page.items.some((m) => m.id === current) ? current : page.items[0]?.id ?? null);
    } catch (e) {
      if (token === request.current) setError(readError(e));
    } finally {
      if (token === request.current) { setLoadingList(false); setLoadingMore(false); }
    }
  };

  useEffect(() => { if (mode === "discover" && local && !isOllama) void loadPage(); }, [debounced, capability, sort, mode, local?.runtime.kind]);

  useEffect(() => {
    if (isWebPreview() || !models.length) return;
    let disposed = false;
    void hubGetAuthorAvatars([...new Set(models.map((model) => model.author))])
      .then((avatars) => !disposed && setAuthorAvatars((current) => ({ ...current, ...avatars })))
      .catch(() => undefined);
    return () => { disposed = true; };
  }, [models]);

  useEffect(() => {
    if (!selectedId || mode !== "discover") return;
    let disposed = false;
    setLoadingDetail(true);
    setDetail(null);
    setVariantId(null);
    (isWebPreview() ? Promise.resolve(mockDetail(selectedId)) : hubGetModel(selectedId))
      .then((model) => {
        if (disposed) return;
        setDetail(model);
        setVariantId(chooseVariant(model.variants, local)?.id ?? null);
      })
      .catch((e) => !disposed && setError(readError(e)))
      .finally(() => !disposed && setLoadingDetail(false));
    return () => { disposed = true; };
  }, [selectedId, mode, local?.telemetry.memoryTotalBytes]);

  const selectedVariant = detail?.variants.find((v) => v.id === variantId) ?? null;
  const selectedLocalModel = selectedVariant && detail
    ? local?.models.find((model) => isVariantOnDevice(model, detail.id, selectedVariant.id))
    : undefined;
  const activeDownload = detail && selectedVariant ? downloads[`${detail.id}:${selectedVariant.id}`] : undefined;
  const installed = !!(detail && selectedVariant && (
    local?.models.some((m) => m.sourceRepo === detail.id && m.variantId === selectedVariant.id) ||
    activeDownload?.status === "complete"
  ));
  const diskEnough = !selectedVariant || local?.modelsStorage.availableBytes == null ||
    local.modelsStorage.availableBytes >= selectedVariant.sizeBytes;

  const startDownload = async () => {
    if (!detail || !selectedVariant || installed || !diskEnough) return;
    setError(null);
    try {
      const state = await hubStartDownload(detail.id, detail.revision, selectedVariant.id);
      setDownloads((all) => ({ ...all, [`${state.repoId}:${state.variantId}`]: state }));
    } catch (e) { setError(readError(e)); }
  };

  const cancelDownload = async (download: DownloadState) => {
    setError(null);
    try {
      const state = await hubCancelDownload(download.id);
      setDownloads((all) => ({ ...all, [state.repoId + ":" + state.variantId]: state }));
    } catch (e) { setError(readError(e)); }
  };

  const load = async (model: LocalModel) => {
    setError(null);
    setModelAction({ id: model.id, type: "load" });
    try {
      await loadModel(model.id);
      await onChanged();
    } catch (e) { setError(readError(e));
    } finally { setModelAction(null); }
  };

  const eject = async (model: LocalModel) => {
    setError(null);
    setModelAction({ id: model.id, type: "eject" });
    try {
      await unloadModel(model.id);
      await onChanged();
    } catch (e) { setError(readError(e));
    } finally { setModelAction(null); }
  };

  const remove = async (model: LocalModel) => {
    setRemovalModel(model);
  };

  const confirmRemove = async () => {
    const model = removalModel;
    if (!model) return;
    setRemovalModel(null);
    setError(null);
    setModelAction({ id: model.id, type: "remove" });
    try {
      await deleteLocalModel(model.id);
      await onChanged();
    } catch (e) { setError(readError(e));
    } finally { setModelAction(null); }
  };

  const selectDevice = (model: LocalModel) => {
    if (model.sourceRepo) {
      setMode("discover");
      setSelectedId(model.sourceRepo);
    }
  };

  return (
    <main className="hub-shell">
      <header className="hub-header">
        <div>
          <h2>Models</h2>
          <p>{isOllama ? "Pull, load, and remove models managed by Ollama." : "Discover GGUF models, compare quantizations, and download them for llama.cpp."}</p>
        </div>
        <HardwareChips local={local} />
      </header>

      <div className="hub-toolbar">
        <div className="hub-segmented" aria-label="Model source">
          <button className={mode === "discover" ? "active" : ""} onClick={() => setMode("discover")}>{isOllama ? "Pull from Ollama" : "Discover"}</button>
          <button className={mode === "device" ? "active" : ""} onClick={() => setMode("device")}>On Device <span>{local?.models.length ?? 0}</span></button>
        </div>
        {mode === "discover" && !isOllama && <>
          <label className="hub-search"><span>⌕</span><input aria-label="Search all models" placeholder="Search all models" value={query} onChange={(e) => setQuery(e.target.value)} /></label>
          <select aria-label="Capability" value={capability} onChange={(e) => setCapability(e.target.value as Capability)}>
            <option value="all">All capabilities</option><option value="text">Text</option><option value="vision">Vision</option>
          </select>
          <select aria-label="Sort models" value={sort} onChange={(e) => setSort(e.target.value as Sort)}>
            <option value="trending">Trending</option><option value="downloads">Most downloaded</option><option value="likes">Most liked</option><option value="newest">Newest</option>
          </select>
        </>}
        {mode === "device" && local?.runtime.kind === "llamacpp" && <>
          <span className="hub-toolbar-spacer" />
          <button className="hub-gpu-defaults-btn" onClick={() => setGpuDefaults(true)}>GPU defaults</button>
        </>}
      </div>

      {error && <div className="hub-error"><span>{error}</span>{!isOllama && <button onClick={() => void loadPage()}>Retry</button>}</div>}

      {mode === "discover" && isOllama ? <OllamaPull onChanged={onChanged} /> : mode === "discover" ? (
        <div className={`hub-workspace ${selectedId ? "has-selection" : ""}`}>
          <section className="hub-list-pane">
            <div className="hub-pane-title"><strong>{debounced ? `Results for “${debounced}”` : "Popular GGUF Models"}</strong><span>{models.length}</span></div>
            {loadingList ? <ListSkeleton /> : models.length ? (
              <div className="hub-model-list">
                {models.map((model) => <ModelRow key={model.id} model={model} avatarUrl={authorAvatars[model.author]} active={model.id === selectedId} onClick={() => setSelectedId(model.id)} />)}
                {nextCursor && <button className="hub-load-more" disabled={loadingMore} onClick={() => void loadPage(nextCursor)}>{loadingMore ? "Loading…" : "Load more"}</button>}
              </div>
            ) : <Empty title="No GGUF models found" body="Try a broader search or another capability filter." />}
          </section>
          <section className="hub-detail-pane">
            {selectedId && <button className="hub-back" onClick={() => setSelectedId(null)}>← Back to models</button>}
            {loadingDetail ? <DetailSkeleton /> : detail ? (
              <ModelDetail
                detail={detail}
                variant={selectedVariant}
                variantId={variantId}
                onVariant={setVariantId}
                local={local}
                download={activeDownload}
                installed={installed}
                diskEnough={diskEnough}
                avatarUrl={modelLogo(detail) ?? authorAvatars[detail.author]}
                onDownload={() => void startDownload()}
                onCancelDownload={cancelDownload}
                localModel={selectedLocalModel}
                action={localModelAction(modelAction, selectedLocalModel?.id)}
                onLoad={load}
                onEject={eject}
                onRemove={remove}
                onSettings={local?.runtime.kind === "llamacpp" ? setSettingsModel : undefined}
              />
            ) : <Empty title="Select a model" body="Choose a repository to compare its available GGUF downloads." />}
          </section>
        </div>
      ) : <OnDevice models={local?.models ?? []} runtimeKind={local?.runtime.kind} onSelect={selectDevice} action={modelAction} onLoad={load} onEject={eject} onRemove={remove} onSettings={local?.runtime.kind === "llamacpp" ? setSettingsModel : undefined} />}
      {settingsModel && <ModelLoadSettingsDialog model={settingsModel} onClose={() => setSettingsModel(null)} onChanged={onChanged} />}
      {gpuDefaults && <GpuDefaultsDialog onClose={() => setGpuDefaults(false)} />}
      {removalModel && <ModelRemovalDialog model={removalModel} onCancel={() => setRemovalModel(null)} onConfirm={() => void confirmRemove()} />}
    </main>
  );
}

function OllamaPull({ onChanged }: { onChanged: () => void }) {
  const [model, setModel] = useState("");
  const [pulling, setPulling] = useState(false);
  const [progress, setProgress] = useState<OllamaPullProgress | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<OllamaPullProgress>("ollama-pull", ({ payload }) => setProgress(payload))
      .then((stop) => { unlisten = stop; });
    return () => unlisten?.();
  }, []);

  const pull = async () => {
    const name = model.trim();
    if (!name || pulling) return;
    setPulling(true);
    setError(null);
    setProgress({ model: name, status: "starting", completed: null, total: null });
    try {
      await ollamaPullModel(name);
      setProgress({ model: name, status: "success", completed: 1, total: 1 });
      setModel("");
      await onChanged();
    } catch (cause) {
      setError(readError(cause));
    } finally {
      setPulling(false);
    }
  };
  const percent = progress?.total && progress.completed != null
    ? Math.min(100, Math.round(progress.completed / progress.total * 100))
    : null;

  return <section className="ollama-pull-panel">
    <div className="ollama-pull-card">
      <span className="ollama-mark">O</span>
      <div>
        <h3>Pull from the Ollama registry</h3>
        <p>Enter a model name and optional tag, just as you would with <code>ollama pull</code>.</p>
      </div>
      <form onSubmit={(event) => { event.preventDefault(); void pull(); }}>
        <input aria-label="Ollama model name" autoCapitalize="none" autoCorrect="off" spellCheck={false} placeholder="e.g. qwen3:8b" value={model} onChange={(event) => setModel(event.target.value)} disabled={pulling} />
        <button type="submit" disabled={!model.trim() || pulling}>{pulling ? "Pulling…" : "Pull model"}</button>
      </form>
      <div className="ollama-examples"><span>Examples</span>{["qwen3:8b", "gemma3:4b", "llama3.2:3b"].map((name) => <button key={name} disabled={pulling} onClick={() => setModel(name)}>{name}</button>)}</div>
      {progress && (pulling || progress.status === "success") && <div className="ollama-pull-progress">
        <div><strong>{progress.model}</strong><span>{progress.status === "success" ? "Pulled" : progress.status}{percent != null && progress.status !== "success" ? ` · ${percent}%` : ""}</span></div>
        <div className={percent == null && pulling ? "indeterminate" : ""}><i style={{ width: `${percent ?? (progress.status === "success" ? 100 : 30)}%` }} /></div>
      </div>}
      {error && <p className="hub-inline-error">{error}</p>}
    </div>
    <p className="ollama-pull-note">Ollama owns the model files and selects the appropriate format. Installed models appear under <b>On Device</b>.</p>
  </section>;
}

function HardwareChips({ local }: { local: LocalStatus | null }) {
  const t = local?.telemetry;
  const vram = (t?.gpus ?? []).filter((g) => g.vendor !== "apple").reduce((n, g) => n + (g.memoryTotalBytes ?? 0), 0);
  return <div className="hub-hardware">
    <span><i className="green" />{vram ? `${formatBytes(vram)} VRAM` : "CPU only"}</span>
    <span><i className="blue" />{formatBytes(t?.memoryTotalBytes)} RAM</span>
    <span><i />{local?.runtime.contextSize ? `${local.runtime.contextSize.toLocaleString()} ctx` : "Auto context"}</span>
    <span><i />{formatBytes(local?.modelsStorage.availableBytes)} free</span>
  </div>;
}

function ModelRow({ model, avatarUrl, active, onClick }: { model: HubModelSummary; avatarUrl?: string; active: boolean; onClick: () => void }) {
  return <button className={`hub-model-row ${active ? "active" : ""}`} onClick={onClick}>
    <Avatar model={model} overrideUrl={avatarUrl} />
    <span className="hub-row-copy"><strong>{model.name}</strong><small>{model.author}</small></span>
    <span className="hub-row-stats"><small>♡ {compact(model.likes)}</small><small>⇩ {compact(model.downloads)}</small><small>{relativeDate(model.lastModified)}</small></span>
  </button>;
}

function Avatar({ model, overrideUrl }: { model: HubModelSummary; overrideUrl?: string | null }) {
  const [failed, setFailed] = useState(false);
  const url = overrideUrl || model.avatarUrl;
  useEffect(() => setFailed(false), [url]);
  return failed || !url ? <span className="hub-avatar fallback">{model.author.slice(0, 1).toUpperCase()}</span> :
    <img className="hub-avatar" src={url} alt="" onError={() => setFailed(true)} />;
}

function ModelDetail({ detail, variant, variantId, onVariant, local, download, installed, diskEnough, avatarUrl, onDownload, onCancelDownload, localModel, action, onLoad, onEject, onRemove, onSettings }: {
  detail: HubModelDetail; variant: GgufVariant | null; variantId: string | null; onVariant: (id: string) => void;
  local: LocalStatus | null; download?: DownloadState; installed: boolean; diskEnough: boolean; avatarUrl?: string | null; onDownload: () => void; onCancelDownload: (download: DownloadState) => Promise<void>;
  localModel?: LocalModel; action: LocalModelAction | null; onLoad: (model: LocalModel) => Promise<void>; onEject: (model: LocalModel) => Promise<void>; onRemove: (model: LocalModel) => Promise<void>;
  onSettings?: (model: LocalModel) => void;
}) {
  const fit = variant ? assessFit(variant, local) : null;
  const downloading = download?.status === "queued" || download?.status === "downloading" || download?.status === "canceling";
  const canceling = download?.status === "canceling";
  const progress = download?.totalBytes ? Math.round(download.downloadedBytes / download.totalBytes * 100) : 0;
  const downloadedVariantIds = new Set(
    (local?.models ?? [])
      .filter((model) => model.sourceRepo === detail.id && model.variantId)
      .map((model) => model.variantId!),
  );
  const capabilityTags = [...new Set([detail.pipelineTag, ...detail.tags])]
    .filter((tag): tag is string => !!tag && ["conversational", "text-generation", "image-text-to-text", "tools"].includes(tag))
    .slice(0, 4);
  return <div className="hub-detail">
    <div className="hub-detail-heading"><Avatar model={detail} overrideUrl={avatarUrl} /><div><h3>{detail.name}</h3><a href={`https://huggingface.co/${detail.id}`} target="_blank" rel="noreferrer">{detail.author} ↗</a></div></div>
    <div className="hub-tags">
      {capabilityTags.map((t) => <span key={t}>{prettyTag(t)}</span>)}
      {detail.baseModels[0] && <span>Base · {detail.baseModels[0]}</span>}
    </div>
    <div className="hub-download-card">
      <div className="hub-variant-row">
        <div className="hub-variant-select"><span className="hub-fit-dot" style={{ borderColor: fit?.color, color: fit?.color }}>✓</span>
          <VariantPicker variants={detail.variants} value={variantId} onChange={onVariant} downloadedVariantIds={downloadedVariantIds} />
        </div>
        {downloading && download ? <button className="hub-download-button hub-cancel-download" disabled={canceling} onClick={() => void onCancelDownload(download)}>{canceling ? "Cancelling…" : "Cancel download"}</button> :
          <button className="hub-download-button" disabled={!variant || installed || !diskEnough} onClick={onDownload}>
            {installed ? "✓ On Device" : !diskEnough ? "Not enough disk" : "⇩ Download"}
          </button>}
      </div>
      {fit && <div className="hub-fit-line"><strong style={{ color: fit.color }}>{fit.label}</strong><span>{fit.detail}</span>{fit.freeMemoryWarning && <em>Free memory before loading.</em>}</div>}
      {downloading && <div className="hub-progress"><i style={{ width: `${progress}%` }} /></div>}
      {download?.status === "error" && <p className="hub-inline-error">{download.error}</p>}
      {variant?.companions.length ? <p className="hub-companion">Vision adapter available ({variant.companions.map((f) => f.path.split("/").pop()).join(", ")}); automatic mmproj loading is not enabled yet.</p> : null}
    </div>
    <div className="hub-meta">
      <span>Updated <b>{relativeDate(detail.lastModified)}</b></span><span>Downloads <b>{compact(detail.downloads)}</b></span><span>Likes <b>{compact(detail.likes)}</b></span><span>License <b>{detail.license ?? "Not specified"}</b></span>
    </div>
    {localModel && <ModelAction model={localModel} action={action} onLoad={onLoad} onEject={onEject} onRemove={onRemove} onSettings={onSettings} />}
    <ModelCardReadme detail={detail} />
  </div>;
}

function VariantPicker({ variants, value, onChange, downloadedVariantIds }: {
  variants: GgufVariant[]; value: string | null; onChange: (id: string) => void; downloadedVariantIds: ReadonlySet<string>;
}) {
  const [open, setOpen] = useState(false);
  const ordered = variantsBySizeAscending(variants);
  const selected = variants.find((variant) => variant.id === value) ?? variants[0];
  if (!selected) return null;
  const select = (id: string) => { onChange(id); setOpen(false); };
  return <div className="hub-variant-picker">
    <button type="button" className="hub-variant-trigger" aria-label="GGUF quantization" aria-expanded={open} onClick={() => setOpen((shown) => !shown)}>
      <span className="hub-variant-copy"><b>{selected.quantization}</b>{downloadedVariantIds.has(selected.id) && <small>Downloaded</small>}</span>
      <span className="hub-variant-size">{formatBytes(selected.sizeBytes)}{selected.files.length > 1 ? ` · ${selected.files.length} shards` : ""}</span><span aria-hidden="true">⌄</span>
    </button>
    {open && <div className="hub-variant-menu" role="listbox" aria-label="GGUF quantizations">
      {ordered.map((candidate) => <button type="button" role="option" aria-selected={candidate.id === selected.id} key={candidate.id} onClick={() => select(candidate.id)}>
        <span className="hub-variant-copy"><b>{candidate.quantization}</b>{downloadedVariantIds.has(candidate.id) && <small>Downloaded</small>}</span>
        <span className="hub-variant-size">{formatBytes(candidate.sizeBytes)}{candidate.files.length > 1 ? ` · ${candidate.files.length} shards` : ""}</span>
      </button>)}
    </div>}
  </div>;
}

export function ModelCardReadme({ detail }: { detail: HubModelDetail }) {
  return <article className="hub-readme">
    {detail.readmeMarkdown ? <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeRaw, [rehypeSanitize, modelCardSchema]]} components={{
      a: ({ href, children }) => <a href={resolveCardUrl(href, detail, "blob")} target="_blank" rel="noreferrer">{children}</a>,
      img: ({ src, alt, title, width, height }) => <img
        src={resolveCardUrl(src, detail, "resolve")}
        alt={alt ?? ""}
        title={title}
        width={width}
        height={height}
        loading="lazy"
      />,
    }}>{stripFrontMatter(detail.readmeMarkdown)}</ReactMarkdown> : <Empty title="No model card" body="This repository does not provide a README." />}
  </article>;
}

function OnDevice({ models, runtimeKind, onSelect, action, onLoad, onEject, onRemove, onSettings }: { models: LocalModel[]; runtimeKind?: string; onSelect: (model: LocalModel) => void; action: { id: string; type: LocalModelAction } | null; onLoad: (model: LocalModel) => Promise<void>; onEject: (model: LocalModel) => Promise<void>; onRemove: (model: LocalModel) => Promise<void>; onSettings?: (model: LocalModel) => void }) {
  return <section className="hub-device">
    <div className="hub-pane-title"><strong>Models on this device</strong><span>{models.length}</span></div>
    {models.length ? <div className="hub-device-grid">{models.map((model) => <article key={model.id} className="hub-device-card">
      <button className="hub-device-select" onClick={() => onSelect(model)} disabled={!model.sourceRepo}>
        <span className="hub-device-icon">◫</span><span><strong>{model.name}</strong><small>{model.sourceRepo ?? (runtimeKind === "ollama" ? "Ollama" : "Local GGUF")}</small><em>{model.quantization ?? (runtimeKind === "ollama" ? "Managed model" : "GGUF")} · {formatBytes(model.sizeBytes)}</em></span><b className={model.loaded ? "loaded" : ""}>{model.loaded ? "Loaded" : "On Device"}</b>
      </button>
      <ModelAction model={model} action={localModelAction(action, model.id)} onLoad={onLoad} onEject={onEject} onRemove={onRemove} onSettings={onSettings} canRemove compact />
    </article>)}</div> : <Empty title="No models on device" body={runtimeKind === "ollama" ? "Pull a model from Ollama to get started." : "Download a GGUF model or place one in the llama.cpp models directory."} />}
  </section>;
}

function ModelAction({ model, action, onLoad, onEject, onRemove, onSettings, canRemove = false, compact = false }: { model: LocalModel; action: LocalModelAction | null; onLoad: (model: LocalModel) => Promise<void>; onEject: (model: LocalModel) => Promise<void>; onRemove: (model: LocalModel) => Promise<void>; onSettings?: (model: LocalModel) => void; canRemove?: boolean; compact?: boolean }) {
  const removable = isLocalModelRemovable(model, canRemove);
  const busy = action != null;
  const loading = action === "load";
  const ejecting = action === "eject";
  return <div className={`hub-model-actions ${compact ? "compact" : ""}`}>
    <div className="hub-model-action-buttons">
      {loading || !model.loaded ? <button className="hub-load-button" disabled={busy} onClick={() => void onLoad(model)}>{loading ? "Loading…" : "Load"}</button> : <button className="hub-eject-button" disabled={busy} onClick={() => void onEject(model)}>{ejecting ? "Ejecting…" : "Eject"}</button>}
      {onSettings && <button className="hub-settings-button" disabled={busy} title="Model load settings" aria-label={`Load settings for ${model.name}`} onClick={() => onSettings(model)}>⚙ Settings</button>}
      {removable && <button className="hub-remove-button" disabled={busy || model.loaded} title={model.loaded ? "Eject this model before removing it" : "Remove downloaded model files"} onClick={() => void onRemove(model)}>{action === "remove" ? "Removing…" : "Remove"}</button>}
    </div>
    <small>{loading ? "Loading this model into memory." : ejecting ? "Releasing model memory; files stay on disk." : model.loaded ? "Releases model memory; files stay on disk." : "Loads this model into memory."}</small>
  </div>;
}

export function ModelRemovalDialog({ model, onCancel, onConfirm }: { model: LocalModel; onCancel: () => void; onConfirm: () => void }) {
  return <div className="hub-settings-backdrop" role="presentation" onMouseDown={onCancel}>
    <section className="hub-removal-dialog" role="dialog" aria-modal="true" aria-labelledby="model-removal-title" onMouseDown={(event) => event.stopPropagation()}>
      <header>
        <h3 id="model-removal-title">Remove model?</h3>
        <button aria-label="Cancel removing model" onClick={onCancel}>×</button>
      </header>
      <div>
        <p>Remove <strong>{model.name}</strong> from this device?</p>
        <p className="hub-removal-warning">This permanently deletes {formatBytes(model.sizeBytes)} of downloaded model files. This cannot be undone.</p>
      </div>
      <footer>
        <span />
        <button onClick={onCancel}>Cancel</button>
        <button className="danger" onClick={onConfirm}>Remove model</button>
      </footer>
    </section>
  </div>;
}

export function localModelAction(action: { id: string; type: LocalModelAction } | null, modelId: string | undefined): LocalModelAction | null {
  return action && action.id === modelId ? action.type : null;
}

export function isLocalModelRemovable(model: LocalModel, runtimeManaged = false) {
  // Every model shown here was found in the active runtime's local model store.
  // The backend resolves the stable id again before deleting files, so models
  // downloaded outside the Hub (and older Hub downloads without a manifest)
  // are removable too.
  return runtimeManaged || !!model.id;
}

export const recommendedModelLoadSettings = (): ModelLoadSettings => ({
  contextSize: null,
  kvCacheType: "auto",
  gpuOffload: "auto",
  flashAttention: "auto",
  cpuThreads: null,
  speculativeDecoding: "auto",
  maxToolCalls: null,
  gpuPlan: null,
});

export function isRecommendedModelLoadSettings(settings: ModelLoadSettings) {
  return settings.contextSize == null && settings.kvCacheType === "auto" &&
    settings.gpuOffload === "auto" && settings.flashAttention === "auto" &&
    settings.cpuThreads == null && settings.speculativeDecoding === "auto" &&
    settings.maxToolCalls == null && settings.gpuPlan == null;
}

export function modelLoadSettingsError(settings: ModelLoadSettings): string | null {
  const planError = gpuPlanError(settings.gpuPlan);
  if (planError) return planError;
  if (settings.contextSize != null && (!Number.isInteger(settings.contextSize) || settings.contextSize < 512 || settings.contextSize > 1_048_576)) {
    return "Context size must be a whole number between 512 and 1,048,576 tokens.";
  }
  if (settings.cpuThreads != null && (!Number.isInteger(settings.cpuThreads) || settings.cpuThreads < 1 || settings.cpuThreads > 512)) {
    return "CPU threads must be a whole number between 1 and 512.";
  }
  if (settings.maxToolCalls != null && (!Number.isInteger(settings.maxToolCalls) || settings.maxToolCalls < 1 || settings.maxToolCalls > 100)) {
    return "Max tool calls must be a whole number between 1 and 100.";
  }
  return null;
}

/** Light validation for a GPU plan; the editor prevents most invalid states. */
function gpuPlanError(plan: GpuPlan | null): string | null {
  if (plan == null) return null;
  if (plan.devices.length === 0) return "Select at least one GPU, or switch back to the default.";
  if (plan.tensorSplit != null) {
    if (plan.tensorSplit.length !== plan.devices.length) return "Tensor split must have one weight per selected GPU.";
    if (plan.tensorSplit.some((w) => !(w >= 0) || !Number.isFinite(w))) return "Tensor split weights must be non-negative numbers.";
    if (plan.tensorSplit.reduce((a, b) => a + b, 0) <= 0) return "At least one tensor split weight must be greater than zero.";
  }
  return null;
}

const SPLIT_MODE_LABELS: Record<SplitMode, string> = {
  auto: "Recommended (layer split)",
  layer: "By layer",
  row: "By row",
  single: "Single GPU",
};

/**
 * GPU usage editor shared by the per-model settings dialog and the rig-default
 * editor. `value === null` means "inherit" (rig default for a model, automatic
 * selection for the rig default itself); a non-null plan is fully explicit.
 * Editing any control produces a normalized explicit plan (leaving inherit mode),
 * keeps at least one GPU enabled, and re-aligns main-GPU / tensor-split (which are
 * positional) to the enabled device list.
 */
function GpuPlanEditor({ devices, value, onChange, inheritLabel, inheritHint, effectiveWhenInherited }: {
  devices: GpuDevice[];
  value: GpuPlan | null;
  onChange: (value: GpuPlan | null) => void;
  inheritLabel: string;
  inheritHint: string;
  effectiveWhenInherited: GpuPlan | null;
}) {
  if (devices.length === 0) {
    return <p className="hub-gpu-empty">Automatic — no selectable GPUs were detected for this runtime.</p>;
  }
  const allTokens = devices.map((d) => d.token);
  const nameOf = (token: string) => devices.find((d) => d.token === token)?.name ?? token;
  const inherited = value == null;

  // Normalize a plan to the available devices (drop unknown, keep device order,
  // ≥1) and re-align main-GPU / tensor-split to that device set.
  const normalize = (plan: GpuPlan): GpuPlan => {
    const kept = allTokens.filter((t) => plan.devices.includes(t));
    const devs = kept.length ? kept : allTokens;
    const mainToken = plan.mainGpu != null ? plan.devices[plan.mainGpu] : null;
    const mainIdx = mainToken != null ? devs.indexOf(mainToken) : -1;
    let tensorSplit: number[] | null = null;
    if (plan.tensorSplit != null) {
      const weight = new Map(plan.devices.map((t, i) => [t, plan.tensorSplit![i] ?? 1]));
      tensorSplit = devs.map((t) => weight.get(t) ?? 1);
    }
    return { devices: devs, splitMode: plan.splitMode, mainGpu: mainIdx >= 0 ? mainIdx : null, tensorSplit };
  };

  // The explicit plan edits build on: the current value, or the effective plan
  // shown while inheriting (rig default, or "all devices / auto" as a fallback).
  const shown: GpuPlan = normalize(value ?? effectiveWhenInherited ?? { devices: allTokens, splitMode: "auto", mainGpu: null, tensorSplit: null });
  const enabled = shown.devices;
  const multi = enabled.length >= 2;
  const update = (plan: GpuPlan) => onChange(normalize(plan));

  const toggleDevice = (token: string) => {
    const has = enabled.includes(token);
    const next = allTokens.filter((t) => (t === token ? !has : enabled.includes(t)));
    if (next.length === 0) return; // keep at least one GPU enabled
    update({ ...shown, devices: next });
  };

  return <div className={`hub-gpu-list ${inherited ? "inherited" : ""}`}>
    <label className="hub-gpu-inherit">
      <input type="checkbox" checked={inherited} onChange={(e) => onChange(e.target.checked ? null : shown)} />
      <span>{inheritLabel}<small>{inheritHint}</small></span>
    </label>
    {devices.map((d, i) => {
      const on = enabled.includes(d.token);
      return <div key={d.token} className="hub-gpu-row">
        <div className="hub-gpu-id"><strong>GPU {i}: {d.name}</strong><small>{d.token}</small></div>
        <button type="button" role="switch" aria-checked={on} aria-label={`GPU ${i}: ${d.name}`}
          className={`hub-gpu-switch ${on ? "on" : ""}`} onClick={() => toggleDevice(d.token)}><span /></button>
      </div>;
    })}
    {multi && <div className="hub-gpu-advanced">
      <label className="hub-gpu-field"><span>Split mode <small>How the model is divided across the selected GPUs.</small></span>
        <select value={shown.splitMode} onChange={(e) => update({ ...shown, splitMode: e.target.value as SplitMode })}>
          {(Object.keys(SPLIT_MODE_LABELS) as SplitMode[]).map((m) => <option key={m} value={m}>{SPLIT_MODE_LABELS[m]}</option>)}
        </select>
      </label>
      <label className="hub-gpu-field"><span>Main GPU <small>Holds the KV cache / small tensors (row &amp; single-GPU modes).</small></span>
        <select value={shown.mainGpu ?? ""} onChange={(e) => update({ ...shown, mainGpu: e.target.value === "" ? null : Number(e.target.value) })}>
          <option value="">Recommended (first)</option>
          {enabled.map((t, i) => <option key={t} value={i}>GPU {allTokens.indexOf(t)}: {nameOf(t)}</option>)}
        </select>
      </label>
      {shown.splitMode !== "single" && <div className="hub-gpu-field hub-gpu-tensor">
        <span>Tensor split <small>Fraction of the model on each GPU. Off = even split.</small></span>
        <div className="hub-gpu-tensor-body">
          <label className="hub-gpu-tensor-toggle">
            <input type="checkbox" checked={shown.tensorSplit != null}
              onChange={(e) => update({ ...shown, tensorSplit: e.target.checked ? enabled.map(() => 1) : null })} />
            <span>Custom weights</span>
          </label>
          {shown.tensorSplit != null && <div className="hub-gpu-tensor-rows">
            {enabled.map((t, i) => <label key={t} className="hub-gpu-tensor-weight">
              <span>GPU {allTokens.indexOf(t)}</span>
              <input type="number" min={0} step="any" value={shown.tensorSplit![i]}
                onChange={(e) => { const next = [...shown.tensorSplit!]; next[i] = e.target.value === "" ? 0 : Number(e.target.value); update({ ...shown, tensorSplit: next }); }} />
            </label>)}
          </div>}
        </div>
      </div>}
    </div>}
  </div>;
}

/** Rig-wide default GPU plan editor, opened from the Models toolbar. */
function GpuDefaultsDialog({ onClose }: { onClose: () => void }) {
  const [info, setInfo] = useState<GpuDeviceList | null>(null);
  const [plan, setPlan] = useState<GpuPlan | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    void listGpuDevices()
      .then((value) => { if (!disposed) { setInfo(value); setPlan(value.defaultPlan); } })
      .catch((cause) => { if (!disposed) setError(readError(cause)); })
      .finally(() => { if (!disposed) setLoading(false); });
    return () => { disposed = true; };
  }, []);

  const save = async () => {
    const validationError = gpuPlanError(plan);
    if (validationError) { setError(validationError); return; }
    setSaving(true); setError(null);
    try { await setGpuDefault(plan); onClose(); }
    catch (cause) { setError(readError(cause)); }
    finally { setSaving(false); }
  };

  return <div className="hub-settings-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget && !saving) onClose(); }}>
    <section className="hub-settings-dialog" role="dialog" aria-modal="true" aria-labelledby="gpu-defaults-title">
      <header><div><span className={`hub-settings-state ${plan == null ? "recommended" : "custom"}`}>{plan == null ? "Automatic" : "Custom"}</span><h3 id="gpu-defaults-title">Default GPUs</h3><p>Applied to every model without its own GPU selection.</p></div><button aria-label="Close" disabled={saving} onClick={onClose}>×</button></header>
      {loading ? <div className="hub-settings-loading">Loading devices…</div> : <div className="hub-gpu-section">
        <GpuPlanEditor devices={info?.devices ?? []} value={plan} onChange={setPlan}
          inheritLabel="Automatic (recommended)" inheritHint="Prefers discrete GPUs and skips an integrated GPU when both are present." effectiveWhenInherited={null} />
      </div>}
      {error && <p className="hub-settings-error">{error}</p>}
      <footer><span /><button disabled={saving} onClick={onClose}>Cancel</button><button className="primary" disabled={loading || saving} onClick={() => void save()}>{saving ? "Saving…" : "Save"}</button></footer>
    </section>
  </div>;
}

function ModelLoadSettingsDialog({ model, onClose, onChanged }: { model: LocalModel; onClose: () => void; onChanged: () => void }) {
  const [settings, setSettings] = useState<ModelLoadSettings>(recommendedModelLoadSettings);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [gpuInfo, setGpuInfo] = useState<GpuDeviceList | null>(null);
  const supportsMtp = model.capabilities.includes("mtp");

  useEffect(() => {
    let disposed = false;
    setLoading(true);
    void Promise.all([getModelLoadSettings(model.id), listGpuDevices()])
      .then(([value, gpus]) => { if (!disposed) { setSettings(value); setGpuInfo(gpus); } })
      .catch((cause) => { if (!disposed) setError(readError(cause)); })
      .finally(() => { if (!disposed) setLoading(false); });
    return () => { disposed = true; };
  }, [model.id]);

  const patch = <K extends keyof ModelLoadSettings>(key: K, value: ModelLoadSettings[K]) => {
    setSettings((current) => ({ ...current, [key]: value }));
    setNotice(null);
    setError(null);
  };
  const save = async (loadNow: boolean) => {
    const validationError = modelLoadSettingsError(settings);
    if (validationError) { setError(validationError); return; }
    if (settings.speculativeDecoding === "mtp" && !supportsMtp) {
      setError("This GGUF does not contain embedded MTP prediction layers."); return;
    }
    setSaving(true); setError(null); setNotice(null);
    try {
      await saveModelLoadSettings(model.id, settings, loadNow);
      await onChanged();
      if (loadNow) onClose();
      else setNotice(model.loaded ? "Saved. Tool-call limits apply to the next message; reload to apply load settings." : "Settings saved for the next load.");
    } catch (cause) { setError(readError(cause));
    } finally { setSaving(false); }
  };

  return <div className="hub-settings-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget && !saving) onClose(); }}>
    <section className="hub-settings-dialog" role="dialog" aria-modal="true" aria-labelledby="model-load-settings-title">
      <header><div><span className={`hub-settings-state ${isRecommendedModelLoadSettings(settings) ? "recommended" : "custom"}`}>{isRecommendedModelLoadSettings(settings) ? "Recommended" : "Custom"}</span><h3 id="model-load-settings-title">Model load settings</h3><p>{model.name}</p></div><button aria-label="Close" disabled={saving} onClick={onClose}>×</button></header>
      {loading ? <div className="hub-settings-loading">Loading settings…</div> : <><div className="hub-settings-fields">
        <label><span>Context window <small>Memory grows with context length.</small></span><input type="number" min={512} max={1048576} step={512} placeholder="Recommended (auto-fit)" value={settings.contextSize ?? ""} onChange={(e) => patch("contextSize", e.target.value ? Number(e.target.value) : null)} /></label>
        <label><span>KV cache type <small>Lower precision saves memory but may reduce compatibility.</small></span><select value={settings.kvCacheType} onChange={(e) => patch("kvCacheType", e.target.value as ModelLoadSettings["kvCacheType"])}><option value="auto">Recommended (llama.cpp default)</option><option value="f16">f16 · highest compatibility</option><option value="q8_0">q8_0 · lower memory</option><option value="q4_0">q4_0 · lowest memory</option></select></label>
        <label><span>GPU offload <small>Auto-fit leaves memory headroom for the KV cache.</small></span><select value={settings.gpuOffload} onChange={(e) => patch("gpuOffload", e.target.value as ModelLoadSettings["gpuOffload"])}><option value="auto">Recommended (auto-fit)</option><option value="all">All model layers</option><option value="cpu_only">CPU only</option></select></label>
        <label><span>Flash Attention <small>Automatic mode uses it only when supported.</small></span><select value={settings.flashAttention} onChange={(e) => patch("flashAttention", e.target.value as ModelLoadSettings["flashAttention"])}><option value="auto">Recommended (automatic)</option><option value="on">On</option><option value="off">Off</option></select></label>
        <label><span>Speculative decoding <small>{supportsMtp ? "Embedded MTP heads detected; Recommended enables them with a safe two-token draft." : "No embedded MTP prediction layers were detected in this GGUF."}</small></span><select value={settings.speculativeDecoding} onChange={(e) => patch("speculativeDecoding", e.target.value as ModelLoadSettings["speculativeDecoding"])}><option value="auto">Recommended ({supportsMtp ? "embedded MTP" : "off"})</option><option value="off">Off</option><option value="mtp" disabled={!supportsMtp}>Embedded MTP</option></select></label>
        <label><span>Max tool calls per message <small>Stops runaway tool loops. Recommended allows 25 calls, then reserves a final answer pass.</small></span><input type="number" min={1} max={100} step={1} placeholder="Recommended (25)" value={settings.maxToolCalls ?? ""} onChange={(e) => patch("maxToolCalls", e.target.value ? Number(e.target.value) : null)} /></label>
        <label><span>CPU threads <small>Automatic mode lets llama.cpp choose.</small></span><input type="number" min={1} max={512} step={1} placeholder="Recommended (automatic)" value={settings.cpuThreads ?? ""} onChange={(e) => patch("cpuThreads", e.target.value ? Number(e.target.value) : null)} /></label>
      </div>
      <div className="hub-gpu-section">
        <div className="hub-gpu-heading"><span>GPUs</span><small>Restrict this model to specific GPUs. Rig default follows the toolbar's “GPU defaults”.</small></div>
        {settings.gpuOffload === "cpu_only"
          ? <p className="hub-gpu-empty">GPU offload is set to CPU only — no GPUs will be used.</p>
          : <GpuPlanEditor devices={gpuInfo?.devices ?? []} value={settings.gpuPlan} onChange={(v) => patch("gpuPlan", v)}
              inheritLabel="Use rig default" inheritHint="Uncheck to choose GPUs just for this model." effectiveWhenInherited={gpuInfo?.defaultPlan ?? null} />}
      </div></>}
      {error && <p className="hub-settings-error">{error}</p>}{notice && <p className="hub-settings-notice">{notice}</p>}
      <footer><button className="hub-settings-reset" disabled={loading || saving || isRecommendedModelLoadSettings(settings)} onClick={() => { setSettings(recommendedModelLoadSettings()); setNotice(null); setError(null); }}>Reset to recommended</button><span /><button disabled={saving} onClick={onClose}>Cancel</button><button disabled={loading || saving} onClick={() => void save(false)}>{saving ? "Saving…" : "Save"}</button><button className="primary" disabled={loading || saving} onClick={() => void save(true)}>{saving ? "Applying…" : "Save & Load"}</button></footer>
    </section>
  </div>;
}

function Empty({ title, body }: { title: string; body: string }) { return <div className="hub-empty"><span>◌</span><strong>{title}</strong><p>{body}</p></div>; }
function ListSkeleton() { return <div className="hub-skeleton-list">{Array.from({ length: 7 }, (_, i) => <i key={i} />)}</div>; }
function DetailSkeleton() { return <div className="hub-skeleton-detail"><i /><i /><i /><i /></div>; }

function resolveCardUrl(value: string | undefined, detail: HubModelDetail, mode: "blob" | "resolve") {
  if (!value || /^(https?:|data:|#)/i.test(value)) return value;
  if (value.startsWith("/")) return `https://huggingface.co${value}`;
  return `https://huggingface.co/${detail.id}/${mode}/${detail.revision}/${value.replace(/^\.\//, "")}`;
}
export function modelLogo(detail: HubModelDetail): string | null {
  const markdown = stripFrontMatter(detail.readmeMarkdown);
  const firstHeading = markdown.search(/^#\s+/m);
  const htmlImages = [...markdown.matchAll(/<img\b[^>]*\bsrc=["']([^"']+)["'][^>]*>/gi)];
  const markdownImages = [...markdown.matchAll(/!\[[^\]]*]\(([^)\s]+)(?:\s+["'][^"']*["'])?\)/g)];
  const candidates = [...htmlImages, ...markdownImages]
    .map((match) => ({ value: match[1], afterHeading: firstHeading >= 0 && (match.index ?? 0) > firstHeading }))
    .filter((candidate): candidate is { value: string; afterHeading: boolean } => !!candidate.value);
  if (!candidates.length) return null;
  candidates.sort((a, b) => logoScore(b.value, b.afterHeading) - logoScore(a.value, a.afterHeading));
  return resolveCardUrl(candidates[0]!.value, detail, "resolve") ?? null;
}
export function isVariantOnDevice(model: LocalModel, repoId: string, variantId: string) {
  return model.sourceRepo === repoId && model.variantId === variantId;
}
export function variantsBySizeAscending(variants: GgufVariant[]) {
  return [...variants].sort((a, b) => a.sizeBytes - b.sizeBytes || a.quantization.localeCompare(b.quantization));
}
function logoScore(value: string, afterHeading: boolean) {
  const lower = value.toLowerCase();
  return (afterHeading ? 8 : 0) + (lower.includes("logo") ? 4 : 0) + (lower.includes("icon") ? 2 : 0) + (lower.endsWith(".svg") ? 1 : 0) - (/button|badge|discord|documentation/.test(lower) ? 6 : 0);
}
function stripFrontMatter(markdown: string) { return markdown.replace(/^---\s*[\s\S]*?\s*---\s*/, ""); }
function prettyTag(tag: string) { return tag.split(/[-_]/).map((x) => x[0]?.toUpperCase() + x.slice(1)).join(" "); }
function compact(n: number) { return Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 }).format(n); }
function relativeDate(value: string | null) {
  if (!value) return "Unknown";
  const days = Math.max(0, Math.floor((Date.now() - new Date(value).getTime()) / 86_400_000));
  if (days < 1) return "Today"; if (days < 30) return `${days}d ago`; if (days < 365) return `${Math.floor(days / 30)}mo ago`; return `${Math.floor(days / 365)}y ago`;
}
function readError(error: unknown) { const text = String(error); return text.replace(/^Error:\s*/, "") || "Something went wrong."; }
function isWebPreview() { return import.meta.env.DEV && !("__TAURI_INTERNALS__" in window); }

function mockPage() {
  const items: HubModelSummary[] = [
    ["unsloth/Qwen3.5-9B-GGUF", "unsloth", "Qwen3.5-9B-GGUF", 94200, 318],
    ["bartowski/DeepSeek-R1-Distill-Qwen-14B-GGUF", "bartowski", "DeepSeek-R1-Distill-Qwen-14B-GGUF", 688000, 1220],
    ["mradermacher/gemma-3-12b-it-GGUF", "mradermacher", "gemma-3-12b-it-GGUF", 232000, 340],
    ["Qwen/Qwen3-8B-GGUF", "Qwen", "Qwen3-8B-GGUF", 493000, 860],
    ["TheBloke/Mistral-7B-Instruct-v0.2-GGUF", "TheBloke", "Mistral-7B-Instruct-v0.2-GGUF", 2100000, 3200],
  ].map(([id, author, name, downloads, likes]) => ({ id: String(id), author: String(author), name: String(name), downloads: Number(downloads), likes: Number(likes), revision: "d34db33f", lastModified: "2026-07-12T00:00:00Z", pipelineTag: "text-generation", tags: ["gguf", "conversational", "text-generation"], avatarUrl: "" }));
  return { items, nextCursor: null };
}
function mockDetail(id: string): HubModelDetail {
  const model = mockPage().items.find((m) => m.id === id) ?? mockPage().items[0]!;
  const make = (q: string, gb: number): GgufVariant => ({ id: `${model.name}-${q}`, quantization: q, sizeBytes: gb * 1024 ** 3, files: [{ path: `${model.name}-${q}.gguf`, sizeBytes: gb * 1024 ** 3 }], companions: [], memory: { weightsBytes: gb * 1024 ** 3, kvCacheBytes: 1024 ** 3, overheadBytes: .5 * 1024 ** 3, totalBytes: (gb + 1.5) * 1024 ** 3, confidence: "low" } });
  return { ...model, license: "apache-2.0", baseModels: ["Qwen/Qwen3.5-9B"], variants: [make("Q8_0", 9.7), make("Q6_K", 7.4), make("Q5_K_M", 6.2), make("Q4_K_M", 5.3), make("Q3_K_M", 4.2)], readmeMarkdown: `<p align="center"><img src="https://huggingface.co/front/assets/huggingface_logo-noborder.svg" width="120" alt="Model logo"></p>\n\n# ${model.name}\n\nA capable open model optimized for local inference with **llama.cpp**. Choose a quantization above based on your available memory.\n\n## Highlights\n\n- Strong instruction following and conversational performance\n- Efficient GGUF quantizations for GPU and CPU inference\n- Long-context support and tool-use capabilities\n\n## Usage\n\nDownload a variant, then load it from the LocalLMOS Dashboard.` };
}
