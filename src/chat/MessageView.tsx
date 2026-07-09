import type { StoredMessage } from "../types";

export function MessageView({
  message,
  streaming,
}: {
  message: StoredMessage;
  streaming: boolean;
}) {
  const user = message.role === "user";
  return (
    <div style={{ marginBottom: 10, textAlign: user ? "right" : "left" }}>
      <span
        style={{
          display: "inline-block",
          maxWidth: "85%",
          padding: "6px 10px",
          borderRadius: 10,
          fontSize: 13,
          whiteSpace: "pre-wrap",
          textAlign: "left",
          background: user ? "#0ea5e9" : "#0b0f17",
          color: user ? "#04121c" : "#e2e8f0",
          border: user ? "none" : "1px solid #1f2937",
        }}
      >
        {message.content || (streaming ? "…" : "")}
        {message.cancelled && (
          <span style={{ color: "#94a3b8", fontStyle: "italic" }}> (stopped)</span>
        )}
      </span>
    </div>
  );
}
