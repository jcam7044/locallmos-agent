import { inputStyle } from "../styles";
import type { LocalModel } from "../types";

export function ModelPicker({
  models,
  value,
  onChange,
}: {
  models: LocalModel[];
  value: string;
  onChange: (model: string) => void;
}) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      style={{ ...inputStyle, marginTop: 0, width: "auto", maxWidth: 320 }}
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
  );
}
