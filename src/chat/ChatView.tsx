import { useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  chatCreateSession,
  chatDeleteSession,
  chatGetSession,
  chatListSessions,
  chatRenameSession,
  chatUpdateSettings,
  localChatCancel,
  localChatSend,
  readDroppedFile,
} from "../api";
import { card, label, secondaryButton } from "../styles";
import {
  newUserMessage,
  type Attachment,
  type ChatSession,
  type LocalModel,
  type SessionMeta,
  type SessionSettings,
} from "../types";
import { fileToAttachment } from "./attachments";
import { Composer } from "./Composer";
import { Conversation } from "./Conversation";
import { ModelPicker } from "./ModelPicker";
import { SessionSettingsPanel } from "./SessionSettingsPanel";
import { Sidebar } from "./Sidebar";
import { useChatStream } from "./useChatStream";

export function ChatView({
  models,
  running,
  enrolled,
}: {
  models: LocalModel[];
  running: boolean;
  enrolled: boolean;
}) {
  const [sessions, setSessions] = useState<SessionMeta[]>([]);
  const [active, setActive] = useState<ChatSession | null>(null);
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const { stream, begin, end } = useChatStream();
  const activeRequest = useRef<string | null>(null);
  const saveTimer = useRef<number | undefined>(undefined);

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

  // Debounced so typing in the system-prompt field doesn't write per keystroke.
  const patchSettings = (patch: Partial<SessionSettings>) => {
    if (!active) return;
    const next = { ...active, settings: { ...active.settings, ...patch } };
    setActive(next);
    window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      chatUpdateSettings(next.id, next.model, next.settings).catch((e) => setError(String(e)));
    }, 400);
  };

  const selectedModel = models.find((m) => m.name === active?.model);
  const canThink = selectedModel?.capabilities.includes("thinking") ?? false;
  const canVision = selectedModel?.capabilities.includes("vision") ?? false;
  const canWebTools = selectedModel?.capabilities.includes("tools") ?? false;
  const canVisionRef = useRef(canVision);
  canVisionRef.current = canVision;

  // --- Attachments -----------------------------------------------------------
  const [pending, setPending] = useState<Attachment[]>([]);

  const addAttachment = (a: Attachment) => {
    if (a.kind === "image" && !canVisionRef.current) {
      setError(`${a.name}: the selected model doesn't support images.`);
      return;
    }
    setPending((p) => [...p, a]);
    setError(null);
  };

  const addFiles = (files: FileList) => {
    for (const f of Array.from(files)) {
      fileToAttachment(f).then(addAttachment, (e) => setError(String(e)));
    }
  };

  // Native drag-drop delivers file *paths* (Tauri intercepts HTML5 drops);
  // the backend reads them into attachments.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let disposed = false;
    void getCurrentWebview()
      .onDragDropEvent((e) => {
        if (e.payload.type !== "drop") return;
        for (const path of e.payload.paths) {
          readDroppedFile(path).then(addAttachment, (err) => setError(String(err)));
        }
      })
      .then((un) => {
        if (disposed) un();
        else unlisten = un;
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // Run one turn: optimistic local update, stream, then re-sync from disk
  // (canonical messages, auto-title, updated ordering).
  const runTurn = async (
    session: ChatSession,
    args: { content: string; attachments?: Attachment[]; regenerate?: boolean },
  ) => {
    const requestId = crypto.randomUUID();
    activeRequest.current = requestId;
    setError(null);
    setStreaming(true);
    begin(requestId, session.id);

    try {
      await localChatSend({ sessionId: session.id, requestId, ...args });
    } catch (e) {
      setError(String(e));
    } finally {
      end();
      setStreaming(false);
      activeRequest.current = null;
      try {
        const fresh = await chatGetSession(session.id);
        setActive((cur) => (cur && cur.id === session.id ? fresh : cur));
      } catch {
        /* session may have been deleted meanwhile */
      }
      void refreshList();
    }
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
    const attachments = pending;
    setPending([]);
    // Optimistic user message; the backend persists the same thing.
    setActive({ ...session, messages: [...session.messages, newUserMessage(text, attachments)] });
    await runTurn(session, { content: text, attachments });
  };

  const regenerate = async () => {
    if (!active || streaming) return;
    if (active.messages[active.messages.length - 1]?.role !== "assistant") return;
    // Optimistically drop the reply being regenerated; the backend does the same.
    setActive({ ...active, messages: active.messages.slice(0, -1) });
    await runTurn(active, { content: "", regenerate: true });
  };

  const stop = () => {
    if (activeRequest.current) void localChatCancel(activeRequest.current);
  };

  if (!running) {
    return (
      <div style={{ ...card, marginTop: 12 }}>
        <p style={{ ...label, margin: 0 }}>
          No local model running — load one from the Dashboard tab to chat.
        </p>
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
            <span
              style={{
                ...label,
                flex: 1,
                minWidth: 0,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {active.title}
            </span>
          )}
          {active && (
            <button
              onClick={() => setSettingsOpen((o) => !o)}
              title="Session settings"
              style={{
                ...secondaryButton,
                marginLeft: "auto",
                flexShrink: 0,
                ...(settingsOpen ? { borderColor: "rgba(56,189,248,0.6)", color: "#38bdf8" } : {}),
              }}
            >
              ⚙
            </button>
          )}
        </div>

        {settingsOpen && active && (
          <SessionSettingsPanel settings={active.settings} onChange={patchSettings} />
        )}

        <Conversation
          messages={active?.messages ?? []}
          live={streaming && stream?.sessionId === active?.id ? stream : null}
          onRegenerate={() => void regenerate()}
        />

        <Composer
          disabled={models.length === 0}
          streaming={streaming}
          onSend={(t) => void send(t)}
          onStop={stop}
          think={active?.settings.think ?? false}
          canThink={canThink}
          onToggleThink={() => patchSettings({ think: !(active?.settings.think ?? false) })}
          webTools={active?.settings.webTools ?? false}
          canWebTools={canWebTools}
          webToolsHint={
            enrolled
              ? "Let the model search and fetch the web"
              : "Page fetch works offline; web search needs a cloud connection"
          }
          onToggleWebTools={() => patchSettings({ webTools: !(active?.settings.webTools ?? false) })}
          attachments={pending}
          onAddFiles={addFiles}
          onRemoveAttachment={(i) => setPending((p) => p.filter((_, idx) => idx !== i))}
        />
        {error && <p style={{ color: "#f87171", fontSize: 12, marginTop: 8 }}>{error}</p>}
      </div>
    </div>
  );
}
