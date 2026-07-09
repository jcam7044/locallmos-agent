import { formatSize } from "./attachments";
import type { Attachment } from "../types";

export function AttachmentChip({
  attachment,
  onRemove,
}: {
  attachment: Attachment;
  onRemove?: () => void;
}) {
  return (
    <span
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 6,
        padding: "3px 8px",
        borderRadius: 8,
        border: "1px solid #1f2937",
        background: "#131926",
        fontSize: 11,
        color: "#cbd5e1",
        maxWidth: 220,
      }}
      title={`${attachment.name} · ${formatSize(attachment.sizeBytes)}`}
    >
      {attachment.kind === "image" && attachment.data ? (
        <img
          src={`data:${attachment.mime};base64,${attachment.data}`}
          alt={attachment.name}
          style={{ width: 22, height: 22, objectFit: "cover", borderRadius: 4 }}
        />
      ) : (
        <span>📄</span>
      )}
      <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
        {attachment.name}
      </span>
      {onRemove && (
        <button
          onClick={onRemove}
          title="Remove"
          style={{
            border: "none",
            background: "transparent",
            color: "#64748b",
            cursor: "pointer",
            padding: 0,
            fontSize: 11,
          }}
        >
          ✕
        </button>
      )}
    </span>
  );
}
