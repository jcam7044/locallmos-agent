import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { CodingEvent, CodingStreamEvent } from "../types";

export type CodingTrace =
  | { kind: "tool"; name: string; summary?: string }
  | { kind: "file_edit"; path: string; diff: string; summary: string; invocationId?: string }
  | { kind: "command"; command: string; output: string; exitCode?: number | null; invocationId?: string };

export type PendingApproval = { invocationId: string; name: string; preview: string };

export type CodingLive = {
  messageId: string | null;
  text: string;
  thinking: string;
  status: "idle" | "loading" | "streaming" | "done" | "error";
  error?: string;
  loadingModel?: string;
  trace: CodingTrace[];
  approvals: PendingApproval[];
};

const EMPTY: CodingLive = {
  messageId: null,
  text: "",
  thinking: "",
  status: "idle",
  trace: [],
  approvals: [],
};

/**
 * Accumulate live coding-stream state for the active session from the agent's
 * Tauri "local-coding" events. Resets when the session changes or a new
 * assistant turn begins.
 */
export function useCodingStream(sessionId: string | null) {
  const [live, setLive] = useState<CodingLive>(EMPTY);
  const sessionRef = useRef(sessionId);
  sessionRef.current = sessionId;

  useEffect(() => {
    setLive(EMPTY);
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen<CodingEvent>("local-coding", ({ payload }) => {
      if (payload.sessionId !== sessionRef.current) return;
      setLive((s) => reduce(s, payload.messageId, payload.event));
    }).then((u) => {
      // `listen` is async, so the effect can tear down before it resolves —
      // StrictMode does exactly that on every mount. Detaching here is the only
      // chance; otherwise the handler leaks, a second one registers, and every
      // token is appended twice ("WorkingWorking!!").
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [sessionId]);

  // Stable so effects can depend on it without re-running each render.
  const reset = useCallback(() => setLive(EMPTY), []);

  return { live, reset };
}

function reduce(s: CodingLive, messageId: string, ev: CodingStreamEvent): CodingLive {
  if (ev.type === "context_updated" || ev.type.startsWith("compaction_")) return s;
  // A new assistant message id means a fresh turn — start accumulation over.
  const base = s.messageId && s.messageId !== messageId ? EMPTY : s;
  const cur: CodingLive = { ...base, messageId };
  switch (ev.type) {
    case "loading":
      return { ...cur, status: "loading", loadingModel: ev.model };
    case "token":
      return { ...cur, status: "streaming", text: cur.text + ev.delta };
    case "thinking":
      return { ...cur, status: "streaming", thinking: cur.thinking + ev.delta };
    case "tool":
      return { ...cur, status: "streaming", trace: [...cur.trace, { kind: "tool", name: ev.name }] };
    case "tool_result": {
      const trace = [...cur.trace];
      for (let i = trace.length - 1; i >= 0; i--) {
        const t = trace[i];
        if (t && t.kind === "tool" && t.name === ev.name && t.summary === undefined) {
          trace[i] = { kind: "tool", name: t.name, summary: ev.summary };
          break;
        }
      }
      return { ...cur, trace };
    }
    case "file_edit":
      return {
        ...cur,
        trace: [
          ...cur.trace,
          { kind: "file_edit", path: ev.path, diff: ev.diff, summary: ev.summary, invocationId: ev.invocationId },
        ],
      };
    case "command":
      return {
        ...cur,
        trace: [
          ...cur.trace,
          { kind: "command", command: ev.command, output: ev.chunk, exitCode: ev.exitCode, invocationId: ev.invocationId },
        ],
      };
    case "approval_needed":
      return {
        ...cur,
        approvals: [...cur.approvals, { invocationId: ev.invocationId, name: ev.name, preview: ev.preview }],
      };
    case "approval_resolved":
      return { ...cur, approvals: cur.approvals.filter((a) => a.invocationId !== ev.invocationId) };
    case "done":
      return { ...cur, status: "done" };
    case "error":
      return { ...cur, status: "error", error: ev.message };
    default:
      return cur;
  }
}
