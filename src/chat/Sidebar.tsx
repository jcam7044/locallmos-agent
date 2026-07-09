import { useEffect, useRef, useState } from "react";
import { buttonStyle, inputStyle, label } from "../styles";
import type { SessionMeta } from "../types";

export function Sidebar({
  sessions,
  activeId,
  onNew,
  onSelect,
  onRename,
  onDelete,
}: {
  sessions: SessionMeta[];
  activeId: string | null;
  onNew: () => void;
  onSelect: (id: string) => void;
  onRename: (id: string, title: string) => void;
  onDelete: (id: string) => void;
}) {
  const [search, setSearch] = useState("");
  const filtered = search.trim()
    ? sessions.filter((s) => s.title.toLowerCase().includes(search.trim().toLowerCase()))
    : sessions;

  return (
    <div
      style={{
        width: 220,
        flexShrink: 0,
        display: "flex",
        flexDirection: "column",
        gap: 8,
        minHeight: 0,
      }}
    >
      <button onClick={onNew} style={{ ...buttonStyle, width: "100%" }}>
        + New chat
      </button>
      <input
        placeholder="Search chats"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        style={{ ...inputStyle, marginTop: 0 }}
      />
      <div style={{ flex: 1, overflowY: "auto", minHeight: 0 }}>
        {filtered.length === 0 ? (
          <p style={{ ...label, textAlign: "center", marginTop: 16 }}>
            {sessions.length === 0 ? "No chats yet" : "No matches"}
          </p>
        ) : (
          filtered.map((s) => (
            <SessionRow
              key={s.id}
              session={s}
              active={s.id === activeId}
              onSelect={() => onSelect(s.id)}
              onRename={(title) => onRename(s.id, title)}
              onDelete={() => onDelete(s.id)}
            />
          ))
        )}
      </div>
    </div>
  );
}

function SessionRow({
  session,
  active,
  onSelect,
  onRename,
  onDelete,
}: {
  session: SessionMeta;
  active: boolean;
  onSelect: () => void;
  onRename: (title: string) => void;
  onDelete: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(session.title);
  const [confirming, setConfirming] = useState(false);
  const [hover, setHover] = useState(false);
  const confirmTimer = useRef<number | undefined>(undefined);

  useEffect(() => () => window.clearTimeout(confirmTimer.current), []);

  const commit = () => {
    setEditing(false);
    const title = draft.trim();
    if (title && title !== session.title) onRename(title);
    else setDraft(session.title);
  };

  return (
    <div
      onClick={onSelect}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 6,
        padding: "7px 8px",
        borderRadius: 8,
        cursor: "pointer",
        background: active ? "rgba(56,189,248,0.12)" : hover ? "#131926" : "transparent",
      }}
    >
      {editing ? (
        <input
          autoFocus
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onClick={(e) => e.stopPropagation()}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
            if (e.key === "Escape") {
              setDraft(session.title);
              setEditing(false);
            }
          }}
          style={{ ...inputStyle, marginTop: 0, padding: "3px 6px", fontSize: 13 }}
        />
      ) : (
        <span
          onDoubleClick={(e) => {
            e.stopPropagation();
            setDraft(session.title);
            setEditing(true);
          }}
          title={session.title}
          style={{
            flex: 1,
            minWidth: 0,
            fontSize: 13,
            whiteSpace: "nowrap",
            overflow: "hidden",
            textOverflow: "ellipsis",
            color: active ? "#e2e8f0" : "#cbd5e1",
          }}
        >
          {session.title}
        </span>
      )}
      {(hover || confirming) && !editing && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            if (confirming) {
              window.clearTimeout(confirmTimer.current);
              setConfirming(false);
              onDelete();
            } else {
              setConfirming(true);
              confirmTimer.current = window.setTimeout(() => setConfirming(false), 3000);
            }
          }}
          title={confirming ? "Click again to delete" : "Delete chat"}
          style={{
            border: "none",
            background: "transparent",
            cursor: "pointer",
            fontSize: 12,
            color: confirming ? "#f87171" : "#64748b",
            padding: "0 2px",
            flexShrink: 0,
          }}
        >
          {confirming ? "sure?" : "✕"}
        </button>
      )}
    </div>
  );
}
