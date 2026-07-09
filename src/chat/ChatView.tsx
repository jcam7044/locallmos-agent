import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { localChat } from "../api";
import { buttonStyle, card, inputStyle, label } from "../styles";
import type { ChatMsg, LocalModel } from "../types";

export function ChatView({ models, running }: { models: LocalModel[]; running: boolean }) {
  const [model, setModel] = useState("");
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  // Default to a loaded model, else the first available.
  useEffect(() => {
    if (!model && models.length > 0) {
      const pick = models.find((m) => m.loaded) ?? models[0];
      if (pick) setModel(pick.name);
    }
  }, [models, model]);

  useEffect(() => {
    scrollRef.current?.scrollTo(0, scrollRef.current.scrollHeight);
  }, [messages]);

  const send = async () => {
    const text = input.trim();
    if (!text || !model || streaming) return;
    const next = [...messages, { role: "user" as const, content: text }];
    setMessages([...next, { role: "assistant", content: "" }]);
    setInput("");
    setStreaming(true);

    const unlisten = await listen<string>("local-chat-delta", (e) => {
      setMessages((m) => {
        const c = [...m];
        const last = c[c.length - 1];
        if (!last) return m;
        c[c.length - 1] = { role: last.role, content: last.content + e.payload };
        return c;
      });
    });
    try {
      const full = await localChat(model, next);
      setMessages((m) => {
        const c = [...m];
        c[c.length - 1] = { role: "assistant", content: full };
        return c;
      });
    } catch (e) {
      setMessages((m) => {
        const c = [...m];
        c[c.length - 1] = { role: "assistant", content: `⚠ ${String(e)}` };
        return c;
      });
    } finally {
      unlisten();
      setStreaming(false);
    }
  };

  if (!running) {
    return (
      <div style={{ ...card, marginTop: 12 }}>
        <p style={{ ...label, margin: 0 }}>Start Ollama to chat with a local model.</p>
      </div>
    );
  }

  return (
    <div style={{ ...card, marginTop: 12, display: "flex", flexDirection: "column", height: 380 }}>
      <select
        value={model}
        onChange={(e) => setModel(e.target.value)}
        style={{ ...inputStyle, marginTop: 0 }}
      >
        {models.length === 0 && <option value="">No models installed</option>}
        {models.map((m) => (
          <option key={m.name} value={m.name}>
            {m.name}
            {m.loaded ? " (loaded)" : ""}
          </option>
        ))}
      </select>

      <div ref={scrollRef} style={{ flex: 1, overflowY: "auto", margin: "10px 0", paddingRight: 4 }}>
        {messages.length === 0 ? (
          <p style={{ ...label, textAlign: "center", marginTop: 24 }}>
            Ask your local model anything — nothing leaves this machine.
          </p>
        ) : (
          messages.map((m, i) => (
            <div
              key={i}
              style={{
                marginBottom: 8,
                textAlign: m.role === "user" ? "right" : "left",
              }}
            >
              <span
                style={{
                  display: "inline-block",
                  maxWidth: "85%",
                  padding: "6px 10px",
                  borderRadius: 10,
                  fontSize: 13,
                  whiteSpace: "pre-wrap",
                  textAlign: "left",
                  background: m.role === "user" ? "#0ea5e9" : "#0b0f17",
                  color: m.role === "user" ? "#04121c" : "#e2e8f0",
                  border: m.role === "user" ? "none" : "1px solid #1f2937",
                }}
              >
                {m.content || (streaming && i === messages.length - 1 ? "…" : "")}
              </span>
            </div>
          ))
        )}
      </div>

      <div style={{ display: "flex", gap: 8 }}>
        <input
          placeholder="Message"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void send();
          }}
          style={{ ...inputStyle, marginTop: 0, flex: 1 }}
        />
        <button onClick={send} disabled={streaming || !input.trim() || !model} style={buttonStyle}>
          {streaming ? "…" : "Send"}
        </button>
      </div>
    </div>
  );
}
