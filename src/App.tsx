import { useEffect, useState } from "react";
import { getAgentStatus, getLocalStatus } from "./api";
import { ChatView } from "./chat/ChatView";
import { ConnectCloud, Dashboard } from "./dashboard/Dashboard";
import type { AgentStatus, LocalStatus } from "./types";

export function App() {
  const [tab, setTab] = useState<"dashboard" | "chat">("dashboard");
  const [local, setLocal] = useState<LocalStatus | null>(null);
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const [l, s] = await Promise.all([getLocalStatus(), getAgentStatus()]);
      setLocal(l);
      setStatus(s);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void refresh();
    const t = setInterval(refresh, 3000);
    return () => clearInterval(t);
  }, []);

  const running = local?.runtime.state === "running";

  return (
    <div style={{ padding: 16, maxWidth: 480, margin: "0 auto" }}>
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
        <TabButton active={tab === "chat"} onClick={() => setTab("chat")}>
          Chat
        </TabButton>
      </nav>

      {tab === "dashboard" ? (
        <Dashboard local={local} running={running} onChanged={refresh} />
      ) : (
        <ChatView models={local?.models ?? []} running={running} />
      )}

      <ConnectCloud status={status} onEnrolled={refresh} />

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
