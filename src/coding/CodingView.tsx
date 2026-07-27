import { useCallback, useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import {
  codingApprove,
  codingCancel,
  codingCreateSession,
  codingDeleteSession,
  codingGetSession,
  codingListSessions,
  codingPreviewClose,
  codingPreviewFocus,
  codingPreviewReload,
  codingPreviewStatus,
  codingSend,
  codingSetPolicy,
} from "../api";
import type { ApprovalPolicy, CodingPreviewStatus, CodingSessionMeta, CodingStoredMessage, ModelOption } from "../types";
import { Markdown } from "../chat/Markdown";
import { Composer, MODES } from "./Composer";
import { C } from "./tokens";
import { useCodingStream, type CodingLive, type CodingTrace } from "./useCodingStream";


const uuid = () =>
  (crypto.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(16).slice(2)}`);

export function CodingView({ models }: { models: ModelOption[] }) {
  const [sessions, setSessions] = useState<CodingSessionMeta[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [messages, setMessages] = useState<CodingStoredMessage[]>([]);
  const [model, setModel] = useState("");
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Mode + attachments belong to the active session, so both reload with it.
  const [policy, setPolicy] = useState<ApprovalPolicy>("approve_writes");
  const [attachments, setAttachments] = useState<string[]>([]);
  const [preview, setPreview] = useState<CodingPreviewStatus | null>(null);
  const requestIdRef = useRef<string | null>(null);
  const { live, reset } = useCodingStream(activeId);

  const refreshSessions = useCallback(async () => {
    try {
      const s = await codingListSessions();
      setSessions(Array.isArray(s) ? s : []);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refreshSessions();
    // Poll so sessions updated from the web (when enrolled) surface here too.
    const t = setInterval(() => void refreshSessions(), 5000);
    return () => clearInterval(t);
  }, [refreshSessions]);

  useEffect(() => {
    if (!model && models.length) {
      const def = models.find((m) => m.loaded) ?? models[0];
      if (def) setModel(def.name);
    }
  }, [models, model]);

  const loadSession = useCallback(async (id: string) => {
    try {
      const session = await codingGetSession(id);
      setMessages(Array.isArray(session?.messages) ? session.messages : []);
      if (session?.approvalPolicy) setPolicy(session.approvalPolicy);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // Attachments are per-message, so a session switch must not carry them over.
  useEffect(() => {
    setAttachments([]);
  }, [activeId]);

  async function changePolicy(next: ApprovalPolicy) {
    if (!activeId) return;
    const previous = policy;
    setPolicy(next); // optimistic — the menu should feel instant
    try {
      await codingSetPolicy(activeId, next);
      await refreshSessions();
    } catch (e) {
      setPolicy(previous);
      setError(String(e));
    }
  }

  useEffect(() => {
    if (activeId) void loadSession(activeId);
    else setMessages([]);
  }, [activeId, loadSession]);

  useEffect(() => {
    let disposed = false;
    if (activeId) {
      void codingPreviewStatus(activeId)
        .then((status) => { if (!disposed) setPreview(status); })
        .catch(() => { if (!disposed) setPreview(null); });
    } else {
      setPreview(null);
    }
    let unlisten: (() => void) | undefined;
    void listen<CodingPreviewStatus>("coding-preview", ({ payload }) => {
      if (!disposed && payload.sessionId === activeId) setPreview(payload);
    }).then((stop) => { unlisten = stop; });
    return () => { disposed = true; unlisten?.(); };
  }, [activeId]);

  // Follow the transcript as it streams, but only while the user is already at
  // the bottom — scrolling up to read earlier output must not yank them back.
  // Mirrors chat/Conversation.tsx.
  const scrollRef = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  };

  // Re-pin when switching sessions, so a fresh transcript opens at the bottom.
  // Declared first so it applies before the scroll effect in the same commit.
  useEffect(() => {
    pinned.current = true;
  }, [activeId]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el && pinned.current) el.scrollTo(0, el.scrollHeight);
    // Tool calls, diffs and approval prompts all change height mid-turn, so the
    // trace and approvals are tracked alongside the streamed text.
  }, [messages, live.text, live.thinking, live.trace.length, live.approvals.length, live.status]);

  // Fold the live overlay into the persisted transcript when a turn finishes.
  const prevStatus = useRef(live.status);
  useEffect(() => {
    const finished = live.status === "done" || live.status === "error";
    if (prevStatus.current !== live.status && finished) {
      const done = live.status === "done";
      if (activeId) {
        // Reload first, then drop the overlay, so the finished turn is never
        // missing for a frame in between. Without the clear, the overlay stays
        // rendered next to the message it duplicates until the session is
        // reopened. Errors keep it — the failure text was never persisted.
        void loadSession(activeId).then(() => {
          if (done) reset();
        });
      } else if (done) {
        reset();
      }
      void refreshSessions();
    }
    prevStatus.current = live.status;
  }, [live.status, activeId, loadSession, refreshSessions, reset]);

  const streaming = live.status === "loading" || live.status === "streaming" || live.approvals.length > 0;

  async function runTurn(sessionId: string, text: string) {
    const requestId = uuid();
    requestIdRef.current = requestId;
    setBusy(true);
    setError(null);
    try {
      await codingSend(sessionId, requestId, text);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      requestIdRef.current = null;
    }
  }

  async function onSend() {
    if (!activeId || !prompt.trim() || streaming) return;
    // Attachments are workspace-relative paths, so they go in as references the
    // agent opens with its own read tools rather than inlined file contents —
    // that keeps large files out of the transcript and out of the cloud mirror.
    const text = withAttachments(prompt.trim(), attachments);
    setPrompt("");
    setAttachments([]);
    setMessages((m) => [...m, optimisticUser(text)]);
    await runTurn(activeId, text);
  }

  async function onStart(workspacePath: string, policy: ApprovalPolicy) {
    if (!prompt.trim() || !model || !workspacePath.trim()) return;
    const text = prompt.trim();
    setBusy(true);
    setError(null);
    try {
      const session = await codingCreateSession(workspacePath.trim(), model, policy);
      setPrompt("");
      reset();
      setActiveId(session.id);
      setMessages([optimisticUser(text)]);
      await refreshSessions();
      await runTurn(session.id, text);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  }

  async function decide(invocationId: string, approved: boolean) {
    try {
      await codingApprove(invocationId, approved);
    } catch (e) {
      setError(String(e));
    }
  }

  async function onDelete(id: string) {
    if (!confirm("Delete this coding session?")) return;
    try {
      await codingDeleteSession(id);
      if (activeId === id) setActiveId(null);
      await refreshSessions();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div style={{ display: "flex", gap: 12, flex: 1, minHeight: 0, marginTop: 12 }}>
      <aside style={{ width: 240, display: "flex", flexDirection: "column", gap: 8, borderRight: C.border, paddingRight: 12 }}>
        <button onClick={() => { setActiveId(null); reset(); }} style={btn(activeId === null)}>
          + New session
        </button>
        <div style={{ overflowY: "auto", display: "flex", flexDirection: "column", gap: 4 }}>
          {sessions.map((s) => (
            <div key={s.id} style={{ position: "relative" }}>
              <button onClick={() => setActiveId(s.id)} style={sessionBtn(activeId === s.id)}>
                <div style={{ fontSize: 13, color: "#e2e8f0", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                  {s.title || "Coding session"}
                </div>
                <div style={{ fontSize: 11, color: C.muted, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                  {folderName(s.workspaceRoot)} · {s.model}
                </div>
              </button>
            </div>
          ))}
          {sessions.length === 0 && <p style={{ fontSize: 12, color: C.muted }}>No sessions yet.</p>}
        </div>
      </aside>

      <main style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
        {error && <div style={{ color: "#f87171", fontSize: 12, marginBottom: 8 }}>{error}</div>}

        {activeId === null ? (
          <NewSession
            models={models}
            model={model}
            setModel={setModel}
            prompt={prompt}
            setPrompt={setPrompt}
            busy={busy}
            onStart={onStart}
            onError={setError}
          />
        ) : (
          <>
            <SessionHeader meta={sessions.find((s) => s.id === activeId)} onDelete={() => onDelete(activeId)} />
            {preview && (preview.windowOpen || preview.serverState !== "stopped") && (
              <PreviewStrip
                status={preview}
                onFocus={() => void codingPreviewFocus(activeId).catch((e) => setError(String(e)))}
                onReload={() => void codingPreviewReload(activeId).catch((e) => setError(String(e)))}
                onClose={() => void codingPreviewClose(activeId).catch((e) => setError(String(e)))}
              />
            )}
            <div
              ref={scrollRef}
              onScroll={onScroll}
              style={{ flex: 1, minHeight: 0, overflowY: "auto", display: "flex", flexDirection: "column", gap: 12, paddingRight: 4 }}
            >
              {messages.filter((m) => m.role !== "system").map((m, i) => (
                <MessageBubble key={i} role={m.role} content={m.content} />
              ))}
              {live.status !== "idle" && <LiveTurn live={live} onDecide={decide} />}
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
              onStop={() => requestIdRef.current && codingCancel(requestIdRef.current)}
              policy={policy}
              onPolicyChange={(p) => void changePolicy(p)}
              sessionId={activeId}
              attachments={attachments}
              setAttachments={setAttachments}
              onError={setError}
            />
          </>
        )}
      </main>
    </div>
  );
}

export function PreviewStrip({
  status,
  onFocus,
  onReload,
  onClose,
}: {
  status: CodingPreviewStatus;
  onFocus: () => void;
  onReload: () => void;
  onClose: () => void;
}) {
  const ready = status.serverState === "ready";
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8, border: C.border, borderRadius: 8, padding: "7px 9px", marginBottom: 8, background: "rgba(15,23,42,0.55)" }}>
      <span style={{ color: ready ? "#34d399" : C.accent, fontSize: 11 }}>
        {status.serverState === "starting" ? "● starting" : ready ? "● preview ready" : "● preview"}
      </span>
      <span title={status.url ?? undefined} style={{ color: C.muted, fontSize: 11, flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
        {status.url ?? status.serverCommand ?? "Local preview"}
      </span>
      {status.windowOpen && <button onClick={onFocus} style={compactBtn}>Focus</button>}
      {status.windowOpen && <button onClick={onReload} style={compactBtn}>Reload</button>}
      <button onClick={onClose} style={{ ...compactBtn, color: "#f87171" }}>Close</button>
    </div>
  );
}

function NewSession({
  models,
  model,
  setModel,
  prompt,
  setPrompt,
  busy,
  onStart,
  onError,
}: {
  models: ModelOption[];
  model: string;
  setModel: (m: string) => void;
  prompt: string;
  setPrompt: (p: string) => void;
  busy: boolean;
  onStart: (path: string, policy: ApprovalPolicy) => void;
  onError: (error: string | null) => void;
}) {
  const [path, setPath] = useState("");
  const [policy, setPolicy] = useState<ApprovalPolicy>("approve_writes");

  async function chooseFolder() {
    try {
      const picked = await open({ directory: true, multiple: false });
      if (typeof picked !== "string") return;
      setPath(picked);
      onError(null);
    } catch (e) {
      onError(String(e));
    }
  }

  return (
    <div style={{ maxWidth: 640, display: "flex", flexDirection: "column", gap: 14 }}>
      <h2 style={{ color: "#e2e8f0", fontSize: 16, margin: 0 }}>Start a coding session</h2>
      <p style={{ fontSize: 12, color: C.muted, margin: 0 }}>
        Runs entirely on this machine — no account needed. The agent can read, search, edit, and run
        commands only inside the folder you attach.
      </p>

      <div>
        <label style={label}>Project folder</label>
        <div style={{ display: "flex", gap: 8 }}>
          <input
            value={path}
            readOnly
            placeholder="Choose a folder on this machine"
            aria-label="Selected project folder"
            style={{ ...input, flex: 1, minWidth: 0 }}
          />
          <button type="button" onClick={() => void chooseFolder()} style={btn(false)}>
            Choose folder…
          </button>
        </div>
      </div>

      <div style={{ display: "flex", gap: 8 }}>
        <div style={{ flex: 1 }}>
          <label style={label}>Model</label>
          <select value={model} onChange={(e) => setModel(e.target.value)} style={{ ...input, width: "100%" }}>
            {models.length === 0 && <option value="">No local models</option>}
            {models.map((m) => (
              <option key={m.name} value={m.name}>
                {m.name} {m.loaded ? "• loaded" : ""}
              </option>
            ))}
          </select>
        </div>
        <div style={{ flex: 1 }}>
          <label style={label}>Approvals</label>
          <select value={policy} onChange={(e) => setPolicy(e.target.value as ApprovalPolicy)} style={{ ...input, width: "100%" }}>
            {MODES.map((m) => (
              <option key={m.value} value={m.value}>
                {m.label}
              </option>
            ))}
          </select>
        </div>
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
        onClick={() => onStart(path, policy)}
        disabled={busy || !path.trim() || !prompt.trim() || !model}
        style={primaryBtn}
      >
        {busy ? "Starting…" : "Start session"}
      </button>
    </div>
  );
}

function SessionHeader({ meta, onDelete }: { meta?: CodingSessionMeta; onDelete: () => void }) {
  return (
    <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", borderBottom: C.border, paddingBottom: 8, marginBottom: 8 }}>
      <div style={{ minWidth: 0 }}>
        <div style={{ fontSize: 14, color: "#e2e8f0", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {meta?.title ?? "Coding session"}
        </div>
        <div style={{ fontSize: 11, color: C.muted, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {/* Mode lives on the composer pill, which is also where it's changed —
              showing it here too would go briefly stale after a change. */}
          {meta?.workspaceRoot}
        </div>
      </div>
      <button onClick={onDelete} style={{ ...btn(false), color: "#f87171" }}>
        Delete
      </button>
    </div>
  );
}

function LiveTurn({ live, onDecide }: { live: CodingLive; onDecide: (id: string, approved: boolean) => void }) {
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
            <button onClick={() => onDecide(a.invocationId, true)} style={primaryBtn}>
              Approve
            </button>
            <button onClick={() => onDecide(a.invocationId, false)} style={btn(false)}>
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

/** Prefix the prompt with the attached paths, as a plain readable reference list. */
function withAttachments(text: string, attachments: string[]): string {
  if (!attachments.length) return text;
  const list = attachments.map((a) => `- ${a}`).join("\n");
  return `Attached context (read these first):\n${list}\n\n${text}`;
}

function optimisticUser(content: string): CodingStoredMessage {
  return { role: "user", content, thinking: null, toolActivity: null, cancelled: false, createdAt: new Date().toISOString() };
}

function folderName(path: string): string {
  return path.replace(/[/\\]+$/, "").split(/[/\\]/).pop() || path;
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
const compactBtn: React.CSSProperties = {
  background: "transparent",
  border: C.border,
  borderRadius: 6,
  color: "#cbd5e1",
  padding: "4px 7px",
  fontSize: 11,
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
    display: "block",
    width: "100%",
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
