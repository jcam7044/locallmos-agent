import { useEffect, useRef } from "react";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";

export type Tab = "dashboard" | "models" | "chat" | "code" | "tools";

type Size = { w: number; h: number };

// The dashboard opens compact; every other tab shares one "work" size.
const DASHBOARD_DEFAULT: Size = { w: 480, h: 560 };
const DASHBOARD_MIN: Size = { w: 420, h: 480 };
const WORK_DEFAULT: Size = { w: 1100, h: 740 };
const WORK_MIN: Size = { w: 800, h: 600 };

/**
 * Resize the window when moving to/from the dashboard. The dashboard opens
 * compact on first launch; all other tabs share a single size that follows the
 * user's manual resizes, so switching between them never changes the window.
 * Resizes are best-effort: a missing capability must not break tab switching.
 */
export function useTabWindowSize(tab: Tab) {
  // The size to restore when the user returns to a non-dashboard tab. Starts at
  // the default and tracks whatever size the user last set while on such a tab.
  const workSize = useRef<Size>({ ...WORK_DEFAULT });
  const prev = useRef<Tab | null>(null);

  useEffect(() => {
    const from = prev.current;
    prev.current = tab;
    if (from === tab) return;

    void (async () => {
      const w = getCurrentWindow();
      // Remember the user's current size before leaving a non-dashboard tab.
      if (from !== null && from !== "dashboard") {
        const [inner, scale] = await Promise.all([w.innerSize(), w.scaleFactor()]);
        workSize.current = { w: inner.width / scale, h: inner.height / scale };
      }

      if (tab === "dashboard") {
        await w.setMinSize(new LogicalSize(DASHBOARD_MIN.w, DASHBOARD_MIN.h));
        // Only the very first mount applies the compact default; returning to the
        // dashboard later keeps whatever (larger) size the window already has.
        if (from === null) {
          await w.setSize(new LogicalSize(DASHBOARD_DEFAULT.w, DASHBOARD_DEFAULT.h));
        }
        return;
      }

      await w.setMinSize(new LogicalSize(WORK_MIN.w, WORK_MIN.h));
      // Switching between non-dashboard tabs maintains the user's size; only
      // arriving from the dashboard (or first launch) restores the work size.
      if (from !== null && from !== "dashboard") return;
      const target = workSize.current;
      await w.setSize(
        new LogicalSize(Math.max(target.w, WORK_MIN.w), Math.max(target.h, WORK_MIN.h)),
      );
    })().catch(() => {
      /* resize is cosmetic — ignore permission/platform failures */
    });
  }, [tab]);
}
