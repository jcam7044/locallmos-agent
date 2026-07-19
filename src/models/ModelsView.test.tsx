import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { HubModelDetail } from "../types";
import { ModelCardReadme, modelLogo } from "./ModelsView";

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
