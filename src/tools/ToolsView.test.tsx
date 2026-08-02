import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { McpCatalogEntry } from "../types";
import { CatalogDetailsDialog } from "./ToolsView";

const entry: McpCatalogEntry = {
  id: "example",
  label: "Example server",
  description: "A short summary.",
  details: "A more complete explanation of what the server does.",
  connection: "Runs locally and connects with a scoped API token.",
  tools: [
    { name: "find_records", description: "Find matching records." },
    { name: "create_record", description: "Create a record." },
  ],
  runtime: "npx",
  inputs: [],
  defaultTrust: "untrusted",
  caveat: "Creating records changes remote data.",
  command: "npx -y example@1.0.0",
};

describe("CatalogDetailsDialog", () => {
  it("shows connection guidance and the server's tools before installation", () => {
    const html = renderToStaticMarkup(
      <CatalogDetailsDialog
        entry={entry}
        installed={false}
        runtimeAvailable
        onClose={() => undefined}
        onInstall={() => undefined}
      />,
    );

    expect(html).toContain('role="dialog"');
    expect(html).toContain("A more complete explanation");
    expect(html).toContain("Typical connection");
    expect(html).toContain("scoped API token");
    expect(html).toContain("find_records");
    expect(html).toContain("create_record");
    expect(html).toContain("Install…");
  });

  it("identifies an installed server and removes the install action", () => {
    const html = renderToStaticMarkup(
      <CatalogDetailsDialog
        entry={entry}
        installed
        runtimeAvailable
        onClose={() => undefined}
        onInstall={() => undefined}
      />,
    );

    expect(html).toContain("Installed");
    expect(html).not.toContain("Install…");
  });
});
