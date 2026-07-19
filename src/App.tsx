import { useCallback, useEffect, useState } from "react";
import { getAgentStatus, getLocalStatus } from "./api";
import { ChatView } from "./chat/ChatView";
import { ConnectCloud, Dashboard } from "./dashboard/Dashboard";
import type { AgentStatus, LocalStatus } from "./types";
import { useTabWindowSize, type Tab } from "./useTabWindowSize";
import { ModelsView } from "./models/ModelsView";

export function App() {
  const [tab, setTab] = useState<Tab>("dashboard");
  useTabWindowSize(tab);
  const [local, setLocal] = useState<LocalStatus | null>(null);
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

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
        />
      ) : (
        <ModelsView local={local} onChanged={refresh} />
      )}

      {error && <p style={{ color: "#f87171", fontSize: 12, marginTop: 12 }}>{error}</p>}
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
