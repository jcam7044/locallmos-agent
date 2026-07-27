import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { PreviewStrip } from "./CodingView";

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
