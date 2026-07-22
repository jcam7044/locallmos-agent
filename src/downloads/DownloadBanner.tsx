import type { DownloadState } from "../types";
import "./download-banner.css";

export function DownloadBanner({ downloads, onDismiss, onCancel }: { downloads: DownloadState[]; onDismiss: (id: string) => void; onCancel: (id: string) => void }) {
  const active = downloads.filter((download) => ["queued", "downloading", "canceling"].includes(download.status));
  if (!active.length) return null;
  return <aside className="download-banner-stack" aria-live="polite" aria-label="Model downloads in progress">
    {active.map((download) => <DownloadProgress key={download.id} download={download} onDismiss={onDismiss} onCancel={onCancel} />)}
  </aside>;
}

export function downloadPercent(download: DownloadState) {
  if (!download.totalBytes) return 0;
  return Math.min(100, Math.round(download.downloadedBytes / download.totalBytes * 100));
}

function DownloadProgress({ download, onDismiss, onCancel }: { download: DownloadState; onDismiss: (id: string) => void; onCancel: (id: string) => void }) {
  const percent = downloadPercent(download);
  const title = download.repoId.split("/").pop() ?? download.repoId;
  const preparing = download.status === "queued";
  const canceling = download.status === "canceling";
  return <section className="download-banner">
    <div className="download-banner-heading"><span className="download-banner-icon">⇩</span><div><strong>{canceling ? "Cancelling download" : preparing ? "Preparing model download" : "Downloading model"}</strong><span>{title} · {download.variantId}</span></div><b>{percent}%</b><button type="button" className="download-banner-cancel" disabled={canceling} onClick={() => onCancel(download.id)}>{canceling ? "Cancelling…" : "Cancel"}</button><button type="button" className="download-banner-dismiss" aria-label={`Dismiss download for ${title}`} onClick={() => onDismiss(download.id)}>×</button></div>
    <div className="download-banner-track" role="progressbar" aria-label={`Downloading ${title}`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent}>
      <i style={{ width: `${percent}%` }} />
    </div>
  </section>;
}
