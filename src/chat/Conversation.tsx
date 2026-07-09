import { useEffect, useRef } from "react";
import { label } from "../styles";
import { MessageView } from "./MessageView";
import { newUserMessage, type StoredMessage } from "../types";
import type { StreamState } from "./useChatStream";

export function Conversation({
  messages,
  live,
}: {
  messages: StoredMessage[];
  /** In-flight assistant reply being streamed, if any. */
  live: StreamState | null;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  };

  useEffect(() => {
    const el = scrollRef.current;
    if (el && pinned.current) el.scrollTo(0, el.scrollHeight);
  }, [messages, live?.content, live?.thinking]);

  const liveMessage: StoredMessage | null = live
    ? {
        ...newUserMessage(live.content),
        role: "assistant",
        thinking: live.thinking || null,
      }
    : null;

  return (
    <div
      ref={scrollRef}
      onScroll={onScroll}
      style={{ flex: 1, overflowY: "auto", minHeight: 0, padding: "10px 4px 10px 0" }}
    >
      {messages.length === 0 && !liveMessage ? (
        <p style={{ ...label, textAlign: "center", marginTop: 48 }}>
          Ask your local model anything — nothing leaves this machine.
        </p>
      ) : (
        <>
          {messages.map((m, i) => (
            <MessageView key={i} message={m} streaming={false} />
          ))}
          {liveMessage && (
            <>
              <MessageView message={liveMessage} streaming />
              {live?.toolStatus && (
                <p style={{ ...label, margin: "2px 0 8px 2px" }}>⚙ {live.toolStatus}</p>
              )}
            </>
          )}
        </>
      )}
    </div>
  );
}
