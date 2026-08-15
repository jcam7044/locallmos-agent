import { REASONING_EFFORTS, type ReasoningEffort } from "../types";

/** Human labels for each level; `none` reads as "Off" in the picker. */
const EFFORT_LABELS: Record<ReasoningEffort, string> = {
  none: "Off",
  minimal: "Minimal",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "X-High",
  max: "Max",
};

/**
 * Pill-styled reasoning-effort picker shared by the Chat and Code composers.
 * Mirrors the `TogglePill` aesthetic — highlighted whenever reasoning is on
 * (any level other than "Off"). Disabled when the model can't reason.
 */
export function EffortSelector({
  value,
  disabled,
  onChange,
  title,
}: {
  value: ReasoningEffort;
  disabled?: boolean;
  onChange: (effort: ReasoningEffort) => void;
  title?: string;
}) {
  const on = value !== "none";
  return (
    <label
      title={
        title ??
        (disabled
          ? "This model doesn't support thinking"
          : "How hard the model should reason before answering")
      }
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 4,
        padding: "3px 8px",
        borderRadius: 999,
        fontSize: 12,
        cursor: disabled ? "default" : "pointer",
        border: `1px solid ${on && !disabled ? "rgba(56,189,248,0.6)" : "#1f2937"}`,
        background: on && !disabled ? "rgba(56,189,248,0.15)" : "transparent",
        color: disabled ? "#475569" : on ? "#38bdf8" : "#94a3b8",
      }}
    >
      <span aria-hidden>💭</span>
      <select
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value as ReasoningEffort)}
        style={{
          appearance: "none",
          border: "none",
          background: "transparent",
          color: "inherit",
          font: "inherit",
          cursor: disabled ? "default" : "pointer",
          outline: "none",
          paddingRight: 2,
        }}
      >
        {REASONING_EFFORTS.map((effort) => (
          <option key={effort} value={effort} style={{ color: "#0f172a" }}>
            {`Think: ${EFFORT_LABELS[effort]}`}
          </option>
        ))}
      </select>
    </label>
  );
}
