import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Composer } from "./Composer";

describe("chat context indicator", () => {
  it("shows current context usage and MCP schema impact", () => {
    const html = renderToStaticMarkup(
      <Composer
        disabled={false}
        streaming={false}
        onSend={() => undefined}
        effort="none"
        canThink={false}
        onChangeEffort={() => undefined}
        webTools={false}
        canWebTools
        onToggleWebTools={() => undefined}
        mcp
        canMcp
        onToggleMcp={() => undefined}
        attachments={[]}
        onAddFiles={() => undefined}
        onRemoveAttachment={() => undefined}
        contextInfo={{
          usedTokens: 12_000,
          maxTokens: 128_000,
          reserveTokens: 8_192,
          percent: 9,
          level: "normal",
          countExact: false,
          autoCompact: true,
          autoThreshold: 80,
          compacted: false,
          status: "idle",
          mcpTools: 33,
          mcpSchemaTokens: 6_400,
        }}
        compacting={false}
        onCompact={() => undefined}
        onContextSettings={() => undefined}
      />,
    );

    expect(html).toContain("Context 9% filled");
    expect(html).toContain("12k of 128k");
  });
});
