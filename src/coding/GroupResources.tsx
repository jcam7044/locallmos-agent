import { useCallback, useEffect, useState } from "react";
import { codingGroupRigMetrics } from "../api";
import type { GroupRigMetrics, GpuStat } from "../types";
import { C } from "./tokens";

const POLL_MS = 3000;
/** A rig whose last heartbeat is older than this reads as offline. */
const FRESH_MS = 90_000;
const CPU_COLOR = "#38bdf8"; // cyan
const GPU_COLOR = "#fb7185"; // pink

/**
 * Collapsible right sidebar for the Code tab: every rig in this rig's group with
 * its live CPU utilization and a per-GPU utilization meter. Data comes from the
 * `coding_group_rig_metrics` command (a SECURITY DEFINER RPC — a device JWT
 * can't read peer rigs' metrics directly). Polls every few seconds; the whole
 * rail hides when this rig is in no group.
 */
export function GroupResources({
  open,
  onToggle,
}: {
  open: boolean;
  onToggle: (open: boolean) => void;
}) {
  const [rigs, setRigs] = useState<GroupRigMetrics[] | null>(null);

  const refresh = useCallback(() => {
    codingGroupRigMetrics()
      .then(setRigs)
      // Not enrolled / offline / no group — just show nothing, don't nag.
      .catch(() => setRigs([]));
  }, []);

  useEffect(() => {
    // Always fetch once (even collapsed) so we know whether there's a group to
    // show and the rail reflects a fresh snapshot. Only the repeating poll is
    // paused while collapsed.
    refresh();
    if (!open) return;
    const t = setInterval(refresh, POLL_MS);
    return () => clearInterval(t);
  }, [refresh, open]);

  // Nothing to show until we know there's a group with rigs in it.
  if (!rigs || rigs.length === 0) return null;

  // This rig first, then the rest by name.
  const sorted = [...rigs].sort((a, b) => {
    if (a.isSelf !== b.isSelf) return a.isSelf ? -1 : 1;
    return (a.name ?? a.rigId).localeCompare(b.name ?? b.rigId);
  });

  if (!open) {
    return (
      <aside style={railStyle}>
        <button style={railToggle} title="Show group resources" onClick={() => onToggle(true)}>
          ‹
        </button>
        <div style={railLabel}>RESOURCES</div>
      </aside>
    );
  }

  return (
    <aside style={panelStyle}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 4 }}>
        <div style={{ fontSize: 12, color: "#e2e8f0", letterSpacing: 0.3 }}>Group resources</div>
        <button style={collapseBtn} title="Collapse" onClick={() => onToggle(false)}>
          ›
        </button>
      </div>
      <div style={{ overflowY: "auto", display: "flex", flexDirection: "column", gap: 10, paddingRight: 2 }}>
        {sorted.map((rig) => (
          <RigCard key={rig.rigId} rig={rig} isSelf={rig.isSelf} />
        ))}
      </div>
    </aside>
  );
}

function RigCard({ rig, isSelf }: { rig: GroupRigMetrics; isSelf: boolean }) {
  const online = isFresh(rig.lastSeen);
  const gpus = [...rig.gpus].sort((a, b) => a.index - b.index);
  return (
    <div style={{ ...cardStyle, opacity: online ? 1 : 0.5 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 6 }}>
        <span
          style={{
            width: 7,
            height: 7,
            borderRadius: 999,
            flexShrink: 0,
            background: online ? "#34d399" : "#64748b",
          }}
        />
        <span style={{ fontSize: 12, color: "#e2e8f0", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {rig.name ?? rig.rigId.slice(0, 8)}
        </span>
        {isSelf && <span style={selfBadge}>this rig</span>}
      </div>

      <Meter label="CPU" pct={online ? rig.cpuUtilizationPct : null} color={CPU_COLOR} />

      {gpus.map((g) => (
        <div key={g.id ?? g.index} style={{ marginTop: 6 }}>
          <Meter
            label={`GPU ${g.index}`}
            pct={online ? g.utilizationPct : null}
            color={GPU_COLOR}
          />
          {vramLine(g) && <div style={vramStyle}>{vramLine(g)}</div>}
        </div>
      ))}
    </div>
  );
}

/** A compact labeled percentage meter. `null` pct renders an em-dash (offline
 * / not reported) rather than an empty bar. */
function Meter({ label, pct, color }: { label: string; pct: number | null; color: string }) {
  const value = pct == null ? null : Math.max(0, Math.min(100, Math.round(pct)));
  return (
    <div>
      <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11, color: C.muted, marginBottom: 2 }}>
        <span>{label}</span>
        <span>{value == null ? "—" : `${value}%`}</span>
      </div>
      <div style={trackStyle}>
        <div style={{ height: "100%", borderRadius: "inherit", width: `${value ?? 0}%`, background: color, transition: "width 0.4s ease" }} />
      </div>
    </div>
  );
}

function isFresh(lastSeen: string | null): boolean {
  if (!lastSeen) return false;
  const t = Date.parse(lastSeen);
  return Number.isFinite(t) && Date.now() - t <= FRESH_MS;
}

function vramLine(g: GpuStat): string | null {
  if (g.memoryTotalBytes == null || g.memoryTotalBytes <= 0) return null;
  const used = g.memoryUsedBytes ?? 0;
  return `VRAM ${gib(used)} / ${gib(g.memoryTotalBytes)} GB`;
}

function gib(bytes: number): string {
  return (bytes / 1024 ** 3).toFixed(1);
}

// --- styles ---------------------------------------------------------------
const panelStyle: React.CSSProperties = {
  width: 230,
  flexShrink: 0,
  display: "flex",
  flexDirection: "column",
  gap: 6,
  borderLeft: C.border,
  paddingLeft: 12,
};
const railStyle: React.CSSProperties = {
  width: 28,
  flexShrink: 0,
  display: "flex",
  flexDirection: "column",
  alignItems: "center",
  gap: 8,
  borderLeft: C.border,
  paddingLeft: 4,
};
const railToggle: React.CSSProperties = {
  background: "rgba(148,163,184,0.1)",
  border: C.border,
  borderRadius: 6,
  color: "#e2e8f0",
  fontSize: 13,
  cursor: "pointer",
  padding: "2px 6px",
};
const railLabel: React.CSSProperties = {
  fontSize: 10,
  color: C.muted,
  letterSpacing: 1,
  writingMode: "vertical-rl",
  transform: "rotate(180deg)",
  userSelect: "none",
};
const collapseBtn: React.CSSProperties = {
  background: "transparent",
  border: "none",
  color: C.muted,
  fontSize: 14,
  cursor: "pointer",
  padding: "0 4px",
};
const cardStyle: React.CSSProperties = {
  border: C.border,
  borderRadius: 8,
  padding: 8,
  background: C.panel,
};
const trackStyle: React.CSSProperties = {
  height: 6,
  borderRadius: 999,
  overflow: "hidden",
  background: "rgba(148,163,184,0.18)",
};
const vramStyle: React.CSSProperties = {
  fontSize: 10,
  color: C.muted,
  marginTop: 2,
};
const selfBadge: React.CSSProperties = {
  fontSize: 9,
  color: C.accent,
  border: `1px solid ${C.accent}55`,
  borderRadius: 4,
  padding: "0 4px",
  textTransform: "uppercase",
  letterSpacing: 0.4,
  flexShrink: 0,
};
