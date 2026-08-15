import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { codingAttach } from "../api";
import type { ApprovalPolicy, CodingContextInfo, ModelOption } from "../types";
import { C } from "./tokens";
import { ContextRing, formatTokens } from "../components/ContextIndicator";

export { ContextRing } from "../components/ContextIndicator";

/**
 * The session's mode. One menu covers both axes the backend exposes: whether
 * mutating tools exist at all, and whether they pause for approval. Ordered
 * least- to most-capable so the numeric shortcuts read as escalating trust.
 */
export const MODES: {
  value: ApprovalPolicy;
  label: string;
  hint: string;
}[] = [
  { value: "read_only", label: "Read-only", hint: "Inspect and answer. Cannot edit or run commands." },
  { value: "plan", label: "Plan", hint: "Research, then propose a plan. Cannot edit or run commands." },
  { value: "approve_writes", label: "Approve edits", hint: "Edits and commands pause for your approval." },
  { value: "auto", label: "Auto", hint: "Runs unattended inside the workspace folder." },
];

export const modeLabel = (p: ApprovalPolicy) =>
  MODES.find((m) => m.value === p)?.label ?? p;

export function Composer({
  models,
  model,
  setModel,
  prompt,
  setPrompt,
  onSend,
  busy,
  streaming,
  onStop,
  policy,
  onPolicyChange,
  mcpEnabled,
  onMcpToggle,
  sessionId,
  attachments,
  setAttachments,
  onError,
  contextInfo,
  compacting,
  onCompact,
  onContextSettings,
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
  policy: ApprovalPolicy;
  onPolicyChange: (p: ApprovalPolicy) => void;
  mcpEnabled: boolean;
  onMcpToggle: (enabled: boolean) => void;
  sessionId: string;
  attachments: string[];
  setAttachments: (a: string[]) => void;
  onError: (e: string | null) => void;
  contextInfo: CodingContextInfo | null;
  compacting: boolean;
  onCompact: () => void;
  onContextSettings: (autoCompact: boolean, autoThreshold: number) => void;
}) {
  const [menu, setMenu] = useState<null | "mode" | "add" | "context">(null);
  const boxRef = useRef<HTMLDivElement>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);

  // Close either popup on an outside click or Escape.
  useEffect(() => {
    if (!menu) return;
    const onDown = (e: MouseEvent) => {
      if (!boxRef.current?.contains(e.target as Node)) setMenu(null);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setMenu(null);
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  // Grow with the text instead of showing a scrollbar at two rows.
  useEffect(() => {
    const el = taRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 220)}px`;
  }, [prompt]);

  async function pick(directory: boolean) {
    setMenu(null);
    try {
      const picked = await open({ multiple: !directory, directory });
      if (!picked) return;
      const paths = Array.isArray(picked) ? picked : [picked];
      // The backend rejects anything outside the workspace — the agent's tools
      // could not open it, so an out-of-tree attachment would be a dead link.
      const rel = await codingAttach(sessionId, paths);
      onError(null);
      setAttachments([...new Set([...attachments, ...rel])]);
    } catch (e) {
      onError(String(e));
    }
  }

  const canSend = !busy && !streaming && !!prompt.trim();

  return (
    <div ref={boxRef} style={{ position: "relative", marginTop: 10 }}>
      {attachments.length > 0 && (
        <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginBottom: 6 }}>
          {attachments.map((a) => (
            <span key={a} style={chip}>
              <span style={{ opacity: 0.65 }}>@</span>
              {a}
              <button
                onClick={() => setAttachments(attachments.filter((x) => x !== a))}
                style={chipX}
                aria-label={`Remove ${a}`}
              >
                ×
              </button>
            </span>
          ))}
        </div>
      )}

      <div style={box}>
        <textarea
          ref={taRef}
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          onKeyDown={(e) => {
            // Enter sends; Shift+Enter inserts a newline. Skip while an IME is
            // composing so Enter can commit a candidate without sending. Cmd/Ctrl+
            // Enter still sends too, for muscle memory.
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              if (canSend) onSend();
            }
          }}
          placeholder="Message the coding agent…"
          rows={1}
          style={textarea}
        />

        <div style={bar}>
          <button onClick={() => setMenu(menu === "add" ? null : "add")} style={iconBtn} title="Add context">
            +
          </button>

          <button onClick={() => setMenu(menu === "mode" ? null : "mode")} style={pill}>
            {modeLabel(policy)}
            <span style={{ opacity: 0.5, fontSize: 10 }}>▾</span>
          </button>

          <button
            onClick={() => onMcpToggle(!mcpEnabled)}
            style={{ ...pill, opacity: mcpEnabled ? 1 : 0.5 }}
            title={
              mcpEnabled
                ? contextInfo
                  ? `${contextInfo.mcpTools} MCP tools use approximately ${formatTokens(contextInfo.mcpSchemaTokens)} context tokens.`
                  : "MCP tools are available to this session. Manage servers in the Tools tab."
                : "Enable the configured MCP servers' tools for this session."
            }
          >
            MCP {mcpEnabled ? "on" : "off"}
          </button>

          <div style={{ flex: 1 }} />

          <button
            onClick={() => setMenu(menu === "context" ? null : "context")}
            style={contextPill}
            title={contextInfo ? `${contextInfo.percent}% filled (${formatTokens(contextInfo.usedTokens)} of ${formatTokens(contextInfo.maxTokens)}); ${contextInfo.countExact ? "exact" : "estimated"} input usage; ${formatTokens(contextInfo.reserveTokens)} safety reserve for generation and tools` : "Loading context usage"}
            aria-label={contextInfo ? `Context ${contextInfo.percent}% filled` : "Loading context usage"}
          >
            {compacting ? (
              <span style={{ ...ringLoading, color: C.muted }}>●</span>
            ) : contextInfo ? (
              <ContextRing percent={contextInfo.percent} level={contextInfo.level} />
            ) : (
              <span style={{ fontSize: 10 }}>…</span>
            )}
          </button>

          <select
            value={model}
            onChange={(e) => setModel(e.target.value)}
            style={modelSelect}
            title="Model"
          >
            {models.length === 0 && <option value="">No local models</option>}
            {models.map((m) => (
              <option key={m.name} value={m.name}>
                {m.name}
              </option>
            ))}
          </select>

          {streaming ? (
            <button onClick={onStop} style={{ ...sendBtn, background: "rgba(148,163,184,0.2)" }} title="Stop">
              ■
            </button>
          ) : (
            <button
              onClick={onSend}
              disabled={!canSend}
              style={{ ...sendBtn, opacity: canSend ? 1 : 0.35, cursor: canSend ? "pointer" : "default" }}
              title="Send (Enter · Shift+Enter for newline)"
            >
              ↑
            </button>
          )}
        </div>
      </div>

      {menu === "add" && (
        <Popup style={{ left: 0 }}>
          <MenuItem label="Add files" onClick={() => void pick(false)} />
          <MenuItem label="Add folder" onClick={() => void pick(true)} />
          <div style={menuNote}>Must be inside the workspace folder.</div>
        </Popup>
      )}

      {menu === "mode" && (
        <Popup style={{ left: 44 }}>
          <div style={menuHead}>Mode</div>
          {MODES.map((m, i) => (
            <MenuItem
              key={m.value}
              label={m.label}
              hint={m.hint}
              shortcut={String(i + 1)}
              checked={m.value === policy}
              onClick={() => {
                setMenu(null);
                if (m.value !== policy) onPolicyChange(m.value);
              }}
            />
          ))}
        </Popup>
      )}

      {menu === "context" && contextInfo && (
        <Popup style={{ right: 36 }}>
          <div style={menuHead}>Context window</div>
          <div style={contextSummary}>
            <strong>{formatTokens(contextInfo.usedTokens)} / {formatTokens(contextInfo.maxTokens)}</strong>
            <span>{contextInfo.countExact ? "Exact count" : "Estimated count"} · {formatTokens(contextInfo.reserveTokens)} reserved</span>
            {contextInfo.mcpTools > 0 && (
              <span>
                MCP definitions: {contextInfo.mcpTools} tools · approximately{" "}
                {formatTokens(contextInfo.mcpSchemaTokens)} tokens ({Math.max(
                  0,
                  Math.round((contextInfo.mcpSchemaTokens / contextInfo.maxTokens) * 100),
                )}% of context)
              </span>
            )}
            {contextInfo.compacted && <span>Older turns are represented by a checkpoint.</span>}
          </div>
          <MenuItem
            label={compacting ? "Compacting…" : "Compact now"}
            hint="Preserves the transcript and replaces older model context with a structured checkpoint. You can also type /compact."
            onClick={() => { setMenu(null); if (!compacting) onCompact(); }}
          />
          <label style={contextSetting}>
            <span>Auto compact</span>
            <input
              type="checkbox"
              checked={contextInfo.autoCompact}
              onChange={(event) => onContextSettings(event.target.checked, contextInfo.autoThreshold)}
            />
          </label>
          <label style={contextSetting}>
            <span>Threshold</span>
            <select
              value={contextInfo.autoThreshold}
              disabled={!contextInfo.autoCompact}
              onChange={(event) => onContextSettings(contextInfo.autoCompact, Number(event.target.value))}
              style={thresholdSelect}
            >
              {[70, 75, 80, 85, 90].map((value) => <option key={value} value={value}>{value}%</option>)}
            </select>
          </label>
        </Popup>
      )}
    </div>
  );
}

function Popup({ children, style }: { children: React.ReactNode; style?: React.CSSProperties }) {
  return (
    <div
      style={{
        position: "absolute",
        bottom: "calc(100% + 6px)",
        minWidth: 240,
        background: "#0b1220",
        border: C.border,
        borderRadius: 10,
        padding: 4,
        boxShadow: "0 12px 32px rgba(0,0,0,0.45)",
        zIndex: 20,
        ...style,
      }}
    >
      {children}
    </div>
  );
}

function MenuItem({
  label,
  hint,
  shortcut,
  checked,
  onClick,
}: {
  label: string;
  hint?: string;
  shortcut?: string;
  checked?: boolean;
  onClick: () => void;
}) {
  const [hover, setHover] = useState(false);
  return (
    <button
      onClick={onClick}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        display: "block",
        width: "100%",
        textAlign: "left",
        background: hover ? "rgba(148,163,184,0.12)" : "transparent",
        border: "none",
        borderRadius: 7,
        color: "#e2e8f0",
        padding: "7px 9px",
        fontSize: 13,
        cursor: "pointer",
      }}
    >
      <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <span style={{ flex: 1 }}>{label}</span>
        {checked && <span style={{ color: C.accent }}>✓</span>}
        {shortcut && <span style={{ color: C.muted, fontSize: 11 }}>{shortcut}</span>}
      </span>
      {hint && <span style={{ display: "block", color: C.muted, fontSize: 11, marginTop: 2 }}>{hint}</span>}
    </button>
  );
}

// --- styles ---------------------------------------------------------------
const box: React.CSSProperties = {
  background: "rgba(15,23,42,0.55)",
  border: C.border,
  borderRadius: 14,
  padding: "8px 8px 6px",
  display: "flex",
  flexDirection: "column",
  gap: 4,
};
const textarea: React.CSSProperties = {
  background: "transparent",
  border: "none",
  outline: "none",
  resize: "none",
  color: "#e2e8f0",
  fontSize: 14,
  lineHeight: 1.5,
  padding: "6px 6px 2px",
  fontFamily: "inherit",
  maxHeight: 220,
};
const bar: React.CSSProperties = { display: "flex", alignItems: "center", gap: 6 };
const iconBtn: React.CSSProperties = {
  width: 26,
  height: 26,
  borderRadius: 8,
  background: "transparent",
  border: C.border,
  color: "#cbd5e1",
  fontSize: 15,
  lineHeight: 1,
  cursor: "pointer",
};
const pill: React.CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: 5,
  height: 26,
  padding: "0 9px",
  borderRadius: 8,
  background: "transparent",
  border: C.border,
  color: "#cbd5e1",
  fontSize: 12,
  cursor: "pointer",
};
const modelSelect: React.CSSProperties = {
  height: 26,
  maxWidth: 200,
  borderRadius: 8,
  background: "transparent",
  border: C.border,
  color: C.muted,
  fontSize: 12,
  padding: "0 6px",
  cursor: "pointer",
};
const contextPill: React.CSSProperties = {
  width: 28,
  height: 28,
  borderRadius: 7,
  background: "transparent",
  border: "none",
  padding: 0,
  cursor: "pointer",
  whiteSpace: "nowrap",
};
const ringLoading: React.CSSProperties = {
  width: 16,
  height: 16,
  fontSize: 8,
  lineHeight: "16px",
  textAlign: "center",
  opacity: 0.7,
};
const contextSummary: React.CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: 3,
  color: C.muted,
  fontSize: 11,
  padding: "5px 9px 8px",
  borderBottom: C.border,
};
const contextSetting: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  gap: 12,
  color: "#cbd5e1",
  fontSize: 12,
  padding: "7px 9px",
};
const thresholdSelect: React.CSSProperties = {
  background: "#111827",
  border: C.border,
  borderRadius: 6,
  color: "#cbd5e1",
  fontSize: 11,
  padding: "2px 5px",
};
const sendBtn: React.CSSProperties = {
  width: 28,
  height: 28,
  borderRadius: "50%",
  border: "none",
  background: C.accent,
  color: "#0b1220",
  fontSize: 14,
  fontWeight: 700,
  cursor: "pointer",
};
const chip: React.CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: 4,
  padding: "3px 6px 3px 8px",
  borderRadius: 7,
  background: "rgba(56,189,248,0.10)",
  border: "1px solid rgba(56,189,248,0.25)",
  color: "#cbd5e1",
  fontSize: 11,
  fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
};
const chipX: React.CSSProperties = {
  background: "transparent",
  border: "none",
  color: C.muted,
  cursor: "pointer",
  fontSize: 13,
  lineHeight: 1,
  padding: "0 1px",
};
const menuHead: React.CSSProperties = {
  color: C.muted,
  fontSize: 11,
  padding: "5px 9px 3px",
};
const menuNote: React.CSSProperties = {
  color: C.muted,
  fontSize: 11,
  padding: "4px 9px 5px",
  borderTop: C.border,
  marginTop: 3,
};
