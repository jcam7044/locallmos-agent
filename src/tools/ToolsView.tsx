import { useCallback, useEffect, useMemo, useState } from "react";
import {
  mcpOverview,
  mcpInstallCatalogEntry,
  mcpUpdateServer,
  mcpDeleteServer,
  mcpStartServer,
  mcpStopServer,
  mcpServerLogs,
  mcpSetSecret,
} from "../api";
import type { McpCatalogEntry, McpOverview, McpServerView, McpStatus } from "../types";
import { card, label, buttonStyle, secondaryButton, inputStyle } from "../styles";

const MAX_TOTAL_MCP_TOOLS = 48;

const STATUS_COLOR: Record<McpStatus, string> = {
  running: "#34d399",
  starting: "#fbbf24",
  failed: "#f87171",
  stopped: "#64748b",
};

const SLUG = /^[a-z0-9_]{1,24}$/;

export function ToolsView() {
  const [overview, setOverview] = useState<McpOverview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [installing, setInstalling] = useState<McpCatalogEntry | null>(null);
  const [logs, setLogs] = useState<{ id: string; text: string } | null>(null);

  const load = useCallback(async () => {
    try {
      setOverview(await mcpOverview());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const run = useCallback(async (fn: () => Promise<McpOverview>) => {
    setBusy(true);
    setError(null);
    try {
      setOverview(await fn());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const activeTools = useMemo(
    () => (overview?.servers ?? []).reduce((n, s) => n + (s.status === "running" ? s.tools.length : 0), 0),
    [overview],
  );

  const installedIds = useMemo(
    () => new Set((overview?.servers ?? []).map((s) => s.catalogId).filter(Boolean)),
    [overview],
  );

  if (!overview) {
    return <p style={{ ...label, marginTop: 16 }}>{error ?? "Loading MCP servers…"}</p>;
  }

  const { runtimes, catalog, servers, truncated } = overview;

  return (
    <div style={{ marginTop: 16, display: "flex", flexDirection: "column", gap: 16 }}>
      <div>
        <h2 style={{ margin: 0, fontSize: 18 }}>MCP Servers</h2>
        <p style={{ ...label, marginTop: 4 }}>
          Connect Model Context Protocol servers to give local models extra tools. Enable them
          per coding session from the Code tab.
        </p>
      </div>

      {(!runtimes.node || !runtimes.uv) && (
        <div style={{ ...card, borderColor: "#78350f", background: "#1c1408" }}>
          <div style={{ fontSize: 13, color: "#fbbf24", fontWeight: 600 }}>Missing runtimes</div>
          <p style={{ ...label, marginTop: 6 }}>
            {!runtimes.node && "Node.js (npx) was not found — npx-based servers can't launch. "}
            {!runtimes.uv && "uv (uvx) was not found — uvx-based servers can't launch. "}
            Install the missing runtime, then reopen this tab.
          </p>
        </div>
      )}

      {/* Active-tool budget */}
      <div style={{ ...label }}>
        {activeTools} / {MAX_TOTAL_MCP_TOOLS} MCP tools active across running servers.
        {truncated > 0 && (
          <span style={{ color: "#fbbf24" }}>
            {" "}
            {truncated} tool{truncated === 1 ? "" : "s"} dropped by the cap — disable some tools or
            servers to fit within the model's context.
          </span>
        )}
      </div>

      {/* Configured servers */}
      {servers.length > 0 && (
        <section style={{ display: "flex", flexDirection: "column", gap: 10 }}>
          <div style={{ fontSize: 14, fontWeight: 600 }}>Configured</div>
          {servers.map((s) => (
            <ServerCard
              key={s.id}
              server={s}
              busy={busy}
              onStart={() => run(() => mcpStartServer(s.id))}
              onStop={() => run(() => mcpStopServer(s.id))}
              onDelete={() => run(() => mcpDeleteServer(s.id))}
              onToggleEnabled={(enabled) => run(() => mcpUpdateServer({ id: s.id, enabled }))}
              onToggleTrust={(trusted) =>
                run(() => mcpUpdateServer({ id: s.id, trust: trusted ? "trusted" : "untrusted" }))
              }
              onToggleTool={(tool, enabled) => {
                const disabled = new Set(s.disabledTools);
                if (enabled) disabled.delete(tool);
                else disabled.add(tool);
                return run(() => mcpUpdateServer({ id: s.id, disabledTools: [...disabled] }));
              }}
              onLogs={async () => {
                try {
                  setLogs({ id: s.id, text: await mcpServerLogs(s.id) });
                } catch (e) {
                  setError(String(e));
                }
              }}
              onSetSecret={async (key, value) => {
                setBusy(true);
                try {
                  await mcpSetSecret(s.id, key, value);
                  await load();
                } catch (e) {
                  setError(String(e));
                } finally {
                  setBusy(false);
                }
              }}
            />
          ))}
        </section>
      )}

      {/* Catalog */}
      <section style={{ display: "flex", flexDirection: "column", gap: 10 }}>
        <div style={{ fontSize: 14, fontWeight: 600 }}>Add from catalog</div>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))",
            gap: 10,
          }}
        >
          {catalog.map((entry) => {
            const runtimeMissing =
              (entry.runtime === "npx" && !runtimes.node) || (entry.runtime === "uvx" && !runtimes.uv);
            return (
              <div key={entry.id} style={{ ...card, display: "flex", flexDirection: "column", gap: 6 }}>
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                  <span style={{ fontWeight: 600, fontSize: 13 }}>{entry.label}</span>
                  <span style={{ ...label, fontFamily: "monospace" }}>{entry.runtime}</span>
                </div>
                <p style={{ ...label, lineHeight: 1.4 }}>{entry.description}</p>
                {entry.caveat && (
                  <p style={{ fontSize: 11, color: "#fbbf24", lineHeight: 1.4 }}>⚠ {entry.caveat}</p>
                )}
                <button
                  style={{ ...secondaryButton, marginTop: "auto", opacity: runtimeMissing ? 0.5 : 1 }}
                  disabled={runtimeMissing || installedIds.has(entry.id)}
                  onClick={() => setInstalling(entry)}
                >
                  {installedIds.has(entry.id) ? "Installed" : "Install…"}
                </button>
              </div>
            );
          })}
        </div>
      </section>

      {error && <p style={{ color: "#f87171", fontSize: 12 }}>{error}</p>}

      {installing && (
        <InstallDialog
          entry={installing}
          existingIds={new Set(servers.map((s) => s.id))}
          onClose={() => setInstalling(null)}
          onInstalled={(next) => {
            setOverview(next);
            setInstalling(null);
          }}
          onError={setError}
        />
      )}

      {logs && <LogsDialog id={logs.id} text={logs.text} onClose={() => setLogs(null)} />}
    </div>
  );
}

function StatusDot({ status }: { status: McpStatus }) {
  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 6, fontSize: 12 }}>
      <span
        style={{
          width: 8,
          height: 8,
          borderRadius: "50%",
          background: STATUS_COLOR[status],
          display: "inline-block",
        }}
      />
      {status}
    </span>
  );
}

function ServerCard({
  server,
  busy,
  onStart,
  onStop,
  onDelete,
  onToggleEnabled,
  onToggleTrust,
  onToggleTool,
  onLogs,
  onSetSecret,
}: {
  server: McpServerView;
  busy: boolean;
  onStart: () => void;
  onStop: () => void;
  onDelete: () => void;
  onToggleEnabled: (enabled: boolean) => void;
  onToggleTrust: (trusted: boolean) => void;
  onToggleTool: (tool: string, enabled: boolean) => void;
  onLogs: () => void;
  onSetSecret: (key: string, value: string) => void;
}) {
  const [showTools, setShowTools] = useState(false);
  const running = server.status === "running";

  return (
    <div style={{ ...card, display: "flex", flexDirection: "column", gap: 8 }}>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 8 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <label style={{ display: "inline-flex", alignItems: "center", gap: 6, cursor: "pointer" }}>
            <input
              type="checkbox"
              checked={server.enabled}
              disabled={busy}
              onChange={(e) => onToggleEnabled(e.target.checked)}
            />
            <span style={{ fontWeight: 600 }}>{server.label}</span>
          </label>
          <StatusDot status={server.status} />
        </div>
        <div style={{ display: "flex", gap: 6 }}>
          {running ? (
            <button style={secondaryButton} disabled={busy} onClick={onStop}>
              Stop
            </button>
          ) : (
            <button style={secondaryButton} disabled={busy || !server.enabled} onClick={onStart}>
              Start
            </button>
          )}
          <button style={secondaryButton} disabled={busy} onClick={onLogs}>
            Logs
          </button>
          <button style={{ ...secondaryButton, color: "#f87171" }} disabled={busy} onClick={onDelete}>
            Delete
          </button>
        </div>
      </div>

      {server.transport.kind === "stdio" && (
        <div style={{ ...label, fontFamily: "monospace", fontSize: 11, wordBreak: "break-all" }}>
          {server.transport.command} {server.transport.args.join(" ")}
        </div>
      )}

      {server.lastError && (
        <div style={{ fontSize: 11, color: "#f87171", whiteSpace: "pre-wrap" }}>{server.lastError}</div>
      )}

      <div style={{ display: "flex", alignItems: "center", gap: 14, flexWrap: "wrap" }}>
        <label style={{ display: "inline-flex", alignItems: "center", gap: 6, fontSize: 12, cursor: "pointer" }}>
          <input
            type="checkbox"
            checked={server.trust === "trusted"}
            disabled={busy}
            onChange={(e) => onToggleTrust(e.target.checked)}
          />
          Trusted (believe read-only hints — skips approval for read-only tools)
        </label>
        {running && (
          <button style={{ ...secondaryButton, padding: "2px 8px" }} onClick={() => setShowTools((v) => !v)}>
            {showTools ? "Hide" : "Show"} {server.tools.length} tool{server.tools.length === 1 ? "" : "s"}
          </button>
        )}
      </div>

      {server.secretKeys.length > 0 && (
        <SecretsEditor keys={server.secretKeys} busy={busy} onSet={onSetSecret} />
      )}

      {running && showTools && (
        <div style={{ display: "flex", flexDirection: "column", gap: 4, marginTop: 4 }}>
          {server.tools.length === 0 && <span style={label}>This server exposed no tools.</span>}
          {server.tools.map((t) => (
            <label
              key={t.qualified}
              style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12, cursor: "pointer" }}
              title={t.description}
            >
              <input
                type="checkbox"
                checked={t.enabled}
                disabled={busy}
                onChange={(e) => onToggleTool(t.name, e.target.checked)}
              />
              <span style={{ fontFamily: "monospace" }}>{t.name}</span>
              {t.mutating ? (
                <span style={{ fontSize: 10, color: "#fbbf24" }}>mutating · needs approval</span>
              ) : (
                <span style={{ fontSize: 10, color: "#34d399" }}>read-only</span>
              )}
            </label>
          ))}
        </div>
      )}
    </div>
  );
}

function SecretsEditor({
  keys,
  busy,
  onSet,
}: {
  keys: string[];
  busy: boolean;
  onSet: (key: string, value: string) => void;
}) {
  const [key, setKey] = useState(keys[0] ?? "");
  const [value, setValue] = useState("");
  return (
    <div style={{ display: "flex", gap: 6, alignItems: "center", flexWrap: "wrap" }}>
      <span style={label}>Secrets set: {keys.join(", ")}</span>
      <select
        value={key}
        onChange={(e) => setKey(e.target.value)}
        style={{ ...inputStyle, width: "auto", marginTop: 0, padding: "4px 8px" }}
      >
        {keys.map((k) => (
          <option key={k} value={k}>
            {k}
          </option>
        ))}
      </select>
      <input
        type="password"
        placeholder="new value"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        style={{ ...inputStyle, width: 160, marginTop: 0, padding: "4px 8px" }}
      />
      <button
        style={{ ...secondaryButton, padding: "4px 10px" }}
        disabled={busy || !value}
        onClick={() => {
          onSet(key, value);
          setValue("");
        }}
      >
        Update
      </button>
    </div>
  );
}

function InstallDialog({
  entry,
  existingIds,
  onClose,
  onInstalled,
  onError,
}: {
  entry: McpCatalogEntry;
  existingIds: Set<string>;
  onClose: () => void;
  onInstalled: (next: McpOverview) => void;
  onError: (message: string) => void;
}) {
  const [serverId, setServerId] = useState(entry.id);
  const [inputs, setInputs] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  const idValid = SLUG.test(serverId) && !existingIds.has(serverId);
  const missing = entry.inputs.some((i) => i.required && !(inputs[i.key] ?? "").trim());

  const submit = async () => {
    setBusy(true);
    try {
      onInstalled(await mcpInstallCatalogEntry(entry.id, serverId, inputs));
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal onClose={onClose}>
      <h3 style={{ margin: "0 0 4px" }}>Install {entry.label}</h3>
      <p style={{ ...label, lineHeight: 1.4 }}>{entry.description}</p>
      {entry.caveat && <p style={{ fontSize: 12, color: "#fbbf24", lineHeight: 1.4 }}>⚠ {entry.caveat}</p>}

      <div style={{ marginTop: 10 }}>
        <div style={label}>This will run (downloads and executes the pinned package):</div>
        <div
          style={{
            ...inputStyle,
            fontFamily: "monospace",
            fontSize: 11,
            whiteSpace: "pre-wrap",
            wordBreak: "break-all",
          }}
        >
          {entry.command}
        </div>
      </div>

      <label style={{ display: "block", marginTop: 10 }}>
        <span style={label}>Server id (used in tool names as mcp__&lt;id&gt;__…)</span>
        <input
          value={serverId}
          onChange={(e) => setServerId(e.target.value)}
          style={{ ...inputStyle, borderColor: idValid ? "#1f2937" : "#f87171" }}
        />
        {!idValid && (
          <span style={{ fontSize: 11, color: "#f87171" }}>
            {existingIds.has(serverId)
              ? "A server with this id already exists."
              : "Use 1–24 lowercase letters, digits, or underscores."}
          </span>
        )}
      </label>

      {entry.inputs.map((spec) => (
        <label key={spec.key} style={{ display: "block", marginTop: 8 }}>
          <span style={label}>
            {spec.label}
            {spec.required ? " *" : ""}
            {spec.secret ? " (stored securely)" : ""}
          </span>
          <input
            type={spec.secret ? "password" : "text"}
            placeholder={spec.placeholder}
            value={inputs[spec.key] ?? ""}
            onChange={(e) => setInputs((prev) => ({ ...prev, [spec.key]: e.target.value }))}
            style={inputStyle}
          />
        </label>
      ))}

      <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 14 }}>
        <button style={secondaryButton} onClick={onClose} disabled={busy}>
          Cancel
        </button>
        <button style={buttonStyle} onClick={submit} disabled={busy || !idValid || missing}>
          {busy ? "Installing…" : "Install & start"}
        </button>
      </div>
    </Modal>
  );
}

function LogsDialog({ id, text, onClose }: { id: string; text: string; onClose: () => void }) {
  return (
    <Modal onClose={onClose}>
      <h3 style={{ margin: "0 0 8px" }}>Logs — {id}</h3>
      <pre
        style={{
          background: "#0b0f17",
          border: "1px solid #1f2937",
          borderRadius: 8,
          padding: 10,
          fontSize: 11,
          maxHeight: 360,
          overflow: "auto",
          whiteSpace: "pre-wrap",
        }}
      >
        {text}
      </pre>
      <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 10 }}>
        <button style={secondaryButton} onClick={onClose}>
          Close
        </button>
      </div>
    </Modal>
  );
}

function Modal({ children, onClose }: { children: React.ReactNode; onClose: () => void }) {
  return (
    <div
      onClick={onClose}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.6)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 50,
        padding: 20,
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          ...card,
          width: "min(560px, 100%)",
          maxHeight: "85vh",
          overflow: "auto",
        }}
      >
        {children}
      </div>
    </div>
  );
}
