import { useRef, useState } from "react";
import { buttonStyle, inputStyle, secondaryButton } from "../styles";
import { AttachmentChip } from "./AttachmentChip";
import { FILE_ACCEPT } from "./attachments";
import type { Attachment, ReasoningEffort } from "../types";
import type { ChatContextInfo } from "../types";
import { ContextRing, formatTokens } from "../components/ContextIndicator";
import { EffortSelector } from "../components/EffortSelector";

export function Composer({
  disabled,
  streaming,
  onSend,
  onStop,
  effort,
  canThink,
  onChangeEffort,
  webTools,
  canWebTools,
  webToolsHint,
  onToggleWebTools,
  mcp,
  canMcp,
  onToggleMcp,
  attachments,
  onAddFiles,
  onRemoveAttachment,
  contextInfo,
  compacting,
  onCompact,
  onContextSettings,
}: {
  disabled: boolean;
  streaming: boolean;
  onSend: (text: string) => void;
  onStop?: () => void;
  effort: ReasoningEffort;
  canThink: boolean;
  onChangeEffort: (effort: ReasoningEffort) => void;
  webTools: boolean;
  canWebTools: boolean;
  webToolsHint?: string;
  onToggleWebTools: () => void;
  mcp: boolean;
  canMcp: boolean;
  onToggleMcp: () => void;
  attachments: Attachment[];
  onAddFiles: (files: FileList) => void;
  onRemoveAttachment: (index: number) => void;
  contextInfo: ChatContextInfo | null;
  compacting: boolean;
  onCompact: () => void;
  onContextSettings: (autoCompact: boolean, autoThreshold: number) => void;
}) {
  const [input, setInput] = useState("");
  const [contextOpen, setContextOpen] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);

  const send = () => {
    const text = input.trim();
    if ((!text && attachments.length === 0) || disabled || streaming) return;
    setInput("");
    onSend(text);
  };

  return (
    <div>
      {attachments.length > 0 && (
        <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginBottom: 6 }}>
          {attachments.map((a, i) => (
            <AttachmentChip key={i} attachment={a} onRemove={() => onRemoveAttachment(i)} />
          ))}
        </div>
      )}
      <div style={{ display: "flex", gap: 6, marginBottom: 6 }}>
        <EffortSelector
          value={canThink ? effort : "none"}
          disabled={!canThink}
          onChange={onChangeEffort}
        />
        {canWebTools && (
          <TogglePill
            on={webTools}
            title={webToolsHint ?? "Let the model search and fetch the web"}
            onClick={onToggleWebTools}
          >
            🌐 Web
          </TogglePill>
        )}
        {canMcp && (
          <TogglePill
            on={mcp}
            title="Offer read-only tools from your trusted MCP servers (Tools tab). Mutating tools stay in the coding harness."
            onClick={onToggleMcp}
          >
            🔌 MCP
          </TogglePill>
        )}
        <div style={{ flex: 1 }} />
        <div style={{ position: "relative" }}>
          <button
            onClick={() => setContextOpen((open) => !open)}
            disabled={!contextInfo}
            aria-label={contextInfo ? `Context ${contextInfo.percent}% filled` : "Loading context usage"}
            title={
              contextInfo
                ? `${contextInfo.percent}% filled (${formatTokens(contextInfo.usedTokens)} of ${formatTokens(contextInfo.maxTokens)})`
                : "Loading context usage"
            }
            style={{
              width: 28,
              height: 28,
              padding: 0,
              border: "none",
              borderRadius: 7,
              background: "transparent",
              cursor: contextInfo ? "pointer" : "default",
            }}
          >
            {contextInfo ? <ContextRing percent={contextInfo.percent} level={contextInfo.level} /> : "…"}
          </button>
          {contextOpen && contextInfo && (
            <div
              style={{
                position: "absolute",
                right: 0,
                bottom: "calc(100% + 6px)",
                width: 270,
                padding: 10,
                border: "1px solid #1f2937",
                borderRadius: 10,
                background: "#0b1220",
                boxShadow: "0 12px 32px rgba(0,0,0,0.45)",
                zIndex: 20,
                color: "#cbd5e1",
                fontSize: 12,
              }}
            >
              <div style={{ fontWeight: 600, color: "#e2e8f0" }}>Context window</div>
              <div style={{ marginTop: 6 }}>
                {formatTokens(contextInfo.usedTokens)} / {formatTokens(contextInfo.maxTokens)} ·{" "}
                {contextInfo.countExact ? "exact" : "estimated"}
              </div>
              <div style={{ marginTop: 4, color: "#64748b" }}>
                {formatTokens(contextInfo.reserveTokens)} reserved for generation and tool results
              </div>
              {contextInfo.compacted && (
                <div style={{ marginTop: 4, color: "#64748b" }}>
                  Older turns are represented by a checkpoint.
                </div>
              )}
              {contextInfo.mcpTools > 0 && (
                <div style={{ marginTop: 8, paddingTop: 8, borderTop: "1px solid #1f2937" }}>
                  MCP definitions: {contextInfo.mcpTools} tools · approximately{" "}
                  {formatTokens(contextInfo.mcpSchemaTokens)} tokens ({Math.max(
                    0,
                    Math.round((contextInfo.mcpSchemaTokens / contextInfo.maxTokens) * 100),
                  )}% of context)
                </div>
              )}
              {mcp && contextInfo.mcpTools === 0 && (
                <div style={{ marginTop: 8, paddingTop: 8, borderTop: "1px solid #1f2937" }}>
                  No trusted read-only MCP tools are currently available in Chat.
                </div>
              )}
              <button
                type="button"
                disabled={compacting || streaming}
                onClick={() => { setContextOpen(false); onCompact(); }}
                style={{ ...secondaryButton, width: "100%", marginTop: 8, padding: "6px 8px" }}
              >
                {compacting ? "Compacting…" : "Compact now"}
              </button>
              <label style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginTop: 8 }}>
                <span>Auto compact</span>
                <input
                  type="checkbox"
                  checked={contextInfo.autoCompact}
                  onChange={(event) => onContextSettings(event.target.checked, contextInfo.autoThreshold)}
                />
              </label>
              <label style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginTop: 6 }}>
                <span>Threshold</span>
                <select
                  value={contextInfo.autoThreshold}
                  disabled={!contextInfo.autoCompact}
                  onChange={(event) => onContextSettings(contextInfo.autoCompact, Number(event.target.value))}
                  style={{ ...inputStyle, width: 82, padding: "4px 6px" }}
                >
                  {[70, 75, 80, 85, 90].map((value) => <option key={value} value={value}>{value}%</option>)}
                </select>
              </label>
            </div>
          )}
        </div>
      </div>
      <div style={{ display: "flex", gap: 8, alignItems: "flex-end" }}>
        <input
          ref={fileInput}
          type="file"
          multiple
          accept={FILE_ACCEPT}
          style={{ display: "none" }}
          onChange={(e) => {
            if (e.target.files?.length) onAddFiles(e.target.files);
            e.target.value = "";
          }}
        />
        <button
          onClick={() => fileInput.current?.click()}
          disabled={disabled}
          title="Attach images or text files (or drop them anywhere)"
          style={{ ...secondaryButton, padding: "8px 10px", flexShrink: 0 }}
        >
          📎
        </button>
        <textarea
          placeholder="Message  (Enter to send, Shift+Enter for a new line)"
          value={input}
          rows={Math.min(6, Math.max(1, input.split("\n").length))}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
          style={{
            ...inputStyle,
            marginTop: 0,
            flex: 1,
            resize: "none",
            fontFamily: "inherit",
            fontSize: 13,
            lineHeight: "18px",
          }}
        />
        {streaming && onStop ? (
          <button onClick={onStop} style={{ ...buttonStyle, background: "#f87171" }}>
            Stop
          </button>
        ) : (
          <button
            onClick={send}
            disabled={disabled || streaming || (!input.trim() && attachments.length === 0)}
            style={buttonStyle}
          >
            Send
          </button>
        )}
      </div>
    </div>
  );
}

export function TogglePill({
  on,
  disabled,
  title,
  onClick,
  children,
}: {
  on: boolean;
  disabled?: boolean;
  title?: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      title={title}
      style={{
        padding: "3px 10px",
        borderRadius: 999,
        fontSize: 12,
        cursor: disabled ? "default" : "pointer",
        border: `1px solid ${on ? "rgba(56,189,248,0.6)" : "#1f2937"}`,
        background: on ? "rgba(56,189,248,0.15)" : "transparent",
        color: disabled ? "#475569" : on ? "#38bdf8" : "#94a3b8",
      }}
    >
      {children}
    </button>
  );
}
