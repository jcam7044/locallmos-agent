import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { DownloadBanner, downloadPercent } from "./DownloadBanner";
import type { DownloadState } from "../types";

const download = (status: DownloadState["status"], downloadedBytes = 0): DownloadState => ({
  id: "download-1", repoId: "owner/model-GGUF", revision: "abc", variantId: "Q4_K_M", status,
  downloadedBytes, totalBytes: 4_000, error: null,
});

describe("DownloadBanner", () => {
  it("shows aggregate model download progress", () => {
    const html = renderToStaticMarkup(<DownloadBanner downloads={[download("downloading", 1_500)]} onDismiss={() => undefined} onCancel={() => undefined} />);
    expect(html).toContain("Downloading model");
    expect(html).toContain("38%");
    expect(html).toContain("model-GGUF · Q4_K_M");
    expect(html).toContain("Dismiss download for model-GGUF");
    expect(html).toContain(">Cancel<");
  });

  it("hides completed downloads and clamps progress", () => {
    expect(renderToStaticMarkup(<DownloadBanner downloads={[download("complete", 4_000)]} onDismiss={() => undefined} onCancel={() => undefined} />)).toBe("");
    expect(downloadPercent(download("downloading", 5_000))).toBe(100);
  });
});
