import { useState } from "react";
import { buttonStyle, inputStyle } from "../styles";

export function Composer({
  disabled,
  streaming,
  onSend,
  onStop,
  think,
  canThink,
  onToggleThink,
}: {
  disabled: boolean;
  streaming: boolean;
  onSend: (text: string) => void;
  onStop?: () => void;
  think: boolean;
  canThink: boolean;
  onToggleThink: () => void;
}) {
  const [input, setInput] = useState("");

  const send = () => {
    const text = input.trim();
    if (!text || disabled || streaming) return;
    setInput("");
    onSend(text);
  };

  return (
    <div>
      <div style={{ display: "flex", gap: 6, marginBottom: 6 }}>
        <TogglePill
          on={think && canThink}
          disabled={!canThink}
          title={canThink ? "Stream the model's reasoning" : "This model doesn't support thinking"}
          onClick={onToggleThink}
        >
          💭 Think
        </TogglePill>
      </div>
      <div style={{ display: "flex", gap: 8, alignItems: "flex-end" }}>
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
            disabled={disabled || streaming || !input.trim()}
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
