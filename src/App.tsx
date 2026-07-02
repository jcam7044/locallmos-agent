import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type AgentStatus = {
  enrolled: boolean;
  rigId: string | null;
  rigName: string | null;
  connected: boolean;
  runtimeKind: string | null;
  runtimeState: string | null;
  loadedModel: string | null;
  cpuPct: number | null;
  gpuName: string | null;
  gpuUtilPct: number | null;
  lastError: string | null;
};

const label: React.CSSProperties = { color: "#64748b", fontSize: 12 };
const card: React.CSSProperties = {
  border: "1px solid #1f2937",
  background: "#131926",
  borderRadius: 12,
  padding: 16,
};

export function App() {
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [code, setCode] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    try {
      setStatus(await invoke<AgentStatus>("get_status"));
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void refresh();
    const t = setInterval(refresh, 2000);
    return () => clearInterval(t);
  }, []);

  const enroll = async () => {
    setBusy(true);
    setError(null);
    try {
      await invoke("enroll", { code: code.trim(), name: name.trim() });
      setCode("");
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div style={{ padding: 20, maxWidth: 460, margin: "0 auto" }}>
      <h1 style={{ fontSize: 18, fontWeight: 700 }}>
        Local<span style={{ color: "#38bdf8" }}>LMOS</span> Agent
      </h1>

      {!status?.enrolled ? (
        <div style={{ ...card, marginTop: 16 }}>
          <p style={{ ...label, marginTop: 0 }}>
            Enter the pairing code from your LocalLMOS dashboard.
          </p>
          <input
            placeholder="Rig name (e.g. Basement 3090)"
            value={name}
            onChange={(e) => setName(e.target.value)}
            style={inputStyle}
          />
          <input
            placeholder="Pairing code"
            value={code}
            onChange={(e) => setCode(e.target.value.toUpperCase())}
            style={{ ...inputStyle, fontFamily: "monospace", letterSpacing: 3 }}
          />
          <button
            onClick={enroll}
            disabled={busy || !code || !name}
            style={buttonStyle}
          >
            {busy ? "Enrolling…" : "Enroll this rig"}
          </button>
        </div>
      ) : (
        <div style={{ ...card, marginTop: 16 }}>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <strong>{status.rigName ?? "This rig"}</strong>
            <span style={{ color: status.connected ? "#34d399" : "#94a3b8" }}>
              {status.connected ? "● connected" : "○ offline"}
            </span>
          </div>
          <Row k="Runtime" v={`${status.runtimeKind ?? "—"} (${status.runtimeState ?? "?"})`} />
          <Row k="Loaded model" v={status.loadedModel ?? "—"} />
          <Row k="CPU" v={status.cpuPct != null ? `${status.cpuPct.toFixed(0)}%` : "—"} />
          <Row
            k="GPU"
            v={
              status.gpuName
                ? `${status.gpuName} · ${status.gpuUtilPct?.toFixed(0) ?? "?"}%`
                : "—"
            }
          />
        </div>
      )}

      {(error || status?.lastError) && (
        <p style={{ color: "#f87171", fontSize: 13, marginTop: 12 }}>
          {error ?? status?.lastError}
        </p>
      )}
    </div>
  );
}

function Row({ k, v }: { k: string; v: string }) {
  return (
    <div style={{ display: "flex", justifyContent: "space-between", marginTop: 8 }}>
      <span style={label}>{k}</span>
      <span style={{ fontSize: 14 }}>{v}</span>
    </div>
  );
}

const inputStyle: React.CSSProperties = {
  width: "100%",
  boxSizing: "border-box",
  marginTop: 8,
  padding: "8px 12px",
  borderRadius: 8,
  border: "1px solid #1f2937",
  background: "#0b0f17",
  color: "#e2e8f0",
  outline: "none",
};

const buttonStyle: React.CSSProperties = {
  width: "100%",
  marginTop: 12,
  padding: "8px 12px",
  borderRadius: 8,
  border: "none",
  background: "#38bdf8",
  color: "#0f172a",
  fontWeight: 600,
  cursor: "pointer",
};
