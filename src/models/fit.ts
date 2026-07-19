import type { GgufVariant, LocalStatus } from "../types";

export type FitKind = "vram" | "tight" | "ram" | "no" | "unknown";
export type FitAssessment = {
  kind: FitKind;
  label: string;
  detail: string;
  color: string;
  freeMemoryWarning: boolean;
};

export function assessFit(variant: GgufVariant, local: LocalStatus | null): FitAssessment {
  const required = variant.memory.totalBytes;
  const ram = local?.telemetry.memoryTotalBytes ?? 0;
  const ramFree = ram - (local?.telemetry.memoryUsedBytes ?? 0);
  const discrete = (local?.telemetry.gpus ?? []).filter((gpu) => {
    const name = (gpu.name ?? "").toLowerCase();
    const unified = gpu.vendor === "apple" ||
      (!!ram && gpu.memoryTotalBytes === ram) || name.includes("processor");
    return !unified && !!gpu.memoryTotalBytes;
  });
  const vram = discrete.reduce((sum, gpu) => sum + (gpu.memoryTotalBytes ?? 0), 0);
  const vramFree = discrete.reduce(
    (sum, gpu) => sum + Math.max(0, (gpu.memoryTotalBytes ?? 0) - (gpu.memoryUsedBytes ?? 0)),
    0,
  );
  const confidence = variant.memory.confidence === "low" ? " · approximate" : "";

  if (!required || (!ram && !vram)) {
    return {
      kind: "unknown",
      label: "Fit unknown",
      detail: `Hardware or model metadata is incomplete${confidence}`,
      color: "#94a3b8",
      freeMemoryWarning: false,
    };
  }
  if (vram && required <= vram * 0.9) {
    return {
      kind: "vram",
      label: "Fits in VRAM",
      detail: `${formatBytes(required)} estimated memory${confidence}`,
      color: "#34d399",
      freeMemoryWarning: vramFree < required,
    };
  }
  if (vram && required <= vram) {
    return {
      kind: "tight",
      label: "Tight VRAM fit",
      detail: `${formatBytes(required)} estimated memory${confidence}`,
      color: "#fbbf24",
      freeMemoryWarning: vramFree < required,
    };
  }
  if (ram && required <= ram * 0.8) {
    return {
      kind: "ram",
      label: vram ? "Fits with CPU offload" : "Fits in system RAM",
      detail: `${formatBytes(required)} estimated memory${confidence}`,
      color: "#60a5fa",
      freeMemoryWarning: ramFree < required,
    };
  }
  return {
    kind: "no",
    label: "Unlikely to fit",
    detail: `${formatBytes(required)} estimated memory exceeds safe capacity${confidence}`,
    color: "#f87171",
    freeMemoryWarning: true,
  };
}

export function chooseVariant(variants: GgufVariant[], local: LocalStatus | null): GgufVariant | null {
  if (!variants.length) return null;
  const ordered = [...variants].sort((a, b) => b.sizeBytes - a.sizeBytes);
  return ordered.find((v) => assessFit(v, local).kind === "vram") ??
    ordered.find((v) => ["tight", "ram"].includes(assessFit(v, local).kind)) ??
    ordered[ordered.length - 1]!;
}

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes)) return "—";
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(bytes >= 10 * 1024 ** 3 ? 0 : 1)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(0)} MB`;
  return `${bytes} B`;
}
