import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { LiveTurn, PreviewStrip, visibleCodingMessages } from "./CodingView";
import { Composer, ContextRing } from "./Composer";

describe("PreviewStrip", () => {
  it("shows the live URL and window controls for a ready preview", () => {
    const html = renderToStaticMarkup(
      <PreviewStrip
        status={{
          sessionId: "session-1",
          windowOpen: true,
          url: "http://localhost:5173/",
          serverState: "ready",
          serverCommand: "pnpm dev",
        }}
        onFocus={() => undefined}
        onReload={() => undefined}
        onClose={() => undefined}
      />,
    );
    expect(html).toContain("preview ready");
    expect(html).toContain("http://localhost:5173/");
    expect(html).toContain("Focus");
    expect(html).toContain("Reload");
    expect(html).toContain("Close");
  });

  it("does not offer window actions while only the server is starting", () => {
    const html = renderToStaticMarkup(
      <PreviewStrip
        status={{
          sessionId: "session-1",
          windowOpen: false,
          url: null,
          serverState: "starting",
          serverCommand: "npm run dev",
        }}
        onFocus={() => undefined}
        onReload={() => undefined}
        onClose={() => undefined}
      />,
    );
    expect(html).toContain("starting");
    expect(html).toContain("npm run dev");
    expect(html).not.toContain("Focus");
    expect(html).not.toContain("Reload");
  });
});

describe("coding turn rendering", () => {
  it("places a pending approval below the response that introduced it", () => {
    const html = renderToStaticMarkup(
      <LiveTurn
        live={{
          messageId: "message-1",
          text: "I inspected the app and need to apply this change.",
          thinking: "",
          status: "streaming",
          trace: [{ kind: "tool", name: "write_file" }],
          approvals: [{ invocationId: "approval-1", name: "write_file", preview: "+ new file" }],
        }}
        onDecide={() => undefined}
      />,
    );
    expect(html.indexOf("I inspected the app")).toBeLessThan(html.indexOf("Approval needed"));
  });

  it("renders a sub-agent trace row with its task and returned summary", () => {
    const html = renderToStaticMarkup(
      <LiveTurn
        live={{
          messageId: "message-1",
          text: "",
          thinking: "",
          status: "streaming",
          trace: [
            { kind: "subagent", agent: "explore", task: "find the auth flow", summary: "auth lives in src/auth.ts" },
          ],
          approvals: [],
        }}
        onDecide={() => undefined}
      />,
    );
    expect(html).toContain("explore");
    expect(html).toContain("find the auth flow");
    expect(html).toContain("auth lives in src/auth.ts");
  });

  it("shows a running indicator for an in-flight sub-agent", () => {
    const html = renderToStaticMarkup(
      <LiveTurn
        live={{
          messageId: "message-1",
          text: "",
          thinking: "",
          status: "streaming",
          trace: [{ kind: "subagent", agent: "explore", task: "map the auth flow" }],
          approvals: [],
        }}
        onDecide={() => undefined}
      />,
    );
    expect(html).toContain("running…");
    expect(html).toContain("map the auth flow");
  });

  it("shows a working indicator while the model loads", () => {
    const html = renderToStaticMarkup(
      <LiveTurn
        live={{
          messageId: "message-1",
          text: "",
          thinking: "",
          status: "loading",
          loadingModel: "coder",
          trace: [],
          approvals: [],
        }}
        onDecide={() => undefined}
      />,
    );
    expect(html).toContain("Loading model coder…");
  });

  it("names the running tool in the working indicator", () => {
    const html = renderToStaticMarkup(
      <LiveTurn
        live={{
          messageId: "message-1",
          text: "",
          thinking: "",
          status: "streaming",
          trace: [{ kind: "tool", name: "read_file" }],
          approvals: [],
        }}
        onDecide={() => undefined}
      />,
    );
    expect(html).toContain("Running read_file…");
  });

  it("shows a generic working cue while streaming a response with no pending step", () => {
    const html = renderToStaticMarkup(
      <LiveTurn
        live={{
          messageId: "message-1",
          text: "Here is what I found",
          thinking: "",
          status: "streaming",
          trace: [],
          approvals: [],
        }}
        onDecide={() => undefined}
      />,
    );
    expect(html).toContain("Working…");
  });

  it("renders reasoning in a collapsible thinking block", () => {
    const html = renderToStaticMarkup(
      <LiveTurn
        live={{
          messageId: "message-1",
          text: "",
          thinking: "Let me check the router config first.",
          status: "streaming",
          trace: [],
          approvals: [],
        }}
        onDecide={() => undefined}
      />,
    );
    expect(html).toContain("Thinking");
    expect(html).toContain("Let me check the router config first.");
  });

  it("shows an estimated token count on the working line", () => {
    const html = renderToStaticMarkup(
      <LiveTurn
        live={{
          messageId: "message-1",
          text: "x".repeat(4000),
          thinking: "",
          status: "streaming",
          trace: [],
          approvals: [],
        }}
        onDecide={() => undefined}
      />,
    );
    expect(html).toContain("~1.0k tokens");
  });

  it("drops the working indicator once the turn pauses for approval", () => {
    const html = renderToStaticMarkup(
      <LiveTurn
        live={{
          messageId: "message-1",
          text: "",
          thinking: "",
          status: "streaming",
          trace: [{ kind: "tool", name: "write_file" }],
          approvals: [{ invocationId: "approval-1", name: "write_file", preview: "+ new file" }],
        }}
        onDecide={() => undefined}
      />,
    );
    expect(html).not.toContain("Working…");
    expect(html).not.toContain("Running write_file…");
  });

  it("hides legacy empty assistant records while preserving user messages", () => {
    const base = { thinking: null, toolActivity: null, contextNotes: null, cancelled: false, createdAt: "2026-01-01T00:00:00Z" };
    const visible = visibleCodingMessages([
      { ...base, role: "assistant", content: "" },
      { ...base, role: "assistant", content: "   " },
      { ...base, role: "user", content: "Continue" },
      { ...base, role: "assistant", content: "Done" },
    ]);
    expect(visible.map((message) => message.content)).toEqual(["Continue", "Done"]);
  });
});

describe("coding context indicator", () => {
  it("shows usage, estimate accuracy, and warning color", () => {
    const html = renderToStaticMarkup(
      <Composer
        models={[
          {
            id: "coder",
            name: "coder",
            loaded: true,
            sizeBytes: null,
            quantization: null,
            capabilities: [],
            sourceRepo: null,
            revision: null,
            variantId: null,
            files: [],
          },
        ]}
        model="coder"
        setModel={() => undefined}
        prompt=""
        setPrompt={() => undefined}
        onSend={() => undefined}
        busy={false}
        streaming={false}
        onStop={() => undefined}
        policy="read_only"
        onPolicyChange={() => undefined}
        mcpEnabled
        onMcpToggle={() => undefined}
        effort="none"
        onEffortChange={() => undefined}
        sessionId="session-1"
        attachments={[]}
        setAttachments={() => undefined}
        onError={() => undefined}
        contextInfo={{
          usedTokens: 24_000,
          maxTokens: 32_000,
          reserveTokens: 6_400,
          percent: 75,
          level: "orange",
          countExact: false,
          autoCompact: true,
          autoThreshold: 80,
          compacted: false,
          status: "idle",
          mcpTools: 8,
          mcpSchemaTokens: 1_200,
        }}
        compacting={false}
        onCompact={() => undefined}
        onContextSettings={() => undefined}
      />,
    );
    expect(html).toContain("Context 75% filled");
    expect(html).toContain("75% filled (24k of 32k)");
    expect(html).toContain("8 MCP tools use approximately 1.2k context tokens");
    expect(html).toContain("#fb923c");
  });

  it("renders a circular arc with the warning color", () => {
    const html = renderToStaticMarkup(<ContextRing percent={72} level="orange" />);
    expect(html).not.toContain("72%");
    expect(html).toContain("#fb923c");
    expect(html).toContain("stroke-dashoffset");
  });

  it("uses a blue arc for normal context usage", () => {
    const html = renderToStaticMarkup(<ContextRing percent={50} level="normal" />);
    expect(html).toContain("#3b82f6");
    expect(html).not.toContain("50%");
  });
});
