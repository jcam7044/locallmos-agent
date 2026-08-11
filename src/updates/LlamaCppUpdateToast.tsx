import type { LlamaCppUpdateInfo, LlamaCppUpdateProgress } from "../types";
import "./llamacpp-update.css";

export function LlamaCppUpdateToast({
  info,
  progress,
  onInstall,
  onDismiss,
}: {
  info: LlamaCppUpdateInfo | null;
  progress: LlamaCppUpdateProgress | null;
  onInstall: () => void;
  onDismiss: () => void;
}) {
  if (!info && !progress) return null;
  const active = progress && !["complete", "error"].includes(progress.phase);
  const percent = progress?.totalBytes
    ? Math.min(100, Math.round(progress.downloadedBytes / progress.totalBytes * 100))
    : 0;
  const tag = progress?.tag ?? info?.latestTag ?? "";
  const title = progress ? phaseTitle(progress.phase) : "llama.cpp update available";
  const detail = progress?.message
    ?? (info
      ? `${info.currentTag ?? "unknown"} → ${info.latestTag} · ${info.backend}${info.variant ? ` ${info.variant}` : ""} · ${formatSize(info.sizeBytes)}`
      : tag);

  return <aside className="llamacpp-update-toast" aria-live="polite" aria-label="llama.cpp update">
    <div className="llamacpp-update-title"><span>⇧</span><div><strong>{title}</strong><small>{detail}</small></div>
      {!active && <button type="button" className="llamacpp-update-close" aria-label="Dismiss llama.cpp update" onClick={onDismiss}>×</button>}
    </div>
    {!progress && info && <p>The local runtime will pause briefly while the signed build is verified and installed.</p>}
    {active && <div className="llamacpp-update-track" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent}><i style={{ width: `${percent}%` }} /></div>}
    {!progress && info && <div className="llamacpp-update-actions">
      <button type="button" onClick={onDismiss}>Dismiss</button>
      <button type="button" className="primary" disabled={!info.installable} onClick={onInstall}>Download and install</button>
    </div>}
    {!progress && info?.reason && <p className="llamacpp-update-warning">{info.reason}</p>}
  </aside>;
}

function phaseTitle(phase: LlamaCppUpdateProgress["phase"]) {
  switch (phase) {
    case "checking": return "Checking signed release";
    case "downloading": return "Downloading llama.cpp";
    case "verifying": return "Verifying download";
    case "installing": return "Installing llama.cpp";
    case "reloading": return "Reloading model";
    case "complete": return "llama.cpp updated";
    case "error": return "llama.cpp update failed";
  }
}

function formatSize(bytes: number) {
  return `${(bytes / 1024 / 1024).toFixed(bytes >= 100 * 1024 * 1024 ? 0 : 1)} MB`;
}
