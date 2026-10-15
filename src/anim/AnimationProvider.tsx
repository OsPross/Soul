import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { MotionConfig } from "framer-motion";
import {
  DEFAULT_APPEARANCE,
  normalizeAppearance,
  resolveAnim,
  type Anim,
  type AppearanceConfig,
} from "./config";
import { applyTokens, hexToRgbTriplet } from "../theme/tokens";
import { translate, type I18nKey } from "../i18n";

interface Ctx {
  anim: Anim;
  /** Settings: change config → applies instantly in every window, persisted (debounced). */
  update: (next: AppearanceConfig) => void;
}

const AnimCtx = createContext<Ctx | null>(null);

const POWER_POLL_MS = 15_000;
const SAVE_DEBOUNCE_MS = 250;

function useReduceMotion(): boolean {
  const [reduce, setReduce] = useState(
    () => window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false,
  );
  useEffect(() => {
    const mq = window.matchMedia?.("(prefers-reduced-motion: reduce)");
    if (!mq) return;
    const on = () => setReduce(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return reduce;
}

/** Battery / fullscreen → saver. Polled only while the battery saver is enabled. */
function usePowerSaver(enabled: boolean): boolean {
  const [saver, setSaver] = useState(false);
  useEffect(() => {
    if (!enabled) {
      setSaver(false);
      return;
    }
    let alive = true;
    const poll = () =>
      invoke<{ onBattery?: boolean | null; fullscreen?: boolean | null }>("system_power_state")
        .then((p) => alive && setSaver(Boolean(p.onBattery) || Boolean(p.fullscreen)))
        .catch(() => undefined);
    void poll();
    const id = window.setInterval(poll, POWER_POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [enabled]);
  return saver;
}

function applyCssVars(cfg: AppearanceConfig, blurOn: boolean) {
  const root = document.documentElement;
  root.style.setProperty("--soul-accent", cfg.accent);
  root.style.setProperty("--soul-accent-rgb", hexToRgbTriplet(cfg.accent));
  root.style.setProperty("--soul-glass-alpha", String(cfg.islandOpacity));
  root.style.setProperty("--soul-blur", `${blurOn ? cfg.blur : 0}px`);
  root.style.setProperty("--soul-glow", String(cfg.glow));
}

export function AnimationProvider({ children }: { children: ReactNode }) {
  const [cfg, setCfg] = useState<AppearanceConfig>(DEFAULT_APPEARANCE);
  const reduceMotion = useReduceMotion();
  const saver = usePowerSaver(cfg.perf.batterySaver);
  const saveTimer = useRef<number | null>(null);

  useEffect(() => {
    applyTokens();
    let alive = true;
    const unsubs: Array<() => void> = [];
    invoke<{ appearance?: unknown }>("get_settings")
      .then((s) => alive && setCfg(normalizeAppearance(s.appearance)))
      .catch(() => undefined);
    listen<unknown>("soul://appearance", (e) => setCfg(normalizeAppearance(e.payload))).then((u) =>
      alive ? unsubs.push(u) : u(),
    );
    return () => {
      alive = false;
      unsubs.forEach((u) => u());
    };
  }, []);

  const anim = useMemo(() => resolveAnim(cfg, { reduceMotion, saver }), [cfg, reduceMotion, saver]);

  useEffect(() => {
    applyCssVars(cfg, anim.on("glassBlur"));
    document.documentElement.dataset.motion = anim.level;
  }, [cfg, anim]);

  const update = useCallback((next: AppearanceConfig) => {
    setCfg(next);
    // Instant live preview in every window; Rust persists and re-broadcasts the same value.
    void emit("soul://appearance", next);
    if (saveTimer.current !== null) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      invoke("save_appearance", { appearance: next }).catch(() => undefined);
    }, SAVE_DEBOUNCE_MS);
  }, []);

  const value = useMemo(() => ({ anim, update }), [anim, update]);

  return (
    <AnimCtx.Provider value={value}>
      {/* "never" — the user's own animation settings already account for reduce motion. */}
      <MotionConfig reducedMotion="never" transition={anim.spring("ui")}>
        {children}
      </MotionConfig>
    </AnimCtx.Provider>
  );
}

export function useAnim(): Anim {
  const ctx = useContext(AnimCtx);
  if (!ctx) throw new Error("useAnim outside AnimationProvider");
  return ctx.anim;
}

export function useAppearance(): Ctx {
  const ctx = useContext(AnimCtx);
  if (!ctx) throw new Error("useAppearance outside AnimationProvider");
  return ctx;
}

/** `t("island.listening")` in the configured UI language. */
export function useT() {
  const lang = useAnim().cfg.uiLang;
  return useCallback(
    (key: I18nKey, vars?: Record<string, string | number>) => translate(lang, key, vars),
    [lang],
  );
}
