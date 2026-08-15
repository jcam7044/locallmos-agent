import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { AgentUpdateInfo } from "../types";
import { AgentUpdateToast } from "./AgentUpdateToast";

const info: AgentUpdateInfo = {
  currentVersion: "0.0.1",
  latestVersion: "0.0.2",
  os: "linux",
  installCommand: "curl -fsSL https://locallmos.com/install.sh | sh",
};

describe("AgentUpdateToast", () => {
  it("shows the version delta and the copyable install command", () => {
    const html = renderToStaticMarkup(<AgentUpdateToast info={info} onDismiss={() => undefined} />);
    expect(html).toContain("Agent update available");
    expect(html).toContain("0.0.1 → 0.0.2");
    expect(html).toContain("curl -fsSL https://locallmos.com/install.sh | sh");
    expect(html).toContain("Copy command");
    expect(html).toContain("Dismiss");
  });

  it("renders the Windows PowerShell command for windows installs", () => {
    const win: AgentUpdateInfo = {
      ...info,
      os: "windows",
      installCommand: "iex ((curl.exe -fsSL https://locallmos.com/install.ps1) -join \"`n\")",
    };
    const html = renderToStaticMarkup(<AgentUpdateToast info={win} onDismiss={() => undefined} />);
    expect(html).toContain("install.ps1");
  });

  it("renders nothing when there is no update", () => {
    const html = renderToStaticMarkup(<AgentUpdateToast info={null} onDismiss={() => undefined} />);
    expect(html).toBe("");
  });
});
