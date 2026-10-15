import { useCallback, useEffect, useRef, useState } from "react";

export type IslandMode = "collapsed" | "peek" | "expanded";

export const HOVER_PEEK_MS = 300;
export const LEAVE_COLLAPSE_MS = 400;
const WHEEL_LOCK_MS = 350;

/**
 * The island's single expand/collapse state machine.
 *
 *   collapsed --hover 300ms--> peek --click / wheel down--> expanded
 *   collapsed --click / wheel down--------------------------^
 *   peek | expanded --click pill (expanded) / wheel up / Esc / window blur /
 *                     pointer outside 400ms / Rust force-collapse--> collapsed
 *
 * All transitions go through `go`, which reads/writes `modeRef`, so timers never
 * see stale state. If the mode is open while the pointer is not over the panel,
 * a leave timer is always armed (so a missed `pointerleave` can't leave it stuck).
 */
export function useIslandMode() {
  const [mode, setMode] = useState<IslandMode>("collapsed");
  const modeRef = useRef<IslandMode>("collapsed");
  const hovering = useRef(false);
  const hoverTimer = useRef<number | null>(null);
  const leaveTimer = useRef<number | null>(null);
  const wheelLockUntil = useRef(0);

  const clearTimers = useCallback(() => {
    if (hoverTimer.current !== null) window.clearTimeout(hoverTimer.current);
    if (leaveTimer.current !== null) window.clearTimeout(leaveTimer.current);
    hoverTimer.current = null;
    leaveTimer.current = null;
  }, []);

  const go = useCallback(
    (next: IslandMode) => {
      clearTimers();
      wheelLockUntil.current = Date.now() + WHEEL_LOCK_MS;
      if (modeRef.current === next) return;
      modeRef.current = next;
      setMode(next);
    },
    [clearTimers],
  );

  const armLeave = useCallback(() => {
    if (leaveTimer.current !== null) return;
    leaveTimer.current = window.setTimeout(() => {
      leaveTimer.current = null;
      if (!hovering.current) go("collapsed");
    }, LEAVE_COLLAPSE_MS);
  }, [go]);

  const collapse = useCallback(() => go("collapsed"), [go]);
  const expand = useCallback(() => go("expanded"), [go]);
  const toggle = useCallback(
    () => go(modeRef.current === "expanded" ? "collapsed" : "expanded"),
    [go],
  );

  const onPointerEnter = useCallback(() => {
    hovering.current = true;
    if (leaveTimer.current !== null) {
      window.clearTimeout(leaveTimer.current);
      leaveTimer.current = null;
    }
    if (modeRef.current === "collapsed" && hoverTimer.current === null) {
      hoverTimer.current = window.setTimeout(() => {
        hoverTimer.current = null;
        if (hovering.current && modeRef.current === "collapsed") go("peek");
      }, HOVER_PEEK_MS);
    }
  }, [go]);

  const onPointerLeave = useCallback(() => {
    hovering.current = false;
    if (hoverTimer.current !== null) {
      window.clearTimeout(hoverTimer.current);
      hoverTimer.current = null;
    }
    if (modeRef.current !== "collapsed") armLeave();
  }, [armLeave]);

  const onWheel = useCallback(
    (e: { deltaY: number }) => {
      if (Date.now() < wheelLockUntil.current) return;
      if (e.deltaY > 8 && modeRef.current !== "expanded") go("expanded");
      else if (e.deltaY < -8 && modeRef.current !== "collapsed") go("collapsed");
    },
    [go],
  );

  // Esc and window blur collapse immediately.
  useEffect(() => {
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") go("collapsed");
    };
    const onBlur = () => go("collapsed");
    window.addEventListener("keydown", onKey);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("blur", onBlur);
    };
  }, [go]);

  // Open but pointer not over the panel (e.g. leave event lost): make sure a timer runs.
  useEffect(() => {
    if (mode !== "collapsed" && !hovering.current) armLeave();
  }, [mode, armLeave]);

  useEffect(() => clearTimers, [clearTimers]);

  return {
    mode,
    collapse,
    expand,
    toggle,
    handlers: { onPointerEnter, onPointerLeave, onWheel },
  };
}
