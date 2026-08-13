import { useEffect, useMemo, useState } from "react";
import { getSystemMetricsSnapshot } from "../api";
import type { DiskStat, GpuStat, SystemMetricsSnapshot } from "../types";
import { MetricGraph, type GraphSeries } from "./MetricGraph";
import {
  appendSample,
  availableDeviceKeys,
  diskValues,
  gpuValues,
  type DeviceKey,
  valuesFor,
} from "./history";
import "./resources.css";

const COLORS = {
  cpu: "#38bdf8",
  memory: "#c084fc",
  gpu: "#fb7185",
  disk: "#fb923c",
  write: "#facc15",
};

export function ResourcesWindow() {
  const [history, setHistory] = useState<SystemMetricsSnapshot[]>([]);
  const [selected, setSelected] = useState<DeviceKey>("cpu");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    let busy = false;
    const poll = async () => {
      if (busy) return;
      busy = true;
      try {
        const snapshot = isWebPreview()
          ? previewSnapshot()
          : await getSystemMetricsSnapshot();
        if (!disposed) {
          setHistory((current) => appendSample(current, snapshot));
          setError(null);
        }
      } catch (reason) {
        if (!disposed) setError(String(reason));
      } finally {
        busy = false;
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 1_000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, []);

  const latest = history.at(-1);
  useEffect(() => {
    if (!availableDeviceKeys(latest).includes(selected)) setSelected("cpu");
  }, [latest, selected]);

  return (
    <ResourcesView
      history={history}
      selected={selected}
      onSelect={setSelected}
      error={error}
    />
  );
}

export function ResourcesView({
  history,
  selected,
  onSelect,
  error = null,
}: {
  history: SystemMetricsSnapshot[];
  selected: DeviceKey;
  onSelect: (key: DeviceKey) => void;
  error?: string | null;
}) {
  const latest = history.at(-1);
  const cards = useMemo(() => deviceCards(history), [history]);

  return (
    <main className="resources-shell">
      <aside className="resources-sidebar" aria-label="Hardware devices">
        <div className="resources-brand">
          <span>Resources</span>
          <small>Last 60 seconds</small>
        </div>
        <div className="device-list">
          {cards.map((device) => (
            <button
              key={device.key}
              className="device-card"
              data-selected={selected === device.key}
              onClick={() => onSelect(device.key)}
              aria-pressed={selected === device.key}
            >
              <div className="device-card-copy">
                <span className="device-kind" style={{ color: device.color }}>{device.kind}</span>
                <strong>{device.title}</strong>
                <small>{device.value}</small>
              </div>
              <MetricGraph
                label={`${device.title} sparkline`}
                series={[{ label: device.title, color: device.color, values: device.values }]}
                max={device.max}
                height={50}
              />
            </button>
          ))}
        </div>
      </aside>

      <section className="resources-detail">
        {error && <div className="resources-error">Metrics unavailable: {error}</div>}
        {!latest ? (
          <div className="resources-loading">Collecting the first system sample…</div>
        ) : (
          <DetailPanel history={history} selected={selected} latest={latest} />
        )}
      </section>
    </main>
  );
}

type DeviceCard = {
  key: DeviceKey;
  kind: string;
  title: string;
  value: string;
  color: string;
  values: Array<number | null>;
  max?: number;
};

function deviceCards(history: SystemMetricsSnapshot[]): DeviceCard[] {
  const latest = history.at(-1);
  const memoryPct = latest ? percent(latest.memoryUsedBytes, latest.memoryTotalBytes) : null;
  const cards: DeviceCard[] = [
    {
      key: "cpu",
      kind: "CPU",
      title: "Processor",
      value: formatPercent(latest?.cpuUtilizationPct),
      color: COLORS.cpu,
      values: valuesFor(history, (sample) => sample.cpuUtilizationPct),
      max: 100,
    },
    {
      key: "memory",
      kind: "RAM",
      title: "Memory",
      value: memoryPct == null
        ? "Not reported"
        : `${formatBytes(latest?.memoryUsedBytes)} / ${formatBytes(latest?.memoryTotalBytes)}`,
      color: COLORS.memory,
      values: valuesFor(history, (sample) => percent(sample.memoryUsedBytes, sample.memoryTotalBytes)),
      max: 100,
    },
  ];
  for (const gpu of latest?.gpus ?? []) {
    cards.push({
      key: `gpu:${gpu.id}`,
      kind: `GPU ${gpu.index + 1}`,
      title: gpu.name ?? titleCase(gpu.vendor),
      value: gpu.utilizationPct == null
        ? "Utilization not reported"
        : `${formatPercent(gpu.utilizationPct)} · ${formatBytes(gpu.memoryUsedBytes)} VRAM`,
      color: COLORS.gpu,
      values: gpuValues(history, gpu.id, (item) => item.utilizationPct),
      max: 100,
    });
  }
  for (const disk of latest?.disks ?? []) {
    const activity = sumNullable(disk.readBytesPerSecond, disk.writeBytesPerSecond);
    cards.push({
      key: `disk:${disk.id}`,
      kind: disk.kind.toUpperCase(),
      title: driveTitle(disk),
      value: activity == null
        ? `${formatPercent(percent(disk.usedBytes, disk.totalBytes))} used`
        : `${formatRate(activity)} total I/O`,
      color: COLORS.disk,
      values: diskValues(history, disk.id, (item) =>
        sumNullable(item.readBytesPerSecond, item.writeBytesPerSecond)),
    });
  }
  return cards;
}

function DetailPanel({
  history,
  selected,
  latest,
}: {
  history: SystemMetricsSnapshot[];
  selected: DeviceKey;
  latest: SystemMetricsSnapshot;
}) {
  if (selected === "cpu") {
    const current = latest.cpuUtilizationPct;
    return <DeviceDetail eyebrow="CPU" title="Processor" summary={formatPercent(current)}>
      <GraphCard title="Total utilization" value={formatPercent(current)}>
        <MetricGraph
          label="CPU utilization"
          series={[series("Utilization", COLORS.cpu, valuesFor(history, (s) => s.cpuUtilizationPct))]}
          max={100}
        />
      </GraphCard>
      <StatGrid stats={[
        ["Current utilization", formatPercent(current)],
        ["Sampling interval", "1 second"],
        ["History", "60 seconds"],
      ]} />
    </DeviceDetail>;
  }

  if (selected === "memory") {
    const current = percent(latest.memoryUsedBytes, latest.memoryTotalBytes);
    return <DeviceDetail eyebrow="RAM" title="Memory" summary={formatPercent(current)}>
      <GraphCard title="Memory usage" value={`${formatBytes(latest.memoryUsedBytes)} / ${formatBytes(latest.memoryTotalBytes)}`}>
        <MetricGraph
          label="Memory utilization"
          series={[series("Used", COLORS.memory, valuesFor(history, (s) =>
            percent(s.memoryUsedBytes, s.memoryTotalBytes)))]}
          max={100}
        />
      </GraphCard>
      <StatGrid stats={[
        ["Used", formatBytes(latest.memoryUsedBytes)],
        ["Available", subtractBytes(latest.memoryTotalBytes, latest.memoryUsedBytes)],
        ["Total", formatBytes(latest.memoryTotalBytes)],
      ]} />
    </DeviceDetail>;
  }

  if (selected.startsWith("gpu:")) {
    const id = selected.slice(4);
    const gpu = latest.gpus.find((item) => item.id === id);
    return gpu ? <GpuDetail gpu={gpu} history={history} /> : null;
  }

  const id = selected.slice(5);
  const disk = latest.disks.find((item) => item.id === id);
  return disk ? <DiskDetail disk={disk} history={history} /> : null;
}

function GpuDetail({ gpu, history }: { gpu: GpuStat; history: SystemMetricsSnapshot[] }) {
  const utilization = gpuValues(history, gpu.id, (item) => item.utilizationPct);
  const memory = gpuValues(history, gpu.id, (item) => item.memoryUsedBytes);
  const temperature = gpuValues(history, gpu.id, (item) => item.temperatureC);
  const power = gpuValues(history, gpu.id, (item) => item.powerWatts);
  return <DeviceDetail
    eyebrow={`GPU ${gpu.index + 1} · ${titleCase(gpu.vendor)}`}
    title={gpu.name ?? `${titleCase(gpu.vendor)} GPU`}
    summary={formatPercent(gpu.utilizationPct)}
  >
    <GraphCard title="Total utilization" value={formatPercent(gpu.utilizationPct)}>
      <MetricGraph label="GPU utilization" series={[series("Utilization", COLORS.gpu, utilization)]} max={100} />
    </GraphCard>
    <GraphCard title="Video memory" value={`${formatBytes(gpu.memoryUsedBytes)} / ${formatBytes(gpu.memoryTotalBytes)}`}>
      <MetricGraph
        label="Video memory usage"
        series={[series("Used VRAM", "#f43f5e", memory)]}
        max={gpu.memoryTotalBytes ?? undefined}
        height={125}
      />
    </GraphCard>
    {(temperature.some(isNumber) || power.some(isNumber)) && <div className="compact-graphs">
      {temperature.some(isNumber) && <GraphCard title="Temperature" value={formatTemperature(gpu.temperatureC)} compact>
        <MetricGraph label="GPU temperature" series={[series("Temperature", "#f97316", temperature)]} height={90} />
      </GraphCard>}
      {power.some(isNumber) && <GraphCard title="Power" value={formatPower(gpu.powerWatts)} compact>
        <MetricGraph label="GPU power" series={[series("Power", "#facc15", power)]} height={90} />
      </GraphCard>}
    </div>}
    <StatGrid stats={[
      ["Video memory", `${formatBytes(gpu.memoryUsedBytes)} / ${formatBytes(gpu.memoryTotalBytes)}`],
      ["Temperature", formatTemperature(gpu.temperatureC)],
      ["Power", formatPower(gpu.powerWatts)],
      ["Device ID", gpu.id],
    ]} />
  </DeviceDetail>;
}

function DiskDetail({ disk, history }: { disk: DiskStat; history: SystemMetricsSnapshot[] }) {
  const read = diskValues(history, disk.id, (item) => item.readBytesPerSecond);
  const write = diskValues(history, disk.id, (item) => item.writeBytesPerSecond);
  const usedPct = percent(disk.usedBytes, disk.totalBytes);
  return <DeviceDetail
    eyebrow={disk.kind.toUpperCase()}
    title={driveTitle(disk)}
    summary={`${formatPercent(usedPct)} used`}
  >
    <GraphCard title="Disk activity" value={`${formatRate(disk.readBytesPerSecond)} read · ${formatRate(disk.writeBytesPerSecond)} write`}>
      <MetricGraph
        label="Disk read and write throughput"
        series={[
          series("Read", COLORS.disk, read),
          series("Write", COLORS.write, write),
        ]}
      />
      {disk.readBytesPerSecond == null && disk.writeBytesPerSecond == null &&
        <p className="unavailable-note">Live I/O counters are not reported on this platform or volume.</p>}
      <div className="graph-legend">
        <span><i style={{ background: COLORS.disk }} />Read</span>
        <span><i style={{ background: COLORS.write }} />Write</span>
      </div>
    </GraphCard>
    <div className="capacity-card">
      <div><span>Storage used</span><strong>{formatBytes(disk.usedBytes)} / {formatBytes(disk.totalBytes)}</strong></div>
      <div className="capacity-track"><i style={{ width: `${usedPct ?? 0}%` }} /></div>
    </div>
    <StatGrid stats={[
      ["Read", formatRate(disk.readBytesPerSecond)],
      ["Write", formatRate(disk.writeBytesPerSecond)],
      ["Available", formatBytes(disk.totalBytes - disk.usedBytes)],
      ["Mount point", disk.mountPoint],
    ]} />
  </DeviceDetail>;
}

function DeviceDetail({
  eyebrow,
  title,
  summary,
  children,
}: {
  eyebrow: string;
  title: string;
  summary: string;
  children: React.ReactNode;
}) {
  return <div className="device-detail-content">
    <header className="detail-header">
      <div><small>{eyebrow}</small><h1>{title}</h1></div>
      <strong>{summary}</strong>
    </header>
    {children}
  </div>;
}

function GraphCard({
  title,
  value,
  compact = false,
  children,
}: {
  title: string;
  value: string;
  compact?: boolean;
  children: React.ReactNode;
}) {
  return <section className={`graph-card${compact ? " graph-card-compact" : ""}`}>
    <header><span>{title}</span><strong>{value}</strong></header>
    {children}
  </section>;
}

function StatGrid({ stats }: { stats: Array<[string, string]> }) {
  return <dl className="stat-grid">
    {stats.map(([name, value]) => <div key={name}><dt>{name}</dt><dd>{value}</dd></div>)}
  </dl>;
}

function series(label: string, color: string, values: Array<number | null>): GraphSeries {
  return { label, color, values };
}

function percent(used: number | null | undefined, total: number | null | undefined) {
  return used == null || total == null || total <= 0 ? null : (used / total) * 100;
}

function sumNullable(a: number | null, b: number | null) {
  return a == null && b == null ? null : (a ?? 0) + (b ?? 0);
}

function formatPercent(value: number | null | undefined) {
  return value == null ? "Not reported" : `${value.toFixed(0)}%`;
}

export function formatBytes(value: number | null | undefined) {
  if (value == null) return "Not reported";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let amount = value;
  let unit = 0;
  while (amount >= 1024 && unit < units.length - 1) {
    amount /= 1024;
    unit += 1;
  }
  return `${amount.toFixed(unit >= 3 ? 1 : 0)} ${units[unit]}`;
}

function formatRate(value: number | null | undefined) {
  return value == null ? "Not reported" : `${formatBytes(value)}/s`;
}

function formatTemperature(value: number | null | undefined) {
  return value == null ? "Not reported" : `${value.toFixed(0)} °C`;
}

function formatPower(value: number | null | undefined) {
  return value == null ? "Not reported" : `${value.toFixed(1)} W`;
}

function subtractBytes(total: number | null, used: number | null) {
  return total == null || used == null ? "Not reported" : formatBytes(Math.max(0, total - used));
}

function driveTitle(disk: DiskStat) {
  return disk.mountPoint || disk.name;
}

function titleCase(value: string) {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

function isNumber(value: number | null): value is number {
  return value != null;
}

function isWebPreview() {
  return import.meta.env.DEV && !("__TAURI_INTERNALS__" in window);
}

/** Representative local-only data for reviewing the window in Vite. */
function previewSnapshot(): SystemMetricsSnapshot {
  const seconds = Date.now() / 1_000;
  const wave = (offset: number, low: number, high: number) =>
    low + ((Math.sin(seconds / 3 + offset) + 1) / 2) * (high - low);
  return {
    sampledAtMs: Date.now(),
    cpuUtilizationPct: wave(0, 8, 62),
    memoryUsedBytes: wave(1, 11, 13) * 1024 ** 3,
    memoryTotalBytes: 32 * 1024 ** 3,
    gpus: [
      {
        id: "preview:nvidia:0",
        index: 0,
        name: "NVIDIA GeForce RTX 5060 Ti",
        vendor: "nvidia",
        utilizationPct: wave(2, 4, 92),
        memoryUsedBytes: wave(3, 7, 12) * 1024 ** 3,
        memoryTotalBytes: 16 * 1024 ** 3,
        temperatureC: wave(4, 39, 67),
        powerWatts: wave(5, 35, 155),
      },
      {
        id: "preview:intel:0",
        index: 1,
        name: "Intel Integrated Graphics",
        vendor: "intel",
        utilizationPct: null,
        memoryUsedBytes: null,
        memoryTotalBytes: null,
        temperatureC: null,
        powerWatts: null,
      },
    ],
    disks: [
      {
        id: "preview:root",
        name: "NVMe SSD",
        mountPoint: "/",
        kind: "ssd",
        usedBytes: 307 * 1024 ** 3,
        totalBytes: 512 * 1024 ** 3,
        readBytesPerSecond: wave(6, 0, 180) * 1024 ** 2,
        writeBytesPerSecond: wave(7, 0, 95) * 1024 ** 2,
      },
      {
        id: "preview:data",
        name: "Data",
        mountPoint: "/mnt/data",
        kind: "hdd",
        usedBytes: 1.4 * 1024 ** 4,
        totalBytes: 2 * 1024 ** 4,
        readBytesPerSecond: null,
        writeBytesPerSecond: null,
      },
    ],
  };
}
