import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getAgentStatus, getLocalStatus, hubCancelDownload, hubListDownloads } from "./api";
import { ChatView } from "./chat/ChatView";
import { ConnectCloud, Dashboard } from "./dashboard/Dashboard";
import { DownloadBanner } from "./downloads/DownloadBanner";
import type { AgentStatus, DownloadState, LocalStatus } from "./types";
import { useTabWindowSize, type Tab } from "./useTabWindowSize";
import { ModelsView } from "./models/ModelsView";

export function App() {
  const [tab, setTab] = useState<Tab>("dashboard");
  useTabWindowSize(tab);
  const [local, setLocal] = useState<LocalStatus | null>(null);
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [downloads, setDownloads] = useState<Record<string, DownloadState>>({});
  const [dismissedDownloads, setDismissedDownloads] = useState<Set<string>>(() => new Set());

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
        ...(tab === "chat" || tab === "models"
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
      </nav>

      {tab === "dashboard" ? (
        <>
          <Dashboard local={local} running={running} onChanged={refresh} />
          <ConnectCloud status={status} onEnrolled={refresh} />
        </>
      ) : tab === "chat" ? (
        <ChatView
          models={local?.models ?? []}
          running={running}
          enrolled={status?.enrolled ?? false}
          runtimeKind={local?.runtime.kind ?? "ollama"}
        />
      ) : (
        <ModelsView local={local} onChanged={refresh} />
      )}

      {error && <p style={{ color: "#f87171", fontSize: 12, marginTop: 12 }}>{error}</p>}
      <DownloadBanner
        downloads={Object.values(downloads).filter((download) => !dismissedDownloads.has(download.id))}
        onDismiss={(id) => setDismissedDownloads((dismissed) => new Set(dismissed).add(id))}
        onCancel={(id) => { void hubCancelDownload(id).then((download) => setDownloads((current) => ({ ...current, [download.id]: download }))).catch((reason) => setError(String(reason))); }}
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
