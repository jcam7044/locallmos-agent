import { useState } from "react";
import type { AgentUpdateInfo } from "../types";
import "./agent-update.css";

/** Update-available toast for the desktop app. The GUI can't overwrite its own
 * (often privileged) binary in place, so instead of self-updating it shows the
 * installer command and a one-click copy. `raised` lifts it above the llama.cpp
 * toast when both are visible so they don't overlap in the bottom-right. */
export function AgentUpdateToast({
  info,
  raised = false,
  onDismiss,
}: {
  info: AgentUpdateInfo | null;
  raised?: boolean;
  onDismiss: () => void;
}) {
  const [copied, setCopied] = useState(false);
  if (!info) return null;

  const copy = () => {
    void navigator.clipboard.writeText(info.installCommand).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }).catch(() => undefined);
  };

  return <aside className={`agent-update-toast${raised ? " raised" : ""}`} aria-live="polite" aria-label="Agent update">
    <div className="agent-update-title"><span>⇧</span><div>
      <strong>Agent update available</strong>
      <small>{info.currentVersion} → {info.latestVersion}</small>
    </div>
      <button type="button" className="agent-update-close" aria-label="Dismiss agent update" onClick={onDismiss}>×</button>
    </div>
    <p>Updating replaces the installed app, which needs terminal privileges. Run this command, then relaunch:</p>
    <code className="agent-update-cmd">{info.installCommand}</code>
    <div className="agent-update-actions">
      <button type="button" onClick={onDismiss}>Dismiss</button>
      <button type="button" className="primary" onClick={copy}>{copied ? "Copied!" : "Copy command"}</button>
    </div>
  </aside>;
}
