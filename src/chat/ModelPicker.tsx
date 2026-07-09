import { inputStyle } from "../styles";
import type { LocalModel } from "../types";

const BADGES: Record<string, { icon: string; title: string }> = {
  vision: { icon: "👁", title: "Supports image input" },
  tools: { icon: "🔧", title: "Supports tool calling" },
  thinking: { icon: "💭", title: "Supports thinking" },
};

export function ModelPicker({
  models,
  value,
  onChange,
}: {
  models: LocalModel[];
  value: string;
  onChange: (model: string) => void;
}) {
  const selected = models.find((m) => m.name === value);
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        style={{ ...inputStyle, marginTop: 0, width: "auto", maxWidth: 300 }}
      >
        {models.length === 0 && <option value="">No models installed</option>}
        {value && !models.some((m) => m.name === value) && (
          <option value={value}>{value} (missing)</option>
        )}
        {models.map((m) => (
          <option key={m.name} value={m.name}>
            {m.name}
            {m.loaded ? " (loaded)" : ""}
          </option>
        ))}
      </select>
      {selected?.capabilities.map((c) =>
        BADGES[c] ? (
          <span
            key={c}
            title={BADGES[c].title}
            style={{
              fontSize: 11,
              padding: "2px 6px",
              borderRadius: 6,
              background: "rgba(56,189,248,0.1)",
              color: "#7dd3fc",
              cursor: "default",
            }}
          >
            {BADGES[c].icon} {c}
          </span>
        ) : null,
      )}
    </div>
  );
}
