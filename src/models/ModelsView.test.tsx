import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { HubModelDetail, LocalModel, LocalStatus } from "../types";
import { isLocalModelRemovable, isRecommendedModelLoadSettings, isVariantOnDevice, localModelAction, ModelCardReadme, ModelsView, modelLoadSettingsError, modelLogo, recommendedModelLoadSettings, variantsBySizeAscending } from "./ModelsView";

function detail(readmeMarkdown: string): HubModelDetail {
  return {
    id: "owner/model-GGUF",
    author: "owner",
    name: "model-GGUF",
    revision: "abc123",
    downloads: 0,
    likes: 0,
    lastModified: null,
    pipelineTag: "text-generation",
    tags: ["gguf"],
    avatarUrl: "",
    license: null,
    baseModels: [],
    variants: [],
    readmeMarkdown,
  };
}

describe("modelLogo", () => {
  it("finds and revision-pins a raw HTML logo", () => {
    const logo = modelLogo(detail('<p align="center"><img src="./assets/model-logo.svg" width="280"></p>'));
    expect(logo).toBe("https://huggingface.co/owner/model-GGUF/resolve/abc123/assets/model-logo.svg");
  });

  it("prefers an explicitly named logo over decorative markdown images", () => {
    const logo = modelLogo(detail("![chart](assets/chart.png)\n![brand](assets/project-logo.png)"));
    expect(logo).toContain("project-logo.png");
  });

  it("prefers the model logo after the title over publisher buttons", () => {
    const logo = modelLogo(detail('<img src="unsloth-logo.png" width="133">\n# Model\n<img src="agentworld/logo.png" width="400px">'));
    expect(logo).toContain("agentworld/logo.png");
  });
});

describe("ModelCardReadme", () => {
  it("preserves safe author-specified image dimensions", () => {
    const html = renderToStaticMarkup(<ModelCardReadme detail={detail('<div style="display:flex"><img src="button.png" width="133"><img src="logo.png" width="400px" height="80"></div>')} />);
    expect(html).toContain('width="133"');
    expect(html).toContain('width="400px"');
    expect(html).toContain('height="80"');
    expect(html).not.toContain('style=');
  });
});

describe("isVariantOnDevice", () => {
  it("only marks the matching Hub repository and quantization as downloaded", () => {
    const model = { id: "local-id", name: "model-Q4_K_M.gguf", sizeBytes: 1, quantization: "Q4_K_M", loaded: false, capabilities: [], sourceRepo: "owner/model-GGUF", revision: "abc", variantId: "q4", files: ["model-Q4_K_M.gguf"] };
    expect(isVariantOnDevice(model, "owner/model-GGUF", "q4")).toBe(true);
    expect(isVariantOnDevice(model, "owner/model-GGUF", "q8")).toBe(false);
    expect(isVariantOnDevice(model, "other/model-GGUF", "q4")).toBe(false);
  });
});

describe("variantsBySizeAscending", () => {
  it("orders displayed quantizations from the smallest download to the largest", () => {
    const variants = [
      { id: "q8", quantization: "Q8_0", sizeBytes: 9_700, files: [], companions: [], memory: {} },
      { id: "q3", quantization: "Q3_K_M", sizeBytes: 4_200, files: [], companions: [], memory: {} },
      { id: "q5", quantization: "Q5_K_M", sizeBytes: 6_200, files: [], companions: [], memory: {} },
    ] as never[];
    expect(variantsBySizeAscending(variants).map((variant) => variant.id)).toEqual(["q3", "q5", "q8"]);
  });
});

describe("model load settings", () => {
  it("recognizes the reset state and custom overrides", () => {
    const recommended = recommendedModelLoadSettings();
    expect(isRecommendedModelLoadSettings(recommended)).toBe(true);
    expect(isRecommendedModelLoadSettings({ ...recommended, kvCacheType: "q8_0" })).toBe(false);
    expect(isRecommendedModelLoadSettings({ ...recommended, contextSize: 32768 })).toBe(false);
    expect(isRecommendedModelLoadSettings({ ...recommended, speculativeDecoding: "off" })).toBe(false);
    expect(isRecommendedModelLoadSettings({ ...recommended, maxToolCalls: 25 })).toBe(false);
  });

  it("validates custom numeric bounds", () => {
    const recommended = recommendedModelLoadSettings();
    expect(modelLoadSettingsError({ ...recommended, contextSize: 511 })).toContain("Context size");
    expect(modelLoadSettingsError({ ...recommended, cpuThreads: 513 })).toContain("CPU threads");
    expect(modelLoadSettingsError({ ...recommended, maxToolCalls: 101 })).toContain("Max tool calls");
    expect(modelLoadSettingsError({ ...recommended, contextSize: 32768, cpuThreads: 12 })).toBeNull();
  });
});

describe("Ollama model management", () => {
  const ollamaModel: LocalModel = {
    id: "qwen3:8b", name: "qwen3:8b", sizeBytes: 5_200_000_000, quantization: "Q4_K_M",
    loaded: false, capabilities: ["tools"], sourceRepo: null, revision: null, variantId: null, files: [],
  };

  it("offers the Ollama pull experience when Ollama is active", () => {
    const local: LocalStatus = {
      runtime: { kind: "ollama", version: "0.9.0", backend: null, state: "running", endpoint: "http://127.0.0.1:11434", modelsDir: null, contextSize: 8192 },
      configuredRuntime: "ollama",
      modelsStorage: { dir: "/models", availableBytes: null, totalBytes: null },
      models: [ollamaModel],
      telemetry: { cpuPct: null, memoryUsedBytes: null, memoryTotalBytes: null, gpus: [] },
    };
    const html = renderToStaticMarkup(<ModelsView local={local} onChanged={() => undefined} />);
    expect(html).toContain("Pull from the Ollama registry");
    expect(html).toContain("On Device <span>1</span>");
    expect(html).not.toContain("Popular GGUF Models");
  });

  it("allows runtime-managed Ollama models to be removed without Hub metadata", () => {
    expect(isLocalModelRemovable(ollamaModel, true)).toBe(true);
    expect(isLocalModelRemovable(ollamaModel, false)).toBe(false);
  });
});

describe("model action labels", () => {
  it("keeps the pending action attached to its model across a status refresh", () => {
    expect(localModelAction({ id: "model-a", type: "load" }, "model-a")).toBe("load");
    expect(localModelAction({ id: "model-a", type: "load" }, "model-b")).toBeNull();
    expect(localModelAction({ id: "model-a", type: "eject" }, "model-a")).toBe("eject");
  });
});
