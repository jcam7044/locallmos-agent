import { describe, expect, it } from "vitest";
import type { GgufVariant, LocalStatus } from "../types";
import { assessFit, chooseVariant } from "./fit";

const GB = 1024 ** 3;

function variant(totalGb: number, id = `${totalGb}gb`): GgufVariant {
  return {
    id,
    quantization: "Q4_K_M",
    sizeBytes: Math.max(1, totalGb - 1) * GB,
    files: [],
    companions: [],
    memory: {
      weightsBytes: Math.max(1, totalGb - 1) * GB,
      kvCacheBytes: GB,
      overheadBytes: 0,
      totalBytes: totalGb * GB,
      confidence: "low",
    },
  };
}

function hardware(ramGb: number, gpuGb = 0, vendor = "nvidia"): LocalStatus {
  return {
    runtime: { kind: "llamacpp", version: null, state: "running", endpoint: null, modelsDir: "/models", contextSize: 8192 },
    configuredRuntime: "llamacpp",
    models: [],
    modelsStorage: { dir: "/models", availableBytes: 100 * GB, totalBytes: 100 * GB },
    telemetry: {
      cpuPct: 0,
      memoryUsedBytes: 2 * GB,
      memoryTotalBytes: ramGb * GB,
      gpus: gpuGb ? [{ index: 0, name: vendor === "apple" ? "Apple M4" : "GPU", vendor, utilizationPct: 0, memoryUsedBytes: GB, memoryTotalBytes: gpuGb * GB, temperatureC: null, powerWatts: null }] : [],
    },
  };
}

describe("assessFit", () => {
  it("distinguishes safe and tight discrete VRAM fits", () => {
    expect(assessFit(variant(12), hardware(32, 16)).kind).toBe("vram");
    expect(assessFit(variant(15), hardware(32, 16)).kind).toBe("tight");
  });

  it("falls back to RAM and reports models that exceed both pools", () => {
    expect(assessFit(variant(20), hardware(32, 12)).kind).toBe("ram");
    expect(assessFit(variant(30), hardware(32, 12)).kind).toBe("no");
  });

  it("does not double count Apple unified memory as discrete VRAM", () => {
    const fit = assessFit(variant(18), hardware(24, 24, "apple"));
    expect(fit.kind).toBe("ram");
    expect(fit.label).toBe("Fits in system RAM");
  });

  it("chooses the largest safe variant before smaller fallbacks", () => {
    const picked = chooseVariant([variant(17, "large"), variant(12, "medium"), variant(6, "small")], hardware(32, 16));
    expect(picked?.id).toBe("medium");
  });
});
