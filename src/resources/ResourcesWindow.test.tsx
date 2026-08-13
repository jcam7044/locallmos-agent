import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { SystemMetricsSnapshot } from "../types";
import { areaPoints, MetricGraph, polylineSegments } from "./MetricGraph";
import { ResourcesView } from "./ResourcesWindow";
import { appendSample, availableDeviceKeys, diskValues, gpuValues } from "./history";

function snapshot(sampledAtMs = 1): SystemMetricsSnapshot {
  return {
    sampledAtMs,
    cpuUtilizationPct: 25,
    memoryUsedBytes: 8 * 1024 ** 3,
    memoryTotalBytes: 32 * 1024 ** 3,
    gpus: [{
      id: "GPU-abc",
      index: 0,
      name: "RTX Test",
      vendor: "nvidia",
      utilizationPct: 40,
      memoryUsedBytes: 4 * 1024 ** 3,
      memoryTotalBytes: 16 * 1024 ** 3,
      temperatureC: 55,
      powerWatts: 80,
    }],
    disks: [{
      id: "/dev/test@/",
      name: "/dev/test",
      mountPoint: "/",
      kind: "ssd",
      usedBytes: 100,
      totalBytes: 400,
      readBytesPerSecond: 2_000,
      writeBytesPerSecond: 1_000,
    }],
  };
}

describe("resource history", () => {
  it("retains only the newest 60 samples", () => {
    let history: SystemMetricsSnapshot[] = [];
    for (let i = 0; i < 65; i += 1) history = appendSample(history, snapshot(i));
    expect(history).toHaveLength(60);
    expect(history[0]!.sampledAtMs).toBe(5);
    expect(history.at(-1)?.sampledAtMs).toBe(64);
  });

  it("tracks stable devices and leaves gaps when a device disappears", () => {
    const missing = { ...snapshot(2), gpus: [], disks: [] };
    expect(availableDeviceKeys(snapshot())).toEqual([
      "cpu", "memory", "gpu:GPU-abc", "disk:/dev/test@/",
    ]);
    expect(gpuValues([snapshot(), missing], "GPU-abc", (gpu) => gpu.utilizationPct)).toEqual([40, null]);
    expect(diskValues([snapshot(), missing], "/dev/test@/", (disk) => disk.readBytesPerSecond)).toEqual([2_000, null]);
  });
});

describe("metric graph", () => {
  it("splits lines at unavailable samples", () => {
    const segments = polylineSegments([10, 20, null, 30, 40], 600, 100, 100);
    expect(segments).toHaveLength(2);
    expect(segments[0]).toContain("90.00");
    expect(segments[1]).toContain("60.00");
  });

  it("closes each line segment against the graph baseline for area fill", () => {
    expect(areaPoints("10.00,80.00 20.00,60.00", 100)).toBe(
      "10.00,100 10.00,80.00 20.00,60.00 20.00,100",
    );
    const html = renderToStaticMarkup(
      <MetricGraph
        label="Filled metric"
        series={[{ label: "Usage", color: "#38bdf8", values: [10, 20] }]}
        max={100}
      />,
    );
    expect(html).toContain("<polygon");
    expect(html).toContain('fill-opacity="0.2"');
    expect(html).toContain("<polyline");
  });
});

describe("ResourcesView", () => {
  it("renders CPU, RAM, every GPU, and every drive in the device list", () => {
    const html = renderToStaticMarkup(
      <ResourcesView history={[snapshot()]} selected="gpu:GPU-abc" onSelect={() => undefined} />,
    );
    expect(html).toContain("Processor");
    expect(html).toContain("Memory");
    expect(html).toContain("RTX Test");
    expect(html).toContain("GPU utilization");
    expect(html).toContain("Video memory");
    expect(html).toContain("SSD");
  });

  it("labels unavailable drive I/O instead of displaying zero", () => {
    const disk = { ...snapshot().disks[0]!, readBytesPerSecond: null, writeBytesPerSecond: null };
    const sample = { ...snapshot(), disks: [disk] };
    const html = renderToStaticMarkup(
      <ResourcesView history={[sample]} selected={`disk:${disk.id}`} onSelect={() => undefined} />,
    );
    expect(html).toContain("Live I/O counters are not reported");
    expect(html).toContain("Not reported");
  });
});
