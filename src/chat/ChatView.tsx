import { useEffect, useRef, useState } from "react";
import {
  chatCreateSession,
  chatDeleteSession,
  chatGetSession,
  chatListSessions,
  chatRenameSession,
  chatUpdateSettings,
  localChatCancel,
  localChatSend,
} from "../api";
import { card, label } from "../styles";
import { newUserMessage, type ChatSession, type LocalModel, type SessionMeta } from "../types";
import { Composer } from "./Composer";
import { Conversation } from "./Conversation";
import { ModelPicker } from "./ModelPicker";
import { Sidebar } from "./Sidebar";
import { useChatStream } from "./useChatStream";

export function ChatView({ models, running }: { models: LocalModel[]; running: boolean }) {
  const [sessions, setSessions] = useState<SessionMeta[]>([]);
  const [active, setActive] = useState<ChatSession | null>(null);
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { stream, begin, end } = useChatStream();
  const activeRequest = useRef<string | null>(null);

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
      setActive((s) => (s && s.id === id ? { ...s, title } : s));
      await refreshList();
    } catch (e) {
      setError(String(e));
    }
  };

  const deleteSession = async (id: string) => {
    try {
      await chatDeleteSession(id);
      setActive((s) => (s && s.id === id ? null : s));
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
    if (streaming) return;
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
    if (!session.model) {
      setError("No model selected.");
      return;
    }

    const requestId = crypto.randomUUID();
    activeRequest.current = requestId;
    setError(null);
    // Optimistic user message; the backend persists the same thing.
    setActive({ ...session, messages: [...session.messages, newUserMessage(text)] });
    setStreaming(true);
    begin(requestId, session.id);

    try {
      await localChatSend({ sessionId: session.id, requestId, content: text });
    } catch (e) {
      setError(String(e));
    } finally {
      end();
      setStreaming(false);
      activeRequest.current = null;
      // Re-sync from disk: canonical messages, auto-title, updated ordering.
      try {
        const fresh = await chatGetSession(session.id);
        setActive((cur) => (cur && cur.id === session.id ? fresh : cur));
      } catch {
        /* session may have been deleted meanwhile */
      }
      void refreshList();
    }
  };

  const stop = () => {
    if (activeRequest.current) void localChatCancel(activeRequest.current);
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

        <Conversation
          messages={active?.messages ?? []}
          live={streaming && stream?.sessionId === active?.id ? stream : null}
        />

        <Composer
          disabled={models.length === 0}
          streaming={streaming}
          onSend={(t) => void send(t)}
          onStop={stop}
        />
        {error && <p style={{ color: "#f87171", fontSize: 12, marginTop: 8 }}>{error}</p>}
      </div>
    </div>
  );
}
