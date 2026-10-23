import type { Transition } from "framer-motion";
import { tokens } from "../theme/tokens";
import type { UiLang } from "../i18n";

/**
 * Central appearance + animation config. Persisted by Rust as the opaque `appearance` blob
 * (`save_appearance`), broadcast as `soul://appearance`, and consumed through
 * `AnimationProvider` / `useAnim()` by every component. No component hardcodes durations or
 * springs — they ask `anim.spring()` / `anim.dur()` / `anim.on(feature)`.
 */

export type Preset = "full" | "balanced" | "minimal" | "off";

export const FEATURES = [
  "spirit",
  "spiritIdle",
  "particles",
  "spiritMic",
  "musicSway",
  "morph",
  "tabs",
  "micro",
  "marquee",
  "equalizer",
  "waveform",
  "albumTint",
  "glassBlur",
  "effects",
  "askEffects",
  "caption",
] as const;
export type Feature = (typeof FEATURES)[number];

export type SpiritStyle = "droplet" | "orb";

export interface AppearanceConfig {
  version: 1;
  uiLang: UiLang;
  /** User accent (hex). */
  accent: string;
  /** Spirit size multiplier 0.8–1.4. */
  spiritSize: number;
  /** SVG droplet ghost or the WebGL particle orb. */
  spiritStyle: SpiritStyle;
  /** Island glass opacity 0.80–1. */
  islandOpacity: number;
  /** Backdrop blur px 0–40. */
  blur: number;
  /** Glow intensity 0–1.5. */
  glow: number;
  anim: {
    preset: Preset;
    followReduceMotion: boolean;
    /** 0.5–2 (2 = twice as fast). */
    speed: number;
    /** 0 = soft, 1 = snappy. */
    stiffness: number;
    features: Record<Feature, boolean>;
  };
  perf: {
    /** Auto Minimal on battery / fullscreen apps / games. */
    batterySaver: boolean;
    fpsCap: 30 | 60;
    /** Pause spirit animations when collapsed and idle for 30 s. */
    pauseWhenIdle: boolean;
  };
  noise: {
    expandOnDictation: boolean;
    expandOnCommand: boolean;
    /** Circle-to-Ask runs in its own window; this shows / hides its spirit guide. */
    expandOnAsk: boolean;
    /** How long results / captions stay visible (ms). */
    resultMs: number;
    sounds: boolean;
    /** 0–1 */
    soundVolume: number;
    /** Synthesized UI sound pack (`sounds: false` = "Wyłączony"). */
    soundPack: "futuristic" | "subtle";
    quiet: boolean;
  };
}

export const DEFAULT_APPEARANCE: AppearanceConfig = {
  version: 1,
  uiLang: "pl",
  accent: tokens.accentDefault,
  spiritSize: 1,
  spiritStyle: "droplet",
  islandOpacity: 0.94,
  blur: 24,
  glow: 1,
  anim: {
    preset: "full",
    followReduceMotion: true,
    speed: 1,
    stiffness: 0.5,
    features: Object.fromEntries(FEATURES.map((f) => [f, true])) as Record<Feature, boolean>,
  },
  perf: { batterySaver: true, fpsCap: 60, pauseWhenIdle: true },
  noise: {
    expandOnDictation: true,
    expandOnCommand: true,
    expandOnAsk: true,
    resultMs: 2200,
    sounds: true,
    soundVolume: 0.5,
    soundPack: "futuristic",
    quiet: false,
  },
};

/** Features each preset allows (a feature runs only when its toggle AND the preset allow it). */
const PRESET_ALLOWS: Record<Preset, ReadonlySet<Feature>> = {
  full: new Set(FEATURES),
  balanced: new Set(FEATURES.filter((f) => f !== "particles")),
  minimal: new Set<Feature>(["spirit", "spiritMic", "morph", "waveform", "albumTint", "glassBlur", "caption"]),
  off: new Set<Feature>(["spirit", "albumTint", "glassBlur", "caption"]),
};

export function presetAllows(preset: Preset, f: Feature): boolean {
  return PRESET_ALLOWS[preset].has(f);
}

const PRESET_RANK: Record<Preset, number> = { full: 3, balanced: 2, minimal: 1, off: 0 };

/** The more restrictive of two presets. */
export function clampPreset(a: Preset, b: Preset): Preset {
  return PRESET_RANK[a] <= PRESET_RANK[b] ? a : b;
}

const clamp = (v: unknown, lo: number, hi: number, d: number) =>
  typeof v === "number" && Number.isFinite(v) ? Math.min(hi, Math.max(lo, v)) : d;
const bool = (v: unknown, d: boolean) => (typeof v === "boolean" ? v : d);

/** Merge a stored (possibly partial / older) blob over the defaults, validating each field. */
export function normalizeAppearance(raw: unknown): AppearanceConfig {
  const d = DEFAULT_APPEARANCE;
  const r = (raw && typeof raw === "object" ? raw : {}) as Partial<AppearanceConfig> & Record<string, unknown>;
  const a = (r.anim ?? {}) as Partial<AppearanceConfig["anim"]>;
  const p = (r.perf ?? {}) as Partial<AppearanceConfig["perf"]>;
  const n = (r.noise ?? {}) as Partial<AppearanceConfig["noise"]>;
  const feats = (a.features ?? {}) as Partial<Record<Feature, boolean>>;
  const preset: Preset = ["full", "balanced", "minimal", "off"].includes(a.preset as string)
    ? (a.preset as Preset)
    : d.anim.preset;
  return {
    version: 1,
    uiLang: r.uiLang === "en" ? "en" : "pl",
    accent: typeof r.accent === "string" && /^#[0-9a-f]{6}$/i.test(r.accent) ? r.accent : d.accent,
    spiritSize: clamp(r.spiritSize, 0.8, 1.4, d.spiritSize),
    spiritStyle: r.spiritStyle === "orb" ? "orb" : "droplet",
    islandOpacity: clamp(r.islandOpacity, 0.8, 1, d.islandOpacity),
    blur: clamp(r.blur, 0, 40, d.blur),
    glow: clamp(r.glow, 0, 1.5, d.glow),
    anim: {
      preset,
      followReduceMotion: bool(a.followReduceMotion, d.anim.followReduceMotion),
      speed: clamp(a.speed, 0.5, 2, d.anim.speed),
      stiffness: clamp(a.stiffness, 0, 1, d.anim.stiffness),
      features: Object.fromEntries(
        FEATURES.map((f) => [f, bool(feats[f], d.anim.features[f])]),
      ) as Record<Feature, boolean>,
    },
    perf: {
      batterySaver: bool(p.batterySaver, d.perf.batterySaver),
      fpsCap: p.fpsCap === 30 ? 30 : 60,
      pauseWhenIdle: bool(p.pauseWhenIdle, d.perf.pauseWhenIdle),
    },
    noise: {
      expandOnDictation: bool(n.expandOnDictation, d.noise.expandOnDictation),
      expandOnCommand: bool(n.expandOnCommand, d.noise.expandOnCommand),
      expandOnAsk: bool(n.expandOnAsk, d.noise.expandOnAsk),
      resultMs: clamp(n.resultMs, 800, 8000, d.noise.resultMs),
      sounds: bool(n.sounds, d.noise.sounds),
      soundVolume: clamp(n.soundVolume, 0, 1, d.noise.soundVolume),
      soundPack: n.soundPack === "subtle" ? "subtle" : "futuristic",
      quiet: bool(n.quiet, d.noise.quiet),
    },
  };
}

export type SpringKind = "morph" | "ui" | "bouncy" | "gentle";

/** Resolved, ready-to-use view of the config for components. */
export interface Anim {
  cfg: AppearanceConfig;
  /** Effective preset after OS reduce-motion and battery saver. */
  level: Preset;
  /** Why `level` is lower than the chosen preset (for Settings), or null. */
  limitedBy: "reduceMotion" | "battery" | null;
  on: (f: Feature) => boolean;
  /** Framer Motion transition honouring preset, speed and stiffness. */
  spring: (kind?: SpringKind) => Transition;
  /** Duration in seconds scaled by speed; 0 when animations are off. */
  dur: (ms: number) => number;
  fps: 30 | 60;
}

const BASE: Record<SpringKind, { k: number; zeta: number; mass: number }> = {
  morph: { k: 420, zeta: 0.72, mass: 0.8 },
  ui: { k: 520, zeta: 0.85, mass: 0.6 },
  bouncy: { k: 380, zeta: 0.5, mass: 0.7 },
  gentle: { k: 200, zeta: 0.9, mass: 1 },
};

export function resolveAnim(
  cfg: AppearanceConfig,
  env: { reduceMotion: boolean; saver: boolean },
): Anim {
  let level = cfg.anim.preset;
  let limitedBy: Anim["limitedBy"] = null;
  if (cfg.anim.followReduceMotion && env.reduceMotion && level !== clampPreset(level, "minimal")) {
    level = "minimal";
    limitedBy = "reduceMotion";
  }
  if (cfg.perf.batterySaver && env.saver && level !== clampPreset(level, "minimal")) {
    level = "minimal";
    limitedBy = limitedBy ?? "battery";
  }
  const speed = cfg.anim.speed;
  const on = (f: Feature) => cfg.anim.features[f] && presetAllows(level, f);
  const dur = (ms: number) => (level === "off" ? 0 : ms / 1000 / speed);

  const spring = (kind: SpringKind = "ui"): Transition => {
    if (level === "off" || (kind === "morph" && !on("morph"))) return { duration: 0 };
    if (level === "minimal") return { type: "tween", ease: [0.2, 0.8, 0.2, 1], duration: 0.14 / speed };
    const b = BASE[kind];
    // stiffness slider: 0 → ×0.45 (soft), 1 → ×1.8 (snappy).
    const kMul = 0.45 * Math.pow(4, cfg.anim.stiffness);
    const stiffness = b.k * kMul * speed * speed;
    // Balanced preset trades bounce for calm.
    const zeta = level === "balanced" ? Math.max(b.zeta, 0.85) : b.zeta;
    const damping = 2 * zeta * Math.sqrt(stiffness * b.mass);
    return { type: "spring", stiffness, damping, mass: b.mass };
  };

  return { cfg, level, limitedBy, on, spring, dur, fps: cfg.perf.fpsCap };
}
