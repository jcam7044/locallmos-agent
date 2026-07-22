import { useState } from "react";
import { label } from "../styles";
import { AttachmentChip } from "./AttachmentChip";
import { Markdown } from "./Markdown";
import { ThinkingBlock } from "./ThinkingBlock";
import type { GenerationMetrics, StoredMessage } from "../types";

export function MessageView({
  message,
  streaming,
  onRegenerate,
}: {
  message: StoredMessage;
  streaming: boolean;
  /** Present only on the last assistant message when idle. */
  onRegenerate?: () => void;
}) {
  const user = message.role === "user";
  const [copied, setCopied] = useState(false);

  const copy = () => {
    void navigator.clipboard.writeText(message.content).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    });
  };

  const tokens =
    message.promptTokens != null || message.completionTokens != null
      ? `${message.promptTokens ?? "?"} in · ${message.completionTokens ?? "?"} out`
      : null;

  return (
    <div style={{ marginBottom: 10, textAlign: user ? "right" : "left" }}>
      {message.attachments.length > 0 && (
        <div
          style={{
            display: "flex",
            flexWrap: "wrap",
            gap: 6,
            marginBottom: 4,
            justifyContent: user ? "flex-end" : "flex-start",
          }}
        >
          {message.attachments.map((a, i) => (
            <AttachmentChip key={i} attachment={a} />
          ))}
        </div>
      )}
      {!user && <ToolActivity activity={message.toolActivity} />}
      {!user && message.toolLimitReached != null && (
        <div
          role="status"
          style={{
            maxWidth: "85%",
            margin: "0 0 6px",
            padding: "7px 9px",
            border: "1px solid #854d0e",
            borderRadius: 8,
            background: "#22180a",
            color: "#fcd34d",
            fontSize: 11,
            lineHeight: 1.4,
            textAlign: "left",
          }}
        >
          ⚠ Maximum tool count reached ({message.toolLimitReached}). The response uses the completed tool results.
        </div>
      )}
      {!user && message.thinking && (
        <ThinkingBlock
          thinking={message.thinking}
          streaming={streaming}
          hasContent={message.content.length > 0}
        />
      )}
      <span
        style={{
          display: "inline-block",
          maxWidth: "85%",
          padding: user ? "6px 10px" : "8px 12px",
          borderRadius: 10,
          fontSize: 13,
          whiteSpace: user ? "pre-wrap" : undefined,
          textAlign: "left",
          background: user ? "#0ea5e9" : "#0b0f17",
          color: user ? "#04121c" : "#e2e8f0",
          border: user ? "none" : "1px solid #1f2937",
        }}
      >
        {user ? (
          message.content
        ) : streaming ? (
          <span style={{ whiteSpace: "pre-wrap" }}>
            {message.content || (!message.thinking ? "…" : "")}
          </span>
        ) : (
          <Markdown>{message.content}</Markdown>
        )}
        {message.cancelled && (
          <span style={{ color: "#94a3b8", fontStyle: "italic" }}> (stopped)</span>
        )}
      </span>

      {!user && !streaming && (
        <div style={{ display: "flex", gap: 10, alignItems: "center", marginTop: 3 }}>
          <ActionLink onClick={copy}>{copied ? "Copied" : "Copy"}</ActionLink>
          {onRegenerate && <ActionLink onClick={onRegenerate}>↻ Regenerate</ActionLink>}
          {tokens && <span style={{ ...label, fontSize: 11 }}>{tokens}</span>}
          {message.generationMetrics?.tokensPerSecond != null && (
            <PerformanceReadout
              metrics={message.generationMetrics}
              promptTokens={message.promptTokens}
              completionTokens={message.completionTokens}
            />
          )}
        </div>
      )}
    </div>
  );
}

function PerformanceReadout({
  metrics,
  promptTokens,
  completionTokens,
}: {
  metrics: GenerationMetrics;
  promptTokens: number | null;
  completionTokens: number | null;
}) {
  const [open, setOpen] = useState(false);
  const rate = metrics.tokensPerSecond;
  if (rate == null || !Number.isFinite(rate)) return null;

  const rows = [
    ["Prompt eval", joinValues(formatNumber(metrics.promptEvalTokens ?? promptTokens), formatMs(metrics.promptEvalMs))],
    ["Prompt speed", formatRate(metrics.promptTokensPerSecond)],
    ["Generation", formatMs(metrics.generationMs)],
    ["Generation speed", formatRate(metrics.tokensPerSecond)],
    ["Output tokens", formatNumber(completionTokens)],
    ["Time to first token", formatMs(metrics.timeToFirstTokenMs)],
    ["Cached prompt", metrics.cachedTokens == null ? null : `${formatNumber(metrics.cachedTokens)} tokens`],
    ["Stream chunks", metrics.streamChunks > 0 ? formatNumber(metrics.streamChunks) : null],
  ].filter((row): row is [string, string] => row[1] != null);

  return (
    <span
      style={{ position: "relative", display: "inline-flex" }}
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
      onFocus={() => setOpen(true)}
      onBlur={() => setOpen(false)}
    >
      <button
        type="button"
        aria-label="Show response performance details"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
        style={{
          border: "none",
          background: "transparent",
          cursor: "pointer",
          padding: 0,
          color: "#7dd3fc",
          fontSize: 11,
          fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
        }}
      >
        {formatRate(rate)}
      </button>
      {open && (
        <span
          role="tooltip"
          style={{
            position: "absolute",
            bottom: "calc(100% + 7px)",
            left: 0,
            zIndex: 20,
            width: 240,
            padding: "9px 10px",
            borderRadius: 8,
            border: "1px solid #334155",
            background: "#101827",
            boxShadow: "0 10px 24px rgba(0, 0, 0, 0.35)",
            color: "#e2e8f0",
            fontSize: 11,
            textAlign: "left",
          }}
        >
          <span style={{ display: "block", color: "#94a3b8", marginBottom: 6 }}>Response performance</span>
          {rows.map(([name, value]) => (
            <span key={name} style={{ display: "flex", justifyContent: "space-between", gap: 12, lineHeight: "18px" }}>
              <span style={{ color: "#94a3b8" }}>{name}</span>
              <span>{value}</span>
            </span>
          ))}
        </span>
      )}
    </span>
  );
}

function formatNumber(value: number | null | undefined): string | null {
  return value == null || !Number.isFinite(value) ? null : value.toLocaleString();
}

function formatRate(value: number | null | undefined): string | null {
  return value == null || !Number.isFinite(value) ? null : `${value.toFixed(1)} tok/s`;
}

function formatMs(value: number | null | undefined): string | null {
  if (value == null || !Number.isFinite(value)) return null;
  return value >= 1000 ? `${(value / 1000).toFixed(2)} s` : `${Math.round(value)} ms`;
}

function joinValues(...values: (string | null)[]): string | null {
  const present = values.filter((value): value is string => value != null);
  return present.length ? present.join(" · ") : null;
}

type ToolActivityRow = {
  name?: string;
  query?: string;
  citations?: { title?: string; url?: string }[];
};

/** Persisted tool usage (web searches/fetches with citations) for a reply. */
function ToolActivity({ activity }: { activity: unknown }) {
  if (!Array.isArray(activity) || activity.length === 0) return null;
  return (
    <div style={{ marginBottom: 6 }}>
      {(activity as ToolActivityRow[]).map((row, i) => (
        <div key={i} style={{ ...label, fontSize: 11, margin: "2px 0" }}>
          ⚙ {row.name ?? "tool"}
          {row.query ? `: ${row.query}` : ""}
          {(row.citations ?? []).slice(0, 5).map((c, j) =>
            c.url ? (
              <a
                key={j}
                href={c.url}
                target="_blank"
                rel="noreferrer"
                title={c.title ?? c.url}
                style={{ color: "#38bdf8", marginLeft: 6 }}
              >
                [{j + 1}]
              </a>
            ) : null,
          )}
        </div>
      ))}
    </div>
  );
}

function ActionLink({ onClick, children }: { onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      style={{
        border: "none",
        background: "transparent",
        cursor: "pointer",
        padding: 0,
        fontSize: 11,
        color: "#64748b",
      }}
    >
      {children}
    </button>
  );
}
