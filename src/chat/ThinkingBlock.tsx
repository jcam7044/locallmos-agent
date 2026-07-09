import { useEffect, useRef, useState } from "react";
import { label } from "../styles";

/**
 * Collapsible reasoning block. Auto-opens while thinking streams in, then
 * auto-collapses once the answer starts — unless the user has toggled it.
 */
export function ThinkingBlock({
  thinking,
  streaming,
  hasContent,
}: {
  thinking: string;
  streaming: boolean;
  hasContent: boolean;
}) {
  const [open, setOpen] = useState(streaming && !hasContent);
  const userToggled = useRef(false);

  useEffect(() => {
    if (userToggled.current) return;
    if (streaming) setOpen(!hasContent);
  }, [streaming, hasContent]);

  const scrollRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (open && streaming) scrollRef.current?.scrollTo(0, scrollRef.current.scrollHeight);
  }, [thinking, open, streaming]);

  return (
    <div style={{ marginBottom: 6 }}>
      <button
        onClick={() => {
          userToggled.current = true;
          setOpen((o) => !o);
        }}
        style={{
          border: "none",
          background: "transparent",
          cursor: "pointer",
          padding: 0,
          color: "#94a3b8",
          fontSize: 12,
        }}
      >
        {open ? "▾" : "▸"} Thinking{streaming && !hasContent ? "…" : ""}
      </button>
      {open && (
        <div
          ref={scrollRef}
          style={{
            ...label,
            marginTop: 4,
            padding: "6px 10px",
            borderLeft: "2px solid #1f2937",
            whiteSpace: "pre-wrap",
            maxHeight: 180,
            overflowY: "auto",
            fontStyle: "italic",
          }}
        >
          {thinking}
        </div>
      )}
    </div>
  );
}
