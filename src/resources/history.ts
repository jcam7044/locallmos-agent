import type { SystemMetricsSnapshot } from "../types";

export const HISTORY_SAMPLES = 60;

export type DeviceKey =
  | "cpu"
  | "memory"
  | `gpu:${string}`
  | `disk:${string}`
  | `network:${string}`;

export function appendSample(
  history: SystemMetricsSnapshot[],
  sample: SystemMetricsSnapshot,
  limit = HISTORY_SAMPLES,
) {
  return [...history, sample].slice(-limit);
}

export function availableDeviceKeys(snapshot: SystemMetricsSnapshot | undefined): DeviceKey[] {
  if (!snapshot) return ["cpu", "memory"];
  return [
    "cpu",
    "memory",
    ...snapshot.gpus.map((gpu) => `gpu:${gpu.id}` as const),
    ...snapshot.disks.map((disk) => `disk:${disk.id}` as const),
    ...snapshot.networks.map((network) => `network:${network.id}` as const),
  ];
}

export function valuesFor(
  history: SystemMetricsSnapshot[],
  read: (sample: SystemMetricsSnapshot) => number | null | undefined,
) {
  return history.map((sample) => read(sample) ?? null);
}

export function gpuValues(
  history: SystemMetricsSnapshot[],
  id: string,
  read: (gpu: SystemMetricsSnapshot["gpus"][number]) => number | null,
) {
  return history.map((sample) => {
    const gpu = sample.gpus.find((candidate) => candidate.id === id);
    return gpu ? read(gpu) : null;
  });
}

export function diskValues(
  history: SystemMetricsSnapshot[],
  id: string,
  read: (disk: SystemMetricsSnapshot["disks"][number]) => number | null,
) {
  return history.map((sample) => {
    const disk = sample.disks.find((candidate) => candidate.id === id);
    return disk ? read(disk) : null;
  });
}

export function networkValues(
  history: SystemMetricsSnapshot[],
  id: string,
  read: (network: SystemMetricsSnapshot["networks"][number]) => number | null,
) {
  return history.map((sample) => {
    const network = sample.networks.find((candidate) => candidate.id === id);
    return network ? read(network) : null;
  });
}
