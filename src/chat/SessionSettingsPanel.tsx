import { inputStyle, label } from "../styles";
import type { SessionSettings } from "../types";

export function SessionSettingsPanel({
  settings,
  onChange,
  showContext,
}: {
  settings: SessionSettings;
  onChange: (patch: Partial<SessionSettings>) => void;
  showContext: boolean;
}) {
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 8,
        padding: "10px 0",
        borderBottom: "1px solid #1f2937",
        marginBottom: 4,
      }}
    >
      <div>
        <span style={label}>System prompt</span>
        <textarea
          placeholder="e.g. You are a concise assistant."
          value={settings.systemPrompt ?? ""}
          onChange={(e) => onChange({ systemPrompt: e.target.value || null })}
          rows={3}
          style={{
            ...inputStyle,
            marginTop: 4,
            resize: "vertical",
            fontFamily: "inherit",
            fontSize: 13,
          }}
        />
      </div>
      <div style={{ display: "flex", gap: 12 }}>
        {showContext ? <div style={{ flex: 1 }}>
          <span style={label}>Temperature (0–2)</span>
          <input
            type="number"
            min={0}
            max={2}
            step={0.1}
            placeholder="model default"
            value={settings.temperature ?? ""}
            onChange={(e) =>
              onChange({ temperature: e.target.value === "" ? null : Number(e.target.value) })
            }
            style={{ ...inputStyle, marginTop: 4 }}
          />
        </div> : <div style={{ flex: 1, color: "#718096", fontSize: 11, alignSelf: "center" }}>
          Context allocation is configured per model in Models → Load settings.
        </div>}
        <div style={{ flex: 1 }}>
          <span style={label}>Context length (num_ctx)</span>
          <input
            type="number"
            min={512}
            step={512}
            placeholder="model default"
            value={settings.numCtx ?? ""}
            onChange={(e) =>
              onChange({ numCtx: e.target.value === "" ? null : Number(e.target.value) })
            }
            style={{ ...inputStyle, marginTop: 4 }}
          />
        </div>
      </div>
    </div>
  );
}
