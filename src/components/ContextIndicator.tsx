export type ContextLevel = "normal" | "orange" | "red";

export function formatTokens(value: number) {
  if (value < 1_000) return String(value);
  const scaled = value / 1_000;
  return `${scaled >= 10 ? scaled.toFixed(0) : scaled.toFixed(1)}k`;
}

export function ContextRing({ percent, level }: { percent: number; level: ContextLevel }) {
  const color = level === "red" ? "#f87171" : level === "orange" ? "#fb923c" : "#3b82f6";
  const radius = 6;
  const circumference = 2 * Math.PI * radius;
  const offset = circumference * (1 - Math.min(100, Math.max(0, percent)) / 100);
  return (
    <span
      aria-hidden="true"
      style={{
        width: 16,
        height: 16,
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <svg width="16" height="16" viewBox="0 0 16 16" style={{ transform: "rotate(-90deg)" }}>
        <circle cx="8" cy="8" r={radius} fill="none" stroke="rgba(148,163,184,0.32)" strokeWidth="2.25" />
        <circle
          cx="8"
          cy="8"
          r={radius}
          fill="none"
          stroke={color}
          strokeWidth="2.25"
          strokeLinecap="round"
          strokeDasharray={circumference}
          strokeDashoffset={offset}
        />
      </svg>
    </span>
  );
}
