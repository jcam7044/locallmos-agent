import { useCallback, useEffect, useState } from "react";
import { codingDeleteAgent, codingListAgents, codingSaveAgent } from "../api";
import type { AgentView } from "../types";
import { C } from "./tokens";

/** The read-only tools a sub-agent may use (mirrors agents.rs READONLY_TOOLS). */
const READONLY_TOOLS = ["read_file", "list_dir", "search", "git"] as const;

type Draft = {
  original: string | null; // the name being edited, or null for a new agent
  scope: "project" | "global";
  name: string;
  description: string;
  tools: string[];
  prompt: string;
};

const BLANK: Draft = {
  original: null,
  scope: "project",
  name: "",
  description: "",
  tools: [...READONLY_TOOLS],
  prompt: "",
};

/**
 * The Code tab's Agents manager: list the built-in + project + global sub-agents
 * for the active session's workspace, and create / edit / delete the file-based
 * ones. Renders as a modal over the transcript.
 */
export function AgentsPanel({
  workspaceRoot,
  onClose,
  onError,
}: {
  workspaceRoot: string;
  onClose: () => void;
  onError: (message: string) => void;
}) {
  const [agents, setAgents] = useState<AgentView[]>([]);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    codingListAgents(workspaceRoot)
      .then(setAgents)
      .catch((e) => onError(String(e)));
  }, [workspaceRoot, onError]);

  useEffect(refresh, [refresh]);

  async function save() {
    if (!draft) return;
    if (!draft.name.trim() || !draft.prompt.trim()) {
      onError("An agent needs a name and a prompt.");
      return;
    }
    setBusy(true);
    try {
      await codingSaveAgent(
        workspaceRoot,
        draft.scope,
        draft.name.trim(),
        draft.description.trim(),
        draft.prompt,
        draft.tools,
      );
      setDraft(null);
      refresh();
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function remove(a: AgentView) {
    if (a.scope === "builtin") return;
    if (!confirm(`Delete the "${a.name}" agent?`)) return;
    try {
      await codingDeleteAgent(workspaceRoot, a.scope as "project" | "global", a.name);
      refresh();
    } catch (e) {
      onError(String(e));
    }
  }

  return (
    <div style={overlay} onClick={onClose}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 12 }}>
          <div style={{ fontSize: 15, color: "#e2e8f0" }}>Agents</div>
          <button onClick={onClose} style={btn}>Close</button>
        </div>

        {draft ? (
          <AgentForm draft={draft} setDraft={setDraft} onSave={save} onCancel={() => setDraft(null)} busy={busy} />
        ) : (
          <>
            <p style={{ fontSize: 12, color: C.muted, margin: "0 0 10px" }}>
              Sub-agents run read-only in their own context and are dispatched with the run_agent tool.
              Project agents live in <code>.agents/</code>; global agents apply to every project.
            </p>
            <div style={{ display: "flex", flexDirection: "column", gap: 8, overflowY: "auto", flex: 1 }}>
              {agents.map((a) => (
                <div key={`${a.scope}:${a.name}`} style={row}>
                  <div style={{ minWidth: 0, flex: 1 }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <span style={{ fontSize: 13, color: "#e2e8f0" }}>{a.name}</span>
                      <span style={badge(a.scope)}>{a.scope}</span>
                    </div>
                    <div style={{ fontSize: 12, color: C.muted, marginTop: 2 }}>{a.description}</div>
                    <div style={{ fontSize: 11, color: C.muted, marginTop: 2 }}>tools: {a.tools.join(", ")}</div>
                  </div>
                  {a.editable && (
                    <div style={{ display: "flex", gap: 6, flexShrink: 0 }}>
                      <button
                        style={btn}
                        onClick={() =>
                          setDraft({
                            original: a.name,
                            scope: a.scope as "project" | "global",
                            name: a.name,
                            description: a.description,
                            tools: a.tools.length ? a.tools : [...READONLY_TOOLS],
                            prompt: a.prompt,
                          })
                        }
                      >
                        Edit
                      </button>
                      <button style={{ ...btn, color: "#f87171" }} onClick={() => void remove(a)}>Delete</button>
                    </div>
                  )}
                </div>
              ))}
            </div>
            <button style={{ ...primaryBtn, marginTop: 12 }} onClick={() => setDraft({ ...BLANK })}>
              + New agent
            </button>
          </>
        )}
      </div>
    </div>
  );
}

function AgentForm({
  draft,
  setDraft,
  onSave,
  onCancel,
  busy,
}: {
  draft: Draft;
  setDraft: (d: Draft) => void;
  onSave: () => void;
  onCancel: () => void;
  busy: boolean;
}) {
  const editing = draft.original !== null;
  const toggleTool = (tool: string) =>
    setDraft({
      ...draft,
      tools: draft.tools.includes(tool)
        ? draft.tools.filter((t) => t !== tool)
        : [...draft.tools, tool],
    });

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10, overflowY: "auto", flex: 1 }}>
      <div style={{ display: "flex", gap: 10 }}>
        <label style={{ flex: 1 }}>
          <div style={fieldLabel}>Name</div>
          <input
            style={input}
            value={draft.name}
            disabled={editing}
            placeholder="reviewer"
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
          />
        </label>
        <label>
          <div style={fieldLabel}>Scope</div>
          <select
            style={{ ...input, cursor: editing ? "not-allowed" : "pointer" }}
            value={draft.scope}
            disabled={editing}
            onChange={(e) => setDraft({ ...draft, scope: e.target.value as "project" | "global" })}
          >
            <option value="project">project (.agents/)</option>
            <option value="global">global (all projects)</option>
          </select>
        </label>
      </div>
      <label>
        <div style={fieldLabel}>Description</div>
        <input
          style={input}
          value={draft.description}
          placeholder="Reviews code for bugs and reports concrete findings."
          onChange={(e) => setDraft({ ...draft, description: e.target.value })}
        />
      </label>
      <div>
        <div style={fieldLabel}>Tools (read-only)</div>
        <div style={{ display: "flex", gap: 12, flexWrap: "wrap" }}>
          {READONLY_TOOLS.map((tool) => (
            <label key={tool} style={{ fontSize: 12, color: "#e2e8f0", display: "flex", gap: 4, alignItems: "center" }}>
              <input type="checkbox" checked={draft.tools.includes(tool)} onChange={() => toggleTool(tool)} />
              {tool}
            </label>
          ))}
        </div>
      </div>
      <label style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0 }}>
        <div style={fieldLabel}>System prompt</div>
        <textarea
          style={{ ...input, minHeight: 160, resize: "vertical", fontFamily: "inherit" }}
          value={draft.prompt}
          placeholder="You are a meticulous code reviewer. Read the file you're asked about, then report concrete bugs with line numbers and a one-line fix."
          onChange={(e) => setDraft({ ...draft, prompt: e.target.value })}
        />
      </label>
      <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
        <button style={btn} onClick={onCancel} disabled={busy}>Cancel</button>
        <button style={primaryBtn} onClick={onSave} disabled={busy}>{editing ? "Save" : "Create"}</button>
      </div>
    </div>
  );
}

// --- styles ---------------------------------------------------------------
const overlay: React.CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(2,6,23,0.6)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 50,
};
const card: React.CSSProperties = {
  width: "min(680px, 92vw)",
  maxHeight: "82vh",
  display: "flex",
  flexDirection: "column",
  background: "#0f172a",
  border: C.border,
  borderRadius: 12,
  padding: 16,
};
const row: React.CSSProperties = {
  display: "flex",
  alignItems: "flex-start",
  gap: 10,
  border: C.border,
  borderRadius: 8,
  padding: 10,
};
const btn: React.CSSProperties = {
  background: "rgba(148,163,184,0.1)",
  border: C.border,
  borderRadius: 6,
  color: "#e2e8f0",
  fontSize: 12,
  padding: "5px 10px",
  cursor: "pointer",
};
const primaryBtn: React.CSSProperties = {
  ...btn,
  background: "rgba(56,189,248,0.15)",
  border: "1px solid rgba(56,189,248,0.5)",
  color: C.accent,
};
const input: React.CSSProperties = {
  width: "100%",
  boxSizing: "border-box",
  background: "rgba(15,23,42,0.6)",
  border: C.border,
  borderRadius: 8,
  color: "#e2e8f0",
  padding: "8px 10px",
  fontSize: 13,
};
const fieldLabel: React.CSSProperties = { fontSize: 12, color: C.muted, marginBottom: 4 };

function badge(scope: string): React.CSSProperties {
  const color = scope === "builtin" ? "#94a3b8" : scope === "global" ? "#f0abfc" : "#38bdf8";
  return {
    fontSize: 10,
    color,
    border: `1px solid ${color}55`,
    borderRadius: 4,
    padding: "1px 6px",
    textTransform: "uppercase",
    letterSpacing: 0.4,
  };
}
