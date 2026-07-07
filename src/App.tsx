import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type GpuStat = {
  index: number;
  name: string | null;
  vendor: string;
  utilizationPct: number | null;
  memoryUsedBytes: number | null;
  memoryTotalBytes: number | null;
  temperatureC: number | null;
  powerWatts: number | null;
};

type AgentStatus = {
  enrolled: boolean;
  rigName: string | null;
  connected: boolean;
};

type LocalModel = {
  name: string;
  sizeBytes: number | null;
  quantization: string | null;
  loaded: boolean;
  capabilities: string[];
};

type LocalStatus = {
  runtime: { kind: string; version: string | null; state: string; endpoint: string | null };
  models: LocalModel[];
  telemetry: {
    cpuPct: number | null;
    memoryUsedBytes: number | null;
    memoryTotalBytes: number | null;
    gpus: GpuStat[];
  };
};

type ChatMsg = { role: "user" | "assistant"; content: string };

function formatGB(bytes: number | null | undefined): string {
  if (bytes == null) return "—";
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

export function App() {
  const [tab, setTab] = useState<"dashboard" | "chat">("dashboard");
  const [local, setLocal] = useState<LocalStatus | null>(null);
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const [l, s] = await Promise.all([
        invoke<LocalStatus>("local_status"),
        invoke<AgentStatus>("get_status"),
      ]);
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
        <Chat models={local?.models ?? []} running={running} />
      )}

      <ConnectCloud status={status} onEnrolled={refresh} />

      {error && <p style={{ color: "#f87171", fontSize: 12, marginTop: 12 }}>{error}</p>}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------
function Dashboard({
  local,
  running,
  onChanged,
}: {
  local: LocalStatus | null;
  running: boolean;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [updateMsg, setUpdateMsg] = useState<string | null>(null);

  const loadModel = async (model: string) => {
    setBusy(model);
    try {
      await invoke("load_model", { model });
      await onChanged();
    } catch (e) {
      setUpdateMsg(String(e));
    } finally {
      setBusy(null);
    }
  };

  const restart = async () => {
    setBusy("__restart");
    try {
      await invoke("restart_runtime");
    } catch (e) {
      setUpdateMsg(String(e));
    } finally {
      setBusy(null);
    }
  };

  const checkUpdate = async () => {
    setBusy("__update");
    setUpdateMsg(null);
    try {
      const v = await invoke<string | null>("local_update");
      setUpdateMsg(v ? `Updating to ${v}… the agent will restart.` : "You're on the latest version.");
    } catch (e) {
      setUpdateMsg(`Update check failed: ${String(e)}`);
    } finally {
      setBusy(null);
    }
  };

  if (!running) {
    return (
      <div style={{ ...card, marginTop: 12, borderColor: "#78350f", background: "#1c1408" }}>
        <strong style={{ color: "#fbbf24" }}>⚠ Ollama not detected</strong>
        <p style={{ ...label, marginTop: 6 }}>
          LocalLMOS uses Ollama to run models. Install it from{" "}
          <span style={{ fontFamily: "monospace", color: "#e2e8f0" }}>ollama.com/download</span>, then
          pull a model (e.g. <span style={{ fontFamily: "monospace" }}>ollama pull llama3.2</span>).
        </p>
        <button onClick={restart} disabled={busy === "__restart"} style={secondaryButton}>
          {busy === "__restart" ? "Restarting…" : "Retry / restart Ollama"}
        </button>
      </div>
    );
  }

  const t = local?.telemetry;
  return (
    <div style={{ marginTop: 12, display: "flex", flexDirection: "column", gap: 12 }}>
      {/* System */}
      <div style={card}>
        <strong style={{ fontSize: 13 }}>System</strong>
        <Row k="CPU" v={t?.cpuPct != null ? `${t.cpuPct.toFixed(0)}%` : "—"} />
        <Row k="Memory" v={`${formatGB(t?.memoryUsedBytes)} / ${formatGB(t?.memoryTotalBytes)}`} />
        {(t?.gpus ?? []).length === 0 ? (
          <Row k="GPU" v="—" />
        ) : (
          (t?.gpus ?? []).map((g) => (
            <Row
              key={g.index}
              k={`GPU ${g.index}`}
              v={`${g.name ?? g.vendor} · ${g.utilizationPct?.toFixed(0) ?? "?"}% · ${formatGB(
                g.memoryUsedBytes,
              )}/${formatGB(g.memoryTotalBytes)}`}
            />
          ))
        )}
      </div>

      {/* Runtime */}
      <div style={card}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <strong style={{ fontSize: 13 }}>
            {local?.runtime.kind ?? "runtime"}{" "}
            <span style={{ color: "#64748b", fontWeight: 400 }}>{local?.runtime.version ?? ""}</span>
          </strong>
          <span style={{ color: "#34d399", fontSize: 12 }}>● running</span>
        </div>
        <div style={{ display: "flex", gap: 8, marginTop: 10 }}>
          <button onClick={restart} disabled={busy === "__restart"} style={secondaryButton}>
            {busy === "__restart" ? "Restarting…" : "Restart"}
          </button>
          <button onClick={checkUpdate} disabled={busy === "__update"} style={secondaryButton}>
            {busy === "__update" ? "Checking…" : "Check for updates"}
          </button>
        </div>
        {updateMsg && <p style={{ ...label, marginTop: 8 }}>{updateMsg}</p>}
      </div>

      {/* Models */}
      <div style={card}>
        <strong style={{ fontSize: 13 }}>Models</strong>
        {(local?.models ?? []).length === 0 ? (
          <p style={{ ...label, marginTop: 8 }}>
            No models installed. Pull one, e.g.{" "}
            <span style={{ fontFamily: "monospace" }}>ollama pull llama3.2</span>.
          </p>
        ) : (
          <div style={{ marginTop: 6 }}>
            {(local?.models ?? []).map((m) => (
              <div
                key={m.name}
                style={{
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  padding: "6px 0",
                  borderTop: "1px solid #1f2937",
                }}
              >
                <div style={{ minWidth: 0 }}>
                  <div style={{ fontSize: 13, overflow: "hidden", textOverflow: "ellipsis" }}>
                    {m.name}
                  </div>
                  <div style={label}>{formatGB(m.sizeBytes)}</div>
                </div>
                {m.loaded ? (
                  <span style={{ color: "#34d399", fontSize: 12 }}>loaded</span>
                ) : (
                  <button
                    onClick={() => loadModel(m.name)}
                    disabled={busy === m.name}
                    style={secondaryButton}
                  >
                    {busy === m.name ? "Loading…" : "Load"}
                  </button>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------
function Chat({ models, running }: { models: LocalModel[]; running: boolean }) {
  const [model, setModel] = useState("");
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  // Default to a loaded model, else the first available.
  useEffect(() => {
    if (!model && models.length > 0) {
      const pick = models.find((m) => m.loaded) ?? models[0];
      if (pick) setModel(pick.name);
    }
  }, [models, model]);

  useEffect(() => {
    scrollRef.current?.scrollTo(0, scrollRef.current.scrollHeight);
  }, [messages]);

  const send = async () => {
    const text = input.trim();
    if (!text || !model || streaming) return;
    const next = [...messages, { role: "user" as const, content: text }];
    setMessages([...next, { role: "assistant", content: "" }]);
    setInput("");
    setStreaming(true);

    const unlisten = await listen<string>("local-chat-delta", (e) => {
      setMessages((m) => {
        const c = [...m];
        const last = c[c.length - 1];
        if (!last) return m;
        c[c.length - 1] = { role: last.role, content: last.content + e.payload };
        return c;
      });
    });
    try {
      const full = await invoke<string>("local_chat", { model, messages: next });
      setMessages((m) => {
        const c = [...m];
        c[c.length - 1] = { role: "assistant", content: full };
        return c;
      });
    } catch (e) {
      setMessages((m) => {
        const c = [...m];
        c[c.length - 1] = { role: "assistant", content: `⚠ ${String(e)}` };
        return c;
      });
    } finally {
      unlisten();
      setStreaming(false);
    }
  };

  if (!running) {
    return (
      <div style={{ ...card, marginTop: 12 }}>
        <p style={{ ...label, margin: 0 }}>Start Ollama to chat with a local model.</p>
      </div>
    );
  }

  return (
    <div style={{ ...card, marginTop: 12, display: "flex", flexDirection: "column", height: 380 }}>
      <select
        value={model}
        onChange={(e) => setModel(e.target.value)}
        style={{ ...inputStyle, marginTop: 0 }}
      >
        {models.length === 0 && <option value="">No models installed</option>}
        {models.map((m) => (
          <option key={m.name} value={m.name}>
            {m.name}
            {m.loaded ? " (loaded)" : ""}
          </option>
        ))}
      </select>

      <div ref={scrollRef} style={{ flex: 1, overflowY: "auto", margin: "10px 0", paddingRight: 4 }}>
        {messages.length === 0 ? (
          <p style={{ ...label, textAlign: "center", marginTop: 24 }}>
            Ask your local model anything — nothing leaves this machine.
          </p>
        ) : (
          messages.map((m, i) => (
            <div
              key={i}
              style={{
                marginBottom: 8,
                textAlign: m.role === "user" ? "right" : "left",
              }}
            >
              <span
                style={{
                  display: "inline-block",
                  maxWidth: "85%",
                  padding: "6px 10px",
                  borderRadius: 10,
                  fontSize: 13,
                  whiteSpace: "pre-wrap",
                  textAlign: "left",
                  background: m.role === "user" ? "#0ea5e9" : "#0b0f17",
                  color: m.role === "user" ? "#04121c" : "#e2e8f0",
                  border: m.role === "user" ? "none" : "1px solid #1f2937",
                }}
              >
                {m.content || (streaming && i === messages.length - 1 ? "…" : "")}
              </span>
            </div>
          ))
        )}
      </div>

      <div style={{ display: "flex", gap: 8 }}>
        <input
          placeholder="Message"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void send();
          }}
          style={{ ...inputStyle, marginTop: 0, flex: 1 }}
        />
        <button onClick={send} disabled={streaming || !input.trim() || !model} style={buttonStyle}>
          {streaming ? "…" : "Send"}
        </button>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Connect to cloud (optional enrollment)
// ---------------------------------------------------------------------------
function ConnectCloud({
  status,
  onEnrolled,
}: {
  status: AgentStatus | null;
  onEnrolled: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [code, setCode] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  if (status?.enrolled) {
    return (
      <p style={{ ...label, marginTop: 16, textAlign: "center" }}>
        Connected to cloud as <strong style={{ color: "#e2e8f0" }}>{status.rigName}</strong>.
      </p>
    );
  }

  const enroll = async () => {
    setBusy(true);
    setErr(null);
    try {
      await invoke("enroll", { code: code.trim(), name: name.trim() });
      setCode("");
      onEnrolled();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div style={{ marginTop: 16 }}>
      {!open ? (
        <button onClick={() => setOpen(true)} style={{ ...secondaryButton, width: "100%" }}>
          ☁ Connect to cloud
        </button>
      ) : (
        <div style={card}>
          <strong style={{ fontSize: 13 }}>Connect to cloud</strong>
          <p style={{ ...label, marginTop: 6 }}>
            Enroll this machine to manage it from the LocaLLMOS dashboard — remote access,
            sharing, teams, orchestration, and API endpoints.
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
          <button onClick={enroll} disabled={busy || !code || !name} style={buttonStyle}>
            {busy ? "Enrolling…" : "Enroll this rig"}
          </button>
          {err && <p style={{ color: "#f87171", fontSize: 12, marginTop: 8 }}>{err}</p>}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Shared bits
// ---------------------------------------------------------------------------
function Row({ k, v }: { k: string; v: string }) {
  return (
    <div style={{ display: "flex", justifyContent: "space-between", marginTop: 8 }}>
      <span style={label}>{k}</span>
      <span style={{ fontSize: 13 }}>{v}</span>
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

const label: React.CSSProperties = { color: "#64748b", fontSize: 12 };
const card: React.CSSProperties = {
  border: "1px solid #1f2937",
  background: "#131926",
  borderRadius: 12,
  padding: 14,
};
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
  padding: "8px 14px",
  borderRadius: 8,
  border: "none",
  background: "#38bdf8",
  color: "#0f172a",
  fontWeight: 600,
  cursor: "pointer",
};
const secondaryButton: React.CSSProperties = {
  padding: "6px 12px",
  borderRadius: 8,
  border: "1px solid #1f2937",
  background: "transparent",
  color: "#e2e8f0",
  fontSize: 12,
  cursor: "pointer",
};
