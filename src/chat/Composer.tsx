import { useState } from "react";
import { buttonStyle, inputStyle } from "../styles";

export function Composer({
  disabled,
  streaming,
  onSend,
  onStop,
}: {
  disabled: boolean;
  streaming: boolean;
  onSend: (text: string) => void;
  onStop?: () => void;
}) {
  const [input, setInput] = useState("");

  const send = () => {
    const text = input.trim();
    if (!text || disabled || streaming) return;
    setInput("");
    onSend(text);
  };

  return (
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
        <button onClick={send} disabled={disabled || streaming || !input.trim()} style={buttonStyle}>
          Send
        </button>
      )}
    </div>
  );
}
