import { useState } from "react";
import { enroll as enrollRig, loadModel, localUpdate, restartRuntime } from "../api";
import { buttonStyle, card, inputStyle, label, secondaryButton } from "../styles";
import { formatGB, type AgentStatus, type LocalStatus } from "../types";

export function Dashboard({
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

  const load = async (model: string) => {
    setBusy(model);
    try {
      await loadModel(model);
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
      await restartRuntime();
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
      const v = await localUpdate();
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
                    onClick={() => load(m.name)}
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

export function ConnectCloud({
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

  const doEnroll = async () => {
    setBusy(true);
    setErr(null);
    try {
      await enrollRig(code.trim(), name.trim());
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
          <button onClick={doEnroll} disabled={busy || !code || !name} style={buttonStyle}>
            {busy ? "Enrolling…" : "Enroll this rig"}
          </button>
          {err && <p style={{ color: "#f87171", fontSize: 12, marginTop: 8 }}>{err}</p>}
        </div>
      )}
    </div>
  );
}

function Row({ k, v }: { k: string; v: string }) {
  return (
    <div style={{ display: "flex", justifyContent: "space-between", marginTop: 8 }}>
      <span style={label}>{k}</span>
      <span style={{ fontSize: 13 }}>{v}</span>
    </div>
  );
}
