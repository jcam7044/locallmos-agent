import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import rehypeRaw from "rehype-raw";
import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import remarkGfm from "remark-gfm";
import { hubGetAuthorAvatars, hubGetModel, hubListDownloads, hubSearchModels, hubStartDownload, loadModel, unloadModel } from "../api";
import type {
  DownloadState,
  GgufVariant,
  HubModelDetail,
  HubModelSummary,
  LocalModel,
  LocalStatus,
} from "../types";
import { assessFit, chooseVariant, formatBytes } from "./fit";
import "./models.css";

type Mode = "discover" | "device";
type Sort = "trending" | "downloads" | "likes" | "newest";
type Capability = "all" | "text" | "vision";

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
  const [modelAction, setModelAction] = useState<string | null>(null);
  const request = useRef(0);

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

  useEffect(() => { if (mode === "discover") void loadPage(); }, [debounced, capability, sort, mode]);

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

  const load = async (model: LocalModel) => {
    setError(null);
    setModelAction(model.id);
    try {
      await loadModel(model.id);
      await onChanged();
    } catch (e) { setError(readError(e));
    } finally { setModelAction(null); }
  };

  const eject = async (model: LocalModel) => {
    setError(null);
    setModelAction(model.id);
    try {
      await unloadModel(model.id);
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
          <p>Discover GGUF models, compare quantizations, and download them for llama.cpp.</p>
        </div>
        <HardwareChips local={local} />
      </header>

      <div className="hub-toolbar">
        <div className="hub-segmented" aria-label="Model source">
          <button className={mode === "discover" ? "active" : ""} onClick={() => setMode("discover")}>Discover</button>
          <button className={mode === "device" ? "active" : ""} onClick={() => setMode("device")}>On Device <span>{local?.models.length ?? 0}</span></button>
        </div>
        {mode === "discover" && <>
          <label className="hub-search"><span>⌕</span><input aria-label="Search all models" placeholder="Search all models" value={query} onChange={(e) => setQuery(e.target.value)} /></label>
          <select aria-label="Capability" value={capability} onChange={(e) => setCapability(e.target.value as Capability)}>
            <option value="all">All capabilities</option><option value="text">Text</option><option value="vision">Vision</option>
          </select>
          <select aria-label="Sort models" value={sort} onChange={(e) => setSort(e.target.value as Sort)}>
            <option value="trending">Trending</option><option value="downloads">Most downloaded</option><option value="likes">Most liked</option><option value="newest">Newest</option>
          </select>
        </>}
      </div>

      {error && <div className="hub-error"><span>{error}</span><button onClick={() => void loadPage()}>Retry</button></div>}

      {mode === "discover" ? (
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
                localModel={selectedVariant ? local?.models.find((model) => isVariantOnDevice(model, detail.id, selectedVariant.id)) : undefined}
                actionBusy={modelAction}
                onLoad={load}
                onEject={eject}
              />
            ) : <Empty title="Select a model" body="Choose a repository to compare its available GGUF downloads." />}
          </section>
        </div>
      ) : <OnDevice models={local?.models ?? []} onSelect={selectDevice} busy={modelAction} onLoad={load} onEject={eject} />}
    </main>
  );
}

function HardwareChips({ local }: { local: LocalStatus | null }) {
  const t = local?.telemetry;
  const vram = (t?.gpus ?? []).filter((g) => g.vendor !== "apple").reduce((n, g) => n + (g.memoryTotalBytes ?? 0), 0);
  return <div className="hub-hardware">
    <span><i className="green" />{vram ? `${formatBytes(vram)} VRAM` : "CPU only"}</span>
    <span><i className="blue" />{formatBytes(t?.memoryTotalBytes)} RAM</span>
    <span><i />{local?.runtime.contextSize?.toLocaleString() ?? "8,192"} ctx</span>
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

function ModelDetail({ detail, variant, variantId, onVariant, local, download, installed, diskEnough, avatarUrl, onDownload, localModel, actionBusy, onLoad, onEject }: {
  detail: HubModelDetail; variant: GgufVariant | null; variantId: string | null; onVariant: (id: string) => void;
  local: LocalStatus | null; download?: DownloadState; installed: boolean; diskEnough: boolean; avatarUrl?: string | null; onDownload: () => void;
  localModel?: LocalModel; actionBusy: string | null; onLoad: (model: LocalModel) => Promise<void>; onEject: (model: LocalModel) => Promise<void>;
}) {
  const fit = variant ? assessFit(variant, local) : null;
  const downloading = download?.status === "queued" || download?.status === "downloading";
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
        <button className="hub-download-button" disabled={!variant || installed || downloading || !diskEnough} onClick={onDownload}>
          {installed ? "✓ On Device" : downloading ? `${progress}%` : !diskEnough ? "Not enough disk" : "⇩ Download"}
        </button>
      </div>
      {fit && <div className="hub-fit-line"><strong style={{ color: fit.color }}>{fit.label}</strong><span>{fit.detail}</span>{fit.freeMemoryWarning && <em>Free memory before loading.</em>}</div>}
      {downloading && <div className="hub-progress"><i style={{ width: `${progress}%` }} /></div>}
      {download?.status === "error" && <p className="hub-inline-error">{download.error}</p>}
      {variant?.companions.length ? <p className="hub-companion">Vision adapter available ({variant.companions.map((f) => f.path.split("/").pop()).join(", ")}); automatic mmproj loading is not enabled yet.</p> : null}
    </div>
    <div className="hub-meta">
      <span>Updated <b>{relativeDate(detail.lastModified)}</b></span><span>Downloads <b>{compact(detail.downloads)}</b></span><span>Likes <b>{compact(detail.likes)}</b></span><span>License <b>{detail.license ?? "Not specified"}</b></span>
    </div>
    {localModel && <ModelAction model={localModel} busy={actionBusy === localModel.id} onLoad={onLoad} onEject={onEject} />}
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

function OnDevice({ models, onSelect, busy, onLoad, onEject }: { models: LocalModel[]; onSelect: (model: LocalModel) => void; busy: string | null; onLoad: (model: LocalModel) => Promise<void>; onEject: (model: LocalModel) => Promise<void> }) {
  return <section className="hub-device">
    <div className="hub-pane-title"><strong>Models on this device</strong><span>{models.length}</span></div>
    {models.length ? <div className="hub-device-grid">{models.map((model) => <article key={model.id} className="hub-device-card">
      <button className="hub-device-select" onClick={() => onSelect(model)} disabled={!model.sourceRepo}>
        <span className="hub-device-icon">◫</span><span><strong>{model.name}</strong><small>{model.sourceRepo ?? "Local GGUF"}</small><em>{model.quantization ?? "GGUF"} · {formatBytes(model.sizeBytes)}</em></span><b className={model.loaded ? "loaded" : ""}>{model.loaded ? "Loaded" : "On Device"}</b>
      </button>
      <ModelAction model={model} busy={busy === model.id} onLoad={onLoad} onEject={onEject} compact />
    </article>)}</div> : <Empty title="No models on device" body="Download a GGUF model or place one in the llama.cpp models directory." />}
  </section>;
}

function ModelAction({ model, busy, onLoad, onEject, compact = false }: { model: LocalModel; busy: boolean; onLoad: (model: LocalModel) => Promise<void>; onEject: (model: LocalModel) => Promise<void>; compact?: boolean }) {
  return <div className={`hub-model-actions ${compact ? "compact" : ""}`}>
    {model.loaded ? <button className="hub-eject-button" disabled={busy} onClick={() => void onEject(model)}>{busy ? "Ejecting…" : "Eject"}</button> : <button className="hub-load-button" disabled={busy} onClick={() => void onLoad(model)}>{busy ? "Loading…" : "Load"}</button>}
    <small>{model.loaded ? "Releases model memory; files stay on disk." : "Loads this model into memory."}</small>
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
