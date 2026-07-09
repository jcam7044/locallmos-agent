import type { Attachment } from "../types";

export const TEXT_CAP = 32 * 1024;

const IMAGE_MIMES = new Set(["image/png", "image/jpeg", "image/webp", "image/gif"]);

export const FILE_ACCEPT = [
  "image/png",
  "image/jpeg",
  "image/webp",
  "image/gif",
  ".txt,.md,.markdown,.csv,.json,.js,.ts,.tsx,.jsx,.py,.rs,.go,.java,.c,.cpp,.h,.html,.css,.xml,.yaml,.yml,.toml,.sh,.sql,.log",
].join(",");

/** Read a picked File into an Attachment: images → base64, others → capped text. */
export async function fileToAttachment(file: File): Promise<Attachment> {
  if (IMAGE_MIMES.has(file.type)) {
    const dataUrl = await new Promise<string>((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => resolve(String(r.result));
      r.onerror = () => reject(new Error(`cannot read ${file.name}`));
      r.readAsDataURL(file);
    });
    return {
      kind: "image",
      name: file.name,
      mime: file.type,
      sizeBytes: file.size,
      data: dataUrl.slice(dataUrl.indexOf(",") + 1),
      text: null,
    };
  }
  let text = await file.text();
  // Binary sniff: real text files don't contain NUL bytes.
  if (text.includes("\u0000")) throw new Error(`${file.name}: not a text file`);
  if (text.length > TEXT_CAP) text = text.slice(0, TEXT_CAP) + "\n…[truncated]";
  return {
    kind: "text",
    name: file.name,
    mime: "text/plain",
    sizeBytes: file.size,
    data: null,
    text,
  };
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
}
