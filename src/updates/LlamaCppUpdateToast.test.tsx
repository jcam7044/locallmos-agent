import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { LlamaCppUpdateInfo, LlamaCppUpdateProgress } from "../types";
import { LlamaCppUpdateToast } from "./LlamaCppUpdateToast";

const info: LlamaCppUpdateInfo = {
  currentTag: "b10087",
  latestTag: "b10353",
  backend: "cuda",
  variant: "13.3",
  sizeBytes: 220 * 1024 * 1024,
  installable: true,
  reason: null,
};

describe("LlamaCppUpdateToast", () => {
  it("presents an explicit user-initiated update", () => {
    const html = renderToStaticMarkup(<LlamaCppUpdateToast info={info} progress={null} onInstall={() => undefined} onDismiss={() => undefined} />);
    expect(html).toContain("llama.cpp update available");
    expect(html).toContain("b10087 → b10353 · cuda 13.3");
    expect(html).toContain("Download and install");
    expect(html).toContain("Dismiss");
  });

  it("renders streamed progress", () => {
    const progress: LlamaCppUpdateProgress = {
      phase: "downloading", tag: "b10353", downloadedBytes: 50, totalBytes: 200, message: null,
    };
    const html = renderToStaticMarkup(<LlamaCppUpdateToast info={info} progress={progress} onInstall={() => undefined} onDismiss={() => undefined} />);
    expect(html).toContain("Downloading llama.cpp");
    expect(html).toContain("width:25%");
    expect(html).toContain('aria-valuenow="25"');
  });

  it("explains externally managed installs", () => {
    const html = renderToStaticMarkup(<LlamaCppUpdateToast info={{ ...info, installable: false, reason: "Use the service installer." }} progress={null} onInstall={() => undefined} onDismiss={() => undefined} />);
    expect(html).toContain("Use the service installer.");
    expect(html).toContain("disabled");
  });
});
