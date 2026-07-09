import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  chatCreateSession,
  chatDeleteSession,
  chatGetSession,
  chatListSessions,
  chatRenameSession,
  chatUpdateSettings,
  localChat,
} from "../api";
import { card, label } from "../styles";
import { newUserMessage, type ChatSession, type LocalModel, type SessionMeta } from "../types";
import { Composer } from "./Composer";
import { Conversation } from "./Conversation";
import { ModelPicker } from "./ModelPicker";
import { Sidebar } from "./Sidebar";

export function ChatView({ models, running }: { models: LocalModel[]; running: boolean }) {
  const [sessions, setSessions] = useState<SessionMeta[]>([]);
  const [active, setActive] = useState<ChatSession | null>(null);
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The session being streamed into; guards against switching mid-stream.
  const streamTarget = useRef<string | null>(null);

  const defaultModel = () => (models.find((m) => m.loaded) ?? models[0])?.name ?? "";

  const refreshList = async () => {
    try {
      setSessions(await chatListSessions());
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void refreshList();
  }, []);

  const openSession = async (id: string) => {
    if (streaming) return;
    try {
      setActive(await chatGetSession(id));
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  const newSession = async () => {
    if (streaming) return;
    try {
      const s = await chatCreateSession(defaultModel());
      setActive(s);
      await refreshList();
    } catch (e) {
      setError(String(e));
    }
  };

  const renameSession = async (id: string, title: string) => {
    try {
      await chatRenameSession(id, title);
      if (active?.id === id) setActive({ ...active, title });
      await refreshList();
    } catch (e) {
      setError(String(e));
    }
  };

  const deleteSession = async (id: string) => {
    try {
      await chatDeleteSession(id);
      if (active?.id === id) setActive(null);
      await refreshList();
    } catch (e) {
      setError(String(e));
    }
  };

  const setModel = (model: string) => {
    if (!active) return;
    const next = { ...active, model };
    setActive(next);
    chatUpdateSettings(next.id, model, next.settings).catch((e) => setError(String(e)));
  };

  const send = async (text: string) => {
    let session = active;
    if (!session) {
      // First message with no session yet: create one on the fly.
      try {
        session = await chatCreateSession(defaultModel());
        setActive(session);
        await refreshList();
      } catch (e) {
        setError(String(e));
        return;
      }
    }
    if (!session.model || streaming) return;

    const history = [...session.messages, newUserMessage(text)];
    const pending = { ...session, messages: [...history, { ...newUserMessage(""), role: "assistant" as const }] };
    setActive(pending);
    setStreaming(true);
    streamTarget.current = session.id;

    const unlisten = await listen<string>("local-chat-delta", (e) => {
      setActive((s) => {
        if (!s || s.id !== streamTarget.current) return s;
        const msgs = [...s.messages];
        const last = msgs[msgs.length - 1];
        if (!last) return s;
        msgs[msgs.length - 1] = { ...last, content: last.content + e.payload };
        return { ...s, messages: msgs };
      });
    });
    try {
      const full = await localChat(
        session.model,
        history.map((m) => ({ role: m.role, content: m.content })),
      );
      setActive((s) => {
        if (!s || s.id !== streamTarget.current) return s;
        const msgs = [...s.messages];
        msgs[msgs.length - 1] = { ...msgs[msgs.length - 1]!, content: full };
        return { ...s, messages: msgs };
      });
    } catch (e) {
      setActive((s) => {
        if (!s || s.id !== streamTarget.current) return s;
        const msgs = [...s.messages];
        msgs[msgs.length - 1] = { ...msgs[msgs.length - 1]!, content: `⚠ ${String(e)}` };
        return { ...s, messages: msgs };
      });
    } finally {
      unlisten();
      setStreaming(false);
      streamTarget.current = null;
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
    <div style={{ display: "flex", gap: 12, marginTop: 12, flex: 1, minHeight: 0 }}>
      <Sidebar
        sessions={sessions}
        activeId={active?.id ?? null}
        onNew={newSession}
        onSelect={openSession}
        onRename={renameSession}
        onDelete={deleteSession}
      />

      <div
        style={{
          ...card,
          flex: 1,
          minWidth: 0,
          display: "flex",
          flexDirection: "column",
          minHeight: 0,
        }}
      >
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <ModelPicker models={models} value={active?.model ?? defaultModel()} onChange={setModel} />
          {active && (
            <span style={{ ...label, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
              {active.title}
            </span>
          )}
        </div>

        <Conversation messages={active?.messages ?? []} streaming={streaming} />

        <Composer
          disabled={models.length === 0}
          streaming={streaming}
          onSend={(t) => void send(t)}
        />
        {error && <p style={{ color: "#f87171", fontSize: 12, marginTop: 8 }}>{error}</p>}
      </div>
    </div>
  );
}
