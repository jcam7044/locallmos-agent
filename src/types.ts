export type GpuStat = {
  index: number;
  name: string | null;
  vendor: string;
  utilizationPct: number | null;
  memoryUsedBytes: number | null;
  memoryTotalBytes: number | null;
  temperatureC: number | null;
  powerWatts: number | null;
};

export type AgentStatus = {
  enrolled: boolean;
  rigName: string | null;
  connected: boolean;
};

export type LocalModel = {
  name: string;
  sizeBytes: number | null;
  quantization: string | null;
  loaded: boolean;
  capabilities: string[];
};

export type LocalStatus = {
  runtime: { kind: string; version: string | null; state: string; endpoint: string | null };
  models: LocalModel[];
  telemetry: {
    cpuPct: number | null;
    memoryUsedBytes: number | null;
    memoryTotalBytes: number | null;
    gpus: GpuStat[];
  };
};

export type ChatMsg = { role: "user" | "assistant"; content: string };

export function formatGB(bytes: number | null | undefined): string {
  if (bytes == null) return "—";
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}
