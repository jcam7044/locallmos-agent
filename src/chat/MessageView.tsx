import { useState } from "react";
import { label } from "../styles";
import { AttachmentChip } from "./AttachmentChip";
import { Markdown } from "./Markdown";
import { ThinkingBlock } from "./ThinkingBlock";
import type { StoredMessage } from "../types";

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
        </div>
      )}
    </div>
  );
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
