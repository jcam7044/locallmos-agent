import { useState } from "react";
import { label } from "../styles";
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
