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
  mcpSetToolLimit,
} from "../api";
import type { McpCatalogEntry, McpOverview, McpServerView, McpStatus } from "../types";
import { card, label, buttonStyle, secondaryButton, inputStyle } from "../styles";

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
  const [details, setDetails] = useState<McpCatalogEntry | null>(null);
  const [logs, setLogs] = useState<{ id: string; text: string } | null>(null);
  const [customLimit, setCustomLimit] = useState(48);

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

  useEffect(() => {
    if (overview) setCustomLimit(overview.toolLimit);
  }, [overview?.toolLimit]);

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
    () =>
      (overview?.servers ?? []).reduce(
        (n, s) => n + (s.status === "running" ? s.tools.filter((tool) => tool.available).length : 0),
        0,
      ),
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
  const presets = [
    { label: "Conservative", limit: 24, context: "8k–16k context" },
    { label: "Balanced", limit: 48, context: "32k–64k context" },
    { label: "Expanded", limit: 96, context: "64k–128k context" },
    { label: "Large context", limit: 128, context: "128k+ context" },
  ];

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

      <div style={{ ...card, display: "flex", flexDirection: "column", gap: 10 }}>
        <div>
          <div style={{ fontSize: 14, fontWeight: 600 }}>MCP tool limit</div>
          <p style={{ ...label, lineHeight: 1.45, margin: "4px 0 0" }}>
            Limits how many tool definitions are sent with each model request. Larger catalogs use
            more context and may make tool selection harder for smaller models.
          </p>
        </div>
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          {presets.map((preset) => (
            <button
              key={preset.limit}
              disabled={busy}
              title={`Suggested for ${preset.context}`}
              onClick={() => run(() => mcpSetToolLimit(preset.limit))}
              style={{
                ...secondaryButton,
                borderColor: overview.toolLimit === preset.limit ? "#38bdf8" : "#1f2937",
                color: overview.toolLimit === preset.limit ? "#38bdf8" : "#e2e8f0",
              }}
            >
              {preset.label} · {preset.limit}
            </button>
          ))}
          <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
            <input
              type="number"
              min={1}
              max={256}
              value={customLimit}
              aria-label="Custom MCP tool limit"
              onChange={(event) => setCustomLimit(Number(event.target.value))}
              style={{ ...inputStyle, width: 76, marginTop: 0, padding: "6px 8px" }}
            />
            <button
              style={secondaryButton}
              disabled={busy || !Number.isInteger(customLimit) || customLimit < 1 || customLimit > 256}
              onClick={() => run(() => mcpSetToolLimit(customLimit))}
            >
              Apply custom
            </button>
          </div>
        </div>
        <div style={{ ...label }}>
          {activeTools} of {overview.availableTools} enabled tools available · approximately{" "}
          {formatTokenCount(overview.activeSchemaTokens)} context tokens
          {overview.availableTools > activeTools &&
            ` (${formatTokenCount(overview.availableSchemaTokens)} if all enabled tools were included)`}
        </div>
        <div style={{ ...label, fontSize: 11 }}>
          Suggestions are starting points: tool schema sizes and model tool-selection quality vary.
        </div>
        {truncated > 0 && (
          <div style={{ color: "#fbbf24", fontSize: 12 }}>
            {truncated} tool{truncated === 1 ? " is" : "s are"} excluded by your {overview.toolLimit}-tool
            limit. Raise the limit or disable tools you do not need.
          </div>
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
            const installed = installedIds.has(entry.id);
            return (
              <div
                key={entry.id}
                role="button"
                tabIndex={0}
                aria-label={`View details for ${entry.label}`}
                onClick={() => setDetails(entry)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    setDetails(entry);
                  }
                }}
                style={{
                  ...card,
                  display: "flex",
                  flexDirection: "column",
                  gap: 6,
                  cursor: "pointer",
                }}
              >
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                  <span style={{ fontWeight: 600, fontSize: 13 }}>{entry.label}</span>
                  <span style={{ ...label, fontFamily: "monospace" }}>{entry.runtime}</span>
                </div>
                <p style={{ ...label, lineHeight: 1.4 }}>{entry.description}</p>
                {entry.caveat && (
                  <p style={{ fontSize: 11, color: "#fbbf24", lineHeight: 1.4 }}>⚠ {entry.caveat}</p>
                )}
                <div
                  aria-hidden="true"
                  style={{
                    ...secondaryButton,
                    marginTop: "auto",
                    textAlign: "center",
                    boxSizing: "border-box",
                  }}
                >
                  {installed ? "Installed · view details" : runtimeMissing ? "View requirements" : "View details"}
                </div>
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

      {details && (
        <CatalogDetailsDialog
          entry={details}
          installed={installedIds.has(details.id)}
          runtimeAvailable={
            (details.runtime === "npx" && runtimes.node) || (details.runtime === "uvx" && runtimes.uv)
          }
          onClose={() => setDetails(null)}
          onInstall={() => {
            setDetails(null);
            setInstalling(details);
          }}
        />
      )}

      {logs && <LogsDialog id={logs.id} text={logs.text} onClose={() => setLogs(null)} />}
    </div>
  );
}

export function CatalogDetailsDialog({
  entry,
  installed,
  runtimeAvailable,
  onClose,
  onInstall,
}: {
  entry: McpCatalogEntry;
  installed: boolean;
  runtimeAvailable: boolean;
  onClose: () => void;
  onInstall: () => void;
}) {
  return (
    <Modal onClose={onClose} ariaLabel={`${entry.label} details`}>
      <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 16 }}>
        <div>
          <h3 style={{ margin: 0 }}>{entry.label}</h3>
          <div style={{ ...label, marginTop: 4 }}>
            MCP server · connects with <span style={{ fontFamily: "monospace" }}>{entry.runtime}</span>
          </div>
        </div>
        {installed && (
          <span
            style={{
              border: "1px solid #166534",
              borderRadius: 999,
              color: "#34d399",
              fontSize: 11,
              padding: "3px 8px",
              whiteSpace: "nowrap",
            }}
          >
            Installed
          </span>
        )}
      </div>

      <p style={{ color: "#cbd5e1", fontSize: 13, lineHeight: 1.55, margin: "16px 0 0" }}>
        {entry.details}
      </p>

      <section style={{ marginTop: 18 }}>
        <h4 style={{ margin: "0 0 6px", fontSize: 13 }}>Typical connection</h4>
        <p style={{ ...label, lineHeight: 1.55, margin: 0 }}>{entry.connection}</p>
      </section>

      <section style={{ marginTop: 18 }}>
        <h4 style={{ margin: "0 0 8px", fontSize: 13 }}>Tools it provides</h4>
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {entry.tools.map((tool) => (
            <div
              key={tool.name}
              style={{
                border: "1px solid #1f2937",
                background: "#0b0f17",
                borderRadius: 8,
                padding: "8px 10px",
              }}
            >
              <div style={{ fontFamily: "monospace", fontSize: 12, color: "#e2e8f0" }}>{tool.name}</div>
              <div style={{ ...label, lineHeight: 1.4, marginTop: 3 }}>{tool.description}</div>
            </div>
          ))}
        </div>
        <p style={{ ...label, fontSize: 11, lineHeight: 1.4, margin: "8px 0 0" }}>
          The connected server reports the authoritative tool list; it may change with the pinned server version.
        </p>
      </section>

      {entry.caveat && (
        <p
          style={{
            border: "1px solid #78350f",
            background: "#1c1408",
            borderRadius: 8,
            color: "#fbbf24",
            fontSize: 12,
            lineHeight: 1.45,
            padding: 10,
            margin: "16px 0 0",
          }}
        >
          ⚠ {entry.caveat}
        </p>
      )}

      {!runtimeAvailable && (
        <p style={{ color: "#fbbf24", fontSize: 12, margin: "12px 0 0" }}>
          Install {entry.runtime === "npx" ? "Node.js (npx)" : "uv (uvx)"} before adding this server.
        </p>
      )}

      <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 18 }}>
        <button style={secondaryButton} onClick={onClose}>
          Close
        </button>
        {!installed && (
          <button style={buttonStyle} disabled={!runtimeAvailable} onClick={onInstall}>
            Install…
          </button>
        )}
      </div>
    </Modal>
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
              {t.enabled && !t.available && (
                <span style={{ fontSize: 10, color: "#fbbf24" }}>excluded by tool limit</span>
              )}
              <span style={{ ...label, fontSize: 10, marginLeft: "auto" }}>
                ~{formatTokenCount(t.schemaTokens)} tokens
              </span>
            </label>
          ))}
        </div>
      )}
    </div>
  );
}

function formatTokenCount(value: number) {
  return value >= 1_000 ? `${(value / 1_000).toFixed(value >= 10_000 ? 0 : 1)}k` : String(value);
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
    <Modal onClose={onClose} ariaLabel={`Install ${entry.label}`}>
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
    <Modal onClose={onClose} ariaLabel={`Logs for ${id}`}>
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

function Modal({
  children,
  onClose,
  ariaLabel,
}: {
  children: React.ReactNode;
  onClose: () => void;
  ariaLabel: string;
}) {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

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
        role="dialog"
        aria-modal="true"
        aria-label={ariaLabel}
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
