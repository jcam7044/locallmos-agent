import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { agentCheckUpdate, getAgentStatus, getLocalStatus, hubCancelDownload, hubListDownloads, llamaCppCheckUpdate, llamaCppInstallUpdate } from "./api";
import { ChatView } from "./chat/ChatView";
import { CodingView } from "./coding/CodingView";
import { ConnectCloud, Dashboard } from "./dashboard/Dashboard";
import { DownloadBanner } from "./downloads/DownloadBanner";
import type { AgentStatus, AgentUpdateInfo, DownloadState, LlamaCppUpdateInfo, LlamaCppUpdateProgress, LocalStatus } from "./types";
import { useTabWindowSize, type Tab } from "./useTabWindowSize";
import { ModelsView } from "./models/ModelsView";
import { ToolsView } from "./tools/ToolsView";
import { AgentUpdateToast } from "./updates/AgentUpdateToast";
import { LlamaCppUpdateToast } from "./updates/LlamaCppUpdateToast";

const DISMISSED_LLAMA_UPDATE = "locallmos.dismissedLlamaCppUpdate";
const DISMISSED_AGENT_UPDATE = "locallmos.dismissedAgentUpdate";

export function App() {
  const [tab, setTab] = useState<Tab>("dashboard");
  useTabWindowSize(tab);
  const [local, setLocal] = useState<LocalStatus | null>(null);
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [downloads, setDownloads] = useState<Record<string, DownloadState>>({});
  const [dismissedDownloads, setDismissedDownloads] = useState<Set<string>>(() => new Set());
  const [llamaUpdate, setLlamaUpdate] = useState<LlamaCppUpdateInfo | null>(null);
  const [llamaProgress, setLlamaProgress] = useState<LlamaCppUpdateProgress | null>(null);
  const [agentUpdate, setAgentUpdate] = useState<AgentUpdateInfo | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [l, s] = await Promise.all([getLocalStatus(), getAgentStatus()]);
      setLocal(l);
      setStatus(s);
      setError(null);
    } catch (e) {
      if ("__TAURI_INTERNALS__" in window || !import.meta.env.DEV) setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const t = setInterval(refresh, 3000);
    return () => clearInterval(t);
  }, [refresh]);

  const checkLlamaUpdate = useCallback(async (manual = false) => {
    if (local?.runtime.kind !== "llamacpp") return "llama.cpp is not the active runtime.";
    const update = await llamaCppCheckUpdate();
    if (!update) {
      if (manual) setLlamaUpdate(null);
      return "llama.cpp is up to date.";
    }
    const dismissed = localStorage.getItem(DISMISSED_LLAMA_UPDATE);
    if (manual || dismissed !== update.latestTag) setLlamaUpdate(update);
    return `llama.cpp ${update.latestTag} is available.`;
  }, [local?.runtime.kind]);

  useEffect(() => {
    if (local?.runtime.kind !== "llamacpp") {
      setLlamaUpdate(null);
      return;
    }
    void checkLlamaUpdate().catch(() => undefined);
    const timer = setInterval(() => { void checkLlamaUpdate().catch(() => undefined); }, 24 * 60 * 60 * 1000);
    return () => clearInterval(timer);
  }, [checkLlamaUpdate, local?.runtime.kind]);

  // The desktop app can't overwrite its own (often privileged) binary in place,
  // so a newer agent release surfaces a copy-command toast instead of a
  // self-update. Manual checks always pop the toast; the periodic check respects
  // a per-version dismissal.
  const checkAgentUpdate = useCallback(async (manual = false) => {
    const update = await agentCheckUpdate();
    if (!update) {
      if (manual) setAgentUpdate(null);
      return "You're on the latest version.";
    }
    const dismissed = localStorage.getItem(DISMISSED_AGENT_UPDATE);
    if (manual || dismissed !== update.latestVersion) setAgentUpdate(update);
    return `Agent ${update.latestVersion} is available.`;
  }, []);

  useEffect(() => {
    void checkAgentUpdate().catch(() => undefined);
    const timer = setInterval(() => { void checkAgentUpdate().catch(() => undefined); }, 24 * 60 * 60 * 1000);
    return () => clearInterval(timer);
  }, [checkAgentUpdate]);

  const dismissAgentUpdate = useCallback(() => {
    if (agentUpdate) localStorage.setItem(DISMISSED_AGENT_UPDATE, agentUpdate.latestVersion);
    setAgentUpdate(null);
  }, [agentUpdate]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<LlamaCppUpdateProgress>("llamacpp-update", ({ payload }) => {
      setLlamaProgress(payload);
      if (payload.phase === "complete") {
        setLlamaUpdate(null);
        localStorage.removeItem(DISMISSED_LLAMA_UPDATE);
        void refresh();
      }
    }).then((stop) => { unlisten = stop; });
    return () => unlisten?.();
  }, [refresh]);

  const installLlamaUpdate = useCallback(() => {
    if (!llamaUpdate) return;
    setLlamaProgress({ phase: "checking", tag: llamaUpdate.latestTag, downloadedBytes: 0, totalBytes: llamaUpdate.sizeBytes, message: null });
    void llamaCppInstallUpdate(llamaUpdate.latestTag).catch((reason) => {
      setLlamaProgress({ phase: "error", tag: llamaUpdate.latestTag, downloadedBytes: 0, totalBytes: llamaUpdate.sizeBytes, message: String(reason) });
    });
  }, [llamaUpdate]);

  const dismissLlamaUpdate = useCallback(() => {
    if (llamaUpdate) localStorage.setItem(DISMISSED_LLAMA_UPDATE, llamaUpdate.latestTag);
    setLlamaUpdate(null);
    setLlamaProgress(null);
  }, [llamaUpdate]);

  useEffect(() => {
    let disposed = false;
    void hubListDownloads().then((items) => {
      if (!disposed) setDownloads(Object.fromEntries(items.map((download) => [download.id, download])));
    }).catch(() => undefined);
    let unlisten: (() => void) | undefined;
    void listen<DownloadState>("model-download", ({ payload }) => {
      setDownloads((current) => ({ ...current, [payload.id]: payload }));
      if (payload.status === "complete") void refresh();
    }).then((stop) => { unlisten = stop; });
    return () => { disposed = true; unlisten?.(); };
  }, [refresh]);

  const running = local?.runtime.state === "running";

  return (
    <div
      style={{
        padding: 16,
        maxWidth: tab === "dashboard" ? 480 : undefined,
        margin: "0 auto",
        boxSizing: "border-box",
        ...(tab === "chat" || tab === "models" || tab === "code"
          ? { height: "100vh", display: "flex", flexDirection: "column", overflow: "hidden" }
          : {}),
      }}
    >
      <header style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
        <h1 style={{ fontSize: 17, fontWeight: 700, margin: 0 }}>
          Loca<span style={{ color: "#38bdf8" }}>LLM</span>OS
        </h1>
        <span style={{ fontSize: 12, color: status?.enrolled ? "#34d399" : "#64748b" }}>
          {status?.enrolled ? `☁ ${status.rigName ?? "cloud"}` : "● local"}
        </span>
      </header>

      <nav style={{ display: "flex", gap: 4, marginTop: 12 }}>
        <TabButton active={tab === "dashboard"} onClick={() => setTab("dashboard")}>
          Dashboard
        </TabButton>
        <TabButton active={tab === "models"} onClick={() => setTab("models")}>
          Models
        </TabButton>
        <TabButton active={tab === "chat"} onClick={() => setTab("chat")}>
          Chat
        </TabButton>
        <TabButton active={tab === "code"} onClick={() => setTab("code")}>
          Code
        </TabButton>
        <TabButton active={tab === "tools"} onClick={() => setTab("tools")}>
          Tools
        </TabButton>
      </nav>

      {tab === "dashboard" ? (
        <>
          <Dashboard local={local} running={running} onChanged={refresh} onCheckAgentUpdate={() => checkAgentUpdate(true)} onCheckLlamaCppUpdate={() => checkLlamaUpdate(true)} />
          <ConnectCloud status={status} onEnrolled={refresh} />
        </>
      ) : tab === "chat" ? (
        <ChatView
          models={local?.models ?? []}
          running={running}
          enrolled={status?.enrolled ?? false}
          runtimeKind={local?.runtime.kind ?? "ollama"}
        />
      ) : tab === "code" ? (
        <CodingView models={local?.models ?? []} />
      ) : tab === "tools" ? (
        <ToolsView />
      ) : (
        <ModelsView local={local} onChanged={refresh} />
      )}

      {error && <p style={{ color: "#f87171", fontSize: 12, marginTop: 12 }}>{error}</p>}
      <DownloadBanner
        downloads={Object.values(downloads).filter((download) => !dismissedDownloads.has(download.id))}
        onDismiss={(id) => setDismissedDownloads((dismissed) => new Set(dismissed).add(id))}
        onCancel={(id) => { void hubCancelDownload(id).then((download) => setDownloads((current) => ({ ...current, [download.id]: download }))).catch((reason) => setError(String(reason))); }}
      />
      <LlamaCppUpdateToast
        info={llamaUpdate}
        progress={llamaProgress}
        onInstall={installLlamaUpdate}
        onDismiss={dismissLlamaUpdate}
      />
      <AgentUpdateToast
        info={agentUpdate}
        raised={Boolean(llamaUpdate || llamaProgress)}
        onDismiss={dismissAgentUpdate}
      />
    </div>
  );
}

function TabButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      style={{
        padding: "6px 14px",
        borderRadius: 8,
        border: "none",
        cursor: "pointer",
        fontSize: 13,
        fontWeight: 600,
        background: active ? "rgba(56,189,248,0.15)" : "transparent",
        color: active ? "#38bdf8" : "#94a3b8",
      }}
    >
      {children}
    </button>
  );
}
