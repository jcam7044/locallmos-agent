import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { LocalChatEvent } from "../types";

export type StreamState = {
  requestId: string;
  sessionId: string;
  content: string;
  thinking: string;
  /** Short human-readable line for the tool currently running / just finished. */
  toolStatus: string | null;
};

/**
 * Buffers "local-chat" delta events for the turn started with `begin()`.
 * Events for other request ids (e.g. a stale turn) are ignored.
 */
export function useChatStream() {
  const [stream, setStream] = useState<StreamState | null>(null);
  const current = useRef<string | null>(null);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let disposed = false;
    void listen<LocalChatEvent>("local-chat", (e) => {
      const p = e.payload;
      if (p.requestId !== current.current) return;
      setStream((s) => {
        if (!s || s.requestId !== p.requestId) return s;
        switch (p.type) {
          case "content":
            return { ...s, content: s.content + p.delta };
          case "thinking":
            return { ...s, thinking: s.thinking + p.delta };
          case "tool":
            return { ...s, toolStatus: `Running ${p.name}…` };
          case "tool_result":
            return { ...s, toolStatus: `${p.name}: ${p.summary}` };
        }
      });
    }).then((un) => {
      if (disposed) un();
      else unlisten = un;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const begin = (requestId: string, sessionId: string) => {
    current.current = requestId;
    setStream({ requestId, sessionId, content: "", thinking: "", toolStatus: null });
  };

  const end = () => {
    current.current = null;
    setStream(null);
  };

  return { stream, begin, end };
}
