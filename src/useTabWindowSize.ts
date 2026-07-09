import { useEffect, useRef } from "react";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";

export type Tab = "dashboard" | "chat";

type Size = { w: number; h: number };

const DEFAULTS: Record<Tab, Size> = {
  dashboard: { w: 480, h: 560 },
  chat: { w: 1000, h: 700 },
};

const MINS: Record<Tab, Size> = {
  dashboard: { w: 420, h: 480 },
  chat: { w: 800, h: 600 },
};

/**
 * Resize the window per tab, remembering the user's manual resizes for each.
 * Resizes are best-effort: a missing capability must not break tab switching.
 */
export function useTabWindowSize(tab: Tab) {
  const sizes = useRef<Record<Tab, Size>>({ ...DEFAULTS });
  const prev = useRef<Tab | null>(null);

  useEffect(() => {
    const from = prev.current;
    prev.current = tab;
    if (from === tab) return;

    void (async () => {
      const w = getCurrentWindow();
      if (from !== null) {
        const [inner, scale] = await Promise.all([w.innerSize(), w.scaleFactor()]);
        sizes.current[from] = { w: inner.width / scale, h: inner.height / scale };
      }
      const min = MINS[tab];
      const target = sizes.current[tab];
      await w.setMinSize(new LogicalSize(min.w, min.h));
      await w.setSize(new LogicalSize(Math.max(target.w, min.w), Math.max(target.h, min.h)));
    })().catch(() => {
      /* resize is cosmetic — ignore permission/platform failures */
    });
  }, [tab]);
}
