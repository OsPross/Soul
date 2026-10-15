import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";

/**
 * Audio levels for animation, kept in refs (no React re-renders at 30 fps).
 *   mic   — `soul://mic-level`   (Rust, ~30 Hz while recording / Settings monitor)
 *   voice — `soul://voice-level` (Rust, TTS playback)
 * Values decay towards 0 if events stop arriving, so a lost "0" can't freeze a glow.
 */
export interface LevelRefs {
  mic: { current: number };
  voice: { current: number };
}

const shared: LevelRefs & { at: { mic: number; voice: number }; users: number; stop?: () => void } = {
  mic: { current: 0 },
  voice: { current: 0 },
  at: { mic: 0, voice: 0 },
  users: 0,
};

function start() {
  let alive = true;
  const unsubs: Array<() => void> = [];
  const on = (ev: string, key: "mic" | "voice") =>
    listen<{ level: number }>(ev, (e) => {
      shared[key].current = Math.max(0, Math.min(1, e.payload.level || 0));
      shared.at[key] = performance.now();
    }).then((u) => (alive ? unsubs.push(u) : u()));
  void on("soul://mic-level", "mic");
  void on("soul://voice-level", "voice");
  listen<{ speaking: boolean }>("soul://voice-speaking", (e) => {
    if (!e.payload.speaking) shared.voice.current = 0;
  }).then((u) => (alive ? unsubs.push(u) : u()));
  shared.stop = () => {
    alive = false;
    unsubs.forEach((u) => u());
  };
}

/** Read a level, decaying stale values (no event for >250 ms). */
export function readLevel(key: "mic" | "voice"): number {
  const age = performance.now() - shared.at[key];
  if (age > 250) shared[key].current *= 0.9;
  return shared[key].current;
}

export function useLevels(): LevelRefs {
  const refs = useRef<LevelRefs>(shared);
  useEffect(() => {
    if (shared.users++ === 0) start();
    return () => {
      if (--shared.users === 0) {
        shared.stop?.();
        shared.stop = undefined;
      }
    };
  }, []);
  return refs.current;
}
