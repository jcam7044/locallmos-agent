import { useCallback, useEffect, useRef, useState } from "react";
import {
  codingApprove,
  codingCancel,
  codingGetSession,
  codingListSessions,
  codingListWorkspaces,
  codingRegisterWorkspace,
  codingSend,
  codingStartSession,
} from "../api";
import type {
  ApprovalPolicy,
  CodingMessage,
  CodingSessionMeta,
  CodingWorkspace,
} from "../types";
import { Markdown } from "../chat/Markdown";
import { useCodingStream, type CodingLive, type CodingTrace } from "./useCodingStream";

type ModelOption = { name: string; loaded: boolean };

const C = {
  border: "1px solid rgba(148,163,184,0.18)",
  panel: "rgba(15,23,42,0.4)",
  accent: "#38bdf8",
  muted: "#94a3b8",
};

export function CodingView({ models, enrolled }: { models: ModelOption[]; enrolled: boolean }) {
  const [sessions, setSessions] = useState<CodingSessionMeta[]>([]);
  const [workspaces, setWorkspaces] = useState<CodingWorkspace[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [messages, setMessages] = useState<CodingMessage[]>([]);
  const [model, setModel] = useState("");
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { live, reset } = useCodingStream(activeId);

  const refreshLists = useCallback(async () => {
    if (!enrolled) return;
    try {
      const [s, w] = await Promise.all([codingListSessions(), codingListWorkspaces()]);
      setSessions(Array.isArray(s) ? s : []);
      setWorkspaces(Array.isArray(w) ? w : []);
    } catch (e) {
      setError(String(e));
    }
  }, [enrolled]);

  useEffect(() => {
    void refreshLists();
  }, [refreshLists]);

  // Default the model picker to a loaded model, else the first available.
  useEffect(() => {
    if (!model && models.length) {
      const def = models.find((m) => m.loaded) ?? models[0];
      if (def) setModel(def.name);
    }
  }, [models, model]);

  const loadMessages = useCallback(async (id: string) => {
    try {
      const msgs = await codingGetSession(id);
      setMessages(Array.isArray(msgs) ? msgs : []);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    if (activeId) void loadMessages(activeId);
    else setMessages([]);
  }, [activeId, loadMessages]);

  // When a turn finishes, fold the live overlay into the persisted transcript.
  const prevStatus = useRef(live.status);
  useEffect(() => {
    if (prevStatus.current !== "done" && live.status === "done" && activeId) {
      void loadMessages(activeId);
      void refreshLists();
    }
    prevStatus.current = live.status;
  }, [live.status, activeId, loadMessages, refreshLists]);

  const streaming = live.status === "loading" || live.status === "streaming" || live.approvals.length > 0;

  async function onStart(workspaceId: string) {
    if (!prompt.trim() || !model) return;
    setBusy(true);
    setError(null);
    try {
      const { conversationId } = await codingStartSession(workspaceId, model, prompt.trim());
      setPrompt("");
      reset();
      setActiveId(conversationId);
      await refreshLists();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function onSend() {
    if (!activeId || !prompt.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await codingSend(activeId, prompt.trim(), model || undefined);
      setPrompt("");
      // Optimistically show the user's message; the stream fills in the reply.
      setMessages((m) => [
        ...m,
        {
          id: `local-${Date.now()}`,
          role: "user",
          content: prompt.trim(),
          status: "done",
          thinking: null,
          tool_activity: null,
          created_at: new Date().toISOString(),
        },
      ]);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function decide(invocationId: string, decision: "approved" | "denied") {
    try {
      await codingApprove(invocationId, decision);
    } catch (e) {
      setError(String(e));
    }
  }

  if (!enrolled) {
    return (
      <div style={{ padding: 24, color: C.muted, maxWidth: 520 }}>
        <h2 style={{ color: "#e2e8f0", fontSize: 16 }}>Coding sessions</h2>
        <p>
          Coding sessions sync through the cloud so you can pick them up from the web. Connect this
          rig from the <strong>Dashboard</strong> tab to get started.
        </p>
      </div>
    );
  }

  return (
    <div style={{ display: "flex", gap: 12, flex: 1, minHeight: 0, marginTop: 12 }}>
      {/* Sidebar */}
      <aside style={{ width: 240, display: "flex", flexDirection: "column", gap: 8, borderRight: C.border, paddingRight: 12 }}>
        <button onClick={() => { setActiveId(null); reset(); }} style={btn(activeId === null)}>
          + New session
        </button>
        <div style={{ overflowY: "auto", display: "flex", flexDirection: "column", gap: 4 }}>
          {sessions.map((s) => (
            <button key={s.id} onClick={() => setActiveId(s.id)} style={sessionBtn(activeId === s.id)}>
              <div style={{ fontSize: 13, color: "#e2e8f0", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                {s.title || "Untitled session"}
              </div>
              <div style={{ fontSize: 11, color: C.muted }}>{s.model ?? ""}</div>
            </button>
          ))}
          {sessions.length === 0 && <p style={{ fontSize: 12, color: C.muted }}>No sessions yet.</p>}
        </div>
      </aside>

      {/* Main */}
      <main style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
        {error && <div style={{ color: "#f87171", fontSize: 12, marginBottom: 8 }}>{error}</div>}

        {activeId === null ? (
          <NewSession
            workspaces={workspaces}
            models={models}
            model={model}
            setModel={setModel}
            onRegister={async (name, path, policy) => {
              setBusy(true);
              setError(null);
              try {
                const { workspaceId } = await codingRegisterWorkspace(name, path, policy);
                await refreshLists();
                return workspaceId;
              } catch (e) {
                setError(String(e));
                return null;
              } finally {
                setBusy(false);
              }
            }}
            onStart={onStart}
            prompt={prompt}
            setPrompt={setPrompt}
            busy={busy}
          />
        ) : (
          <>
            <div style={{ flex: 1, overflowY: "auto", display: "flex", flexDirection: "column", gap: 12, paddingRight: 4 }}>
              {messages
                .filter((m) => m.role !== "system")
                .map((m) => (
                  <MessageBubble key={m.id} role={m.role} content={m.content} />
                ))}
              {(live.status !== "idle" && live.messageId) && <LiveTurn live={live} onDecide={decide} />}
            </div>

            <Composer
              models={models}
              model={model}
              setModel={setModel}
              prompt={prompt}
              setPrompt={setPrompt}
              onSend={onSend}
              busy={busy}
              streaming={streaming}
              onStop={() => live.messageId && codingCancel(live.messageId)}
            />
          </>
        )}
      </main>
    </div>
  );
}

function NewSession({
  workspaces,
  models,
  model,
  setModel,
  onRegister,
  onStart,
  prompt,
  setPrompt,
  busy,
}: {
  workspaces: CodingWorkspace[];
  models: ModelOption[];
  model: string;
  setModel: (m: string) => void;
  onRegister: (name: string, path: string, policy: ApprovalPolicy) => Promise<string | null>;
  onStart: (workspaceId: string) => void;
  prompt: string;
  setPrompt: (p: string) => void;
  busy: boolean;
}) {
  const [workspaceId, setWorkspaceId] = useState<string>("");
  const [showAttach, setShowAttach] = useState(false);
  const [path, setPath] = useState("");
  const [name, setName] = useState("");
  const [policy, setPolicy] = useState<ApprovalPolicy>("approve_writes");

  useEffect(() => {
    if (!workspaceId && workspaces.length) {
      const first = workspaces[0];
      if (first) setWorkspaceId(first.id);
    }
  }, [workspaces, workspaceId]);

  async function attach() {
    if (!path.trim()) return;
    const derivedName = name.trim() || path.trim().replace(/\/+$/, "").split(/[/\\]/).pop() || "workspace";
    const id = await onRegister(derivedName, path.trim(), policy);
    if (id) {
      setWorkspaceId(id);
      setShowAttach(false);
      setPath("");
      setName("");
    }
  }

  return (
    <div style={{ maxWidth: 620, display: "flex", flexDirection: "column", gap: 14 }}>
      <h2 style={{ color: "#e2e8f0", fontSize: 16, margin: 0 }}>Start a coding session</h2>

      <div>
        <label style={label}>Workspace</label>
        <div style={{ display: "flex", gap: 8 }}>
          <select value={workspaceId} onChange={(e) => setWorkspaceId(e.target.value)} style={{ ...input, flex: 1 }}>
            {workspaces.length === 0 && <option value="">No workspaces yet — attach one</option>}
            {workspaces.map((w) => (
              <option key={w.id} value={w.id}>
                {w.name} — {w.root_path} ({w.approval_policy})
              </option>
            ))}
          </select>
          <button onClick={() => setShowAttach((s) => !s)} style={btn(false)}>
            Attach folder
          </button>
        </div>
      </div>

      {showAttach && (
        <div style={{ display: "flex", flexDirection: "column", gap: 8, padding: 12, border: C.border, borderRadius: 8, background: C.panel }}>
          <label style={label}>Absolute folder path on this rig</label>
          <input value={path} onChange={(e) => setPath(e.target.value)} placeholder="/home/you/projects/my-repo" style={input} />
          <div style={{ display: "flex", gap: 8 }}>
            <input value={name} onChange={(e) => setName(e.target.value)} placeholder="Name (optional)" style={{ ...input, flex: 1 }} />
            <select value={policy} onChange={(e) => setPolicy(e.target.value as ApprovalPolicy)} style={input}>
              <option value="approve_writes">Approve writes &amp; commands</option>
              <option value="plan">Plan (approve everything)</option>
              <option value="auto">Auto (no approvals)</option>
            </select>
          </div>
          <button onClick={attach} disabled={busy || !path.trim()} style={primaryBtn}>
            Attach
          </button>
        </div>
      )}

      <div>
        <label style={label}>Model</label>
        <select value={model} onChange={(e) => setModel(e.target.value)} style={{ ...input, width: "100%" }}>
          {models.map((m) => (
            <option key={m.name} value={m.name}>
              {m.name} {m.loaded ? "• loaded" : ""}
            </option>
          ))}
        </select>
      </div>

      <div>
        <label style={label}>Task</label>
        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          placeholder="Describe what you want the agent to do in this repo…"
          rows={4}
          style={{ ...input, width: "100%", resize: "vertical" }}
        />
      </div>

      <button
        onClick={() => workspaceId && onStart(workspaceId)}
        disabled={busy || !workspaceId || !prompt.trim() || !model}
        style={primaryBtn}
      >
        {busy ? "Starting…" : "Start session"}
      </button>
    </div>
  );
}

function LiveTurn({ live, onDecide }: { live: CodingLive; onDecide: (id: string, d: "approved" | "denied") => void }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      {live.status === "loading" && (
        <div style={{ fontSize: 12, color: C.muted }}>Loading model {live.loadingModel}…</div>
      )}
      {live.trace.map((t, i) => (
        <TraceItem key={i} trace={t} />
      ))}
      {live.approvals.map((a) => (
        <div key={a.invocationId} style={{ border: "1px solid rgba(56,189,248,0.5)", borderRadius: 8, padding: 10, background: "rgba(56,189,248,0.08)" }}>
          <div style={{ fontSize: 12, color: C.accent, marginBottom: 6 }}>Approval needed — {a.name}</div>
          <pre style={preStyle}>{a.preview}</pre>
          <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
            <button onClick={() => onDecide(a.invocationId, "approved")} style={primaryBtn}>
              Approve
            </button>
            <button onClick={() => onDecide(a.invocationId, "denied")} style={btn(false)}>
              Deny
            </button>
          </div>
        </div>
      ))}
      {live.text && (
        <div style={bubble("assistant")}>
          <Markdown>{live.text}</Markdown>
        </div>
      )}
      {live.status === "error" && <div style={{ color: "#f87171", fontSize: 12 }}>{live.error}</div>}
    </div>
  );
}

function TraceItem({ trace }: { trace: CodingTrace }) {
  if (trace.kind === "tool") {
    return (
      <div style={{ fontSize: 12, color: C.muted }}>
        ⚙ {trace.name}
        {trace.summary ? ` — ${trace.summary}` : "…"}
      </div>
    );
  }
  if (trace.kind === "file_edit") {
    return (
      <details style={{ border: C.border, borderRadius: 8, padding: 8 }}>
        <summary style={{ fontSize: 12, color: "#e2e8f0", cursor: "pointer" }}>
          ✎ {trace.path} <span style={{ color: C.muted }}>({trace.summary})</span>
        </summary>
        <pre style={preStyle}>{trace.diff}</pre>
      </details>
    );
  }
  return (
    <details style={{ border: C.border, borderRadius: 8, padding: 8 }}>
      <summary style={{ fontSize: 12, color: "#e2e8f0", cursor: "pointer" }}>
        $ {trace.command}{" "}
        <span style={{ color: trace.exitCode === 0 ? "#34d399" : "#f87171" }}>
          [exit {trace.exitCode ?? "?"}]
        </span>
      </summary>
      <pre style={preStyle}>{trace.output}</pre>
    </details>
  );
}

function MessageBubble({ role, content }: { role: string; content: string }) {
  return (
    <div style={bubble(role)}>
      {role === "assistant" ? <Markdown>{content}</Markdown> : <div style={{ whiteSpace: "pre-wrap" }}>{content}</div>}
    </div>
  );
}

function Composer({
  models,
  model,
  setModel,
  prompt,
  setPrompt,
  onSend,
  busy,
  streaming,
  onStop,
}: {
  models: ModelOption[];
  model: string;
  setModel: (m: string) => void;
  prompt: string;
  setPrompt: (p: string) => void;
  onSend: () => void;
  busy: boolean;
  streaming: boolean;
  onStop: () => void;
}) {
  return (
    <div style={{ borderTop: C.border, paddingTop: 10, marginTop: 10, display: "flex", flexDirection: "column", gap: 8 }}>
      <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
        <select value={model} onChange={(e) => setModel(e.target.value)} style={{ ...input, maxWidth: 260 }}>
          {models.map((m) => (
            <option key={m.name} value={m.name}>
              {m.name}
            </option>
          ))}
        </select>
        {streaming && (
          <button onClick={onStop} style={btn(false)}>
            Stop
          </button>
        )}
      </div>
      <div style={{ display: "flex", gap: 8 }}>
        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) onSend();
          }}
          placeholder="Message the coding agent… (Cmd/Ctrl+Enter to send)"
          rows={2}
          style={{ ...input, flex: 1, resize: "vertical" }}
        />
        <button onClick={onSend} disabled={busy || !prompt.trim()} style={primaryBtn}>
          Send
        </button>
      </div>
    </div>
  );
}

// --- styles ---------------------------------------------------------------
const input: React.CSSProperties = {
  background: "rgba(15,23,42,0.6)",
  border: C.border,
  borderRadius: 8,
  color: "#e2e8f0",
  padding: "8px 10px",
  fontSize: 13,
};
const label: React.CSSProperties = { display: "block", fontSize: 12, color: C.muted, marginBottom: 4 };
const preStyle: React.CSSProperties = {
  margin: "6px 0 0",
  padding: 8,
  background: "rgba(2,6,23,0.6)",
  borderRadius: 6,
  fontSize: 12,
  color: "#cbd5e1",
  whiteSpace: "pre-wrap",
  overflowX: "auto",
  maxHeight: 320,
};
const primaryBtn: React.CSSProperties = {
  background: C.accent,
  color: "#0f172a",
  border: "none",
  borderRadius: 8,
  padding: "8px 14px",
  fontSize: 13,
  fontWeight: 600,
  cursor: "pointer",
};
function btn(active: boolean): React.CSSProperties {
  return {
    background: active ? "rgba(56,189,248,0.15)" : "transparent",
    border: C.border,
    borderRadius: 8,
    color: active ? C.accent : "#94a3b8",
    padding: "8px 12px",
    fontSize: 13,
    cursor: "pointer",
  };
}
function sessionBtn(active: boolean): React.CSSProperties {
  return {
    background: active ? "rgba(56,189,248,0.12)" : "transparent",
    border: C.border,
    borderRadius: 8,
    padding: "8px 10px",
    textAlign: "left",
    cursor: "pointer",
  };
}
function bubble(role: string): React.CSSProperties {
  return {
    alignSelf: role === "user" ? "flex-end" : "flex-start",
    maxWidth: "85%",
    background: role === "user" ? "rgba(56,189,248,0.12)" : "rgba(30,41,59,0.5)",
    border: C.border,
    borderRadius: 10,
    padding: "8px 12px",
    fontSize: 14,
    color: "#e2e8f0",
  };
}
