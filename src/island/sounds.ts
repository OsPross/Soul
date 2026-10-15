/**
 * UI sound effects, synthesized locally with WebAudio (no bundled or third-party assets).
 * Two packs: `futuristic` (glassy sweeps and chimes, Jarvis-like) and `subtle` (soft sine blips).
 * Created lazily; callers decide whether sounds are enabled (Island: settings + quiet mode).
 */
import { invoke } from "@tauri-apps/api/core";

let ctx: AudioContext | null = null;

export type Cue = "listen" | "listenEnd" | "success" | "error" | "notify" | "timer";
export type SoundPack = "futuristic" | "subtle";

interface Tone {
  type?: OscillatorType;
  /** Start / end frequency (Hz); a sweep when they differ. */
  f0: number;
  f1?: number;
  at: number;
  len: number;
  gain: number;
  /** Optional low-pass cutoff sweep (Hz). */
  lp?: [number, number];
  detune?: number;
}

function tone(c: AudioContext, out: AudioNode, t: Tone) {
  const now = c.currentTime + t.at;
  const osc = c.createOscillator();
  const g = c.createGain();
  osc.type = t.type ?? "sine";
  osc.frequency.setValueAtTime(t.f0, now);
  if (t.f1 && t.f1 !== t.f0) osc.frequency.exponentialRampToValueAtTime(t.f1, now + t.len * 0.85);
  if (t.detune) osc.detune.setValueAtTime(t.detune, now);
  g.gain.setValueAtTime(0.0001, now);
  g.gain.exponentialRampToValueAtTime(t.gain, now + Math.min(0.015, t.len / 4));
  g.gain.exponentialRampToValueAtTime(0.0001, now + t.len);
  let node: AudioNode = osc;
  if (t.lp) {
    const f = c.createBiquadFilter();
    f.type = "lowpass";
    f.frequency.setValueAtTime(t.lp[0], now);
    f.frequency.exponentialRampToValueAtTime(t.lp[1], now + t.len);
    osc.connect(f);
    node = f;
  }
  node.connect(g).connect(out);
  osc.start(now);
  osc.stop(now + t.len + 0.03);
}

/** Bell = fundamental + inharmonic partials with faster decay. */
function bell(at: number, f: number, gain: number, len = 0.5): Tone[] {
  return [
    { f0: f, at, len, gain },
    { f0: f * 2.76, at, len: len * 0.45, gain: gain * 0.35 },
    { f0: f * 5.4, at, len: len * 0.2, gain: gain * 0.15 },
  ];
}

const FUTURISTIC: Record<Cue, Tone[]> = {
  // Rising glassy sweep + shimmer: "I'm listening".
  listen: [
    { type: "sine", f0: 480, f1: 1040, at: 0, len: 0.22, gain: 0.13 },
    { type: "triangle", f0: 482, f1: 1046, at: 0, len: 0.22, gain: 0.05, detune: 9 },
    { type: "sine", f0: 2080, at: 0.16, len: 0.12, gain: 0.04 },
  ],
  // Short falling sweep: "got it".
  listenEnd: [
    { type: "sine", f0: 980, f1: 560, at: 0, len: 0.16, gain: 0.1 },
    { type: "triangle", f0: 990, f1: 566, at: 0, len: 0.16, gain: 0.035, detune: -8 },
  ],
  // Two-tone confirm with a filtered sparkle.
  success: [
    ...bell(0, 1318.5, 0.08, 0.32),
    ...bell(0.085, 1975.5, 0.07, 0.42),
    { type: "sawtooth", f0: 659, at: 0, len: 0.22, gain: 0.02, lp: [3000, 500] },
  ],
  // Low filtered double pulse.
  error: [
    { type: "square", f0: 220, f1: 180, at: 0, len: 0.14, gain: 0.05, lp: [1400, 300] },
    { type: "square", f0: 196, f1: 150, at: 0.16, len: 0.2, gain: 0.05, lp: [1200, 250] },
    { type: "sine", f0: 110, at: 0, len: 0.36, gain: 0.06 },
  ],
  // Rising three-note chime.
  notify: [...bell(0, 1046.5, 0.07, 0.4), ...bell(0.09, 1318.5, 0.065, 0.4), ...bell(0.18, 1568, 0.06, 0.55)],
  // Alarm-ish chime pattern, three times.
  timer: [0, 0.55, 1.1].flatMap((d) => [...bell(d, 1568, 0.08, 0.35), ...bell(d + 0.14, 2093, 0.07, 0.45)]),
};

const SUBTLE: Record<Cue, Tone[]> = {
  listen: [
    { f0: 660, at: 0, len: 0.09, gain: 0.1 },
    { f0: 880, at: 0.07, len: 0.12, gain: 0.1 },
  ],
  listenEnd: [{ f0: 740, f1: 620, at: 0, len: 0.1, gain: 0.07 }],
  success: [
    { f0: 784, at: 0, len: 0.12, gain: 0.1 },
    { f0: 1175, at: 0.09, len: 0.22, gain: 0.1 },
  ],
  error: [
    { f0: 330, at: 0, len: 0.16, gain: 0.1 },
    { f0: 247, at: 0.11, len: 0.24, gain: 0.1 },
  ],
  notify: [
    { f0: 988, at: 0, len: 0.14, gain: 0.08 },
    { f0: 1319, at: 0.1, len: 0.2, gain: 0.08 },
  ],
  timer: [0, 0.45, 0.9].flatMap((d) => [
    { f0: 1047, at: d, len: 0.16, gain: 0.1 } as Tone,
    { f0: 1319, at: d + 0.12, len: 0.2, gain: 0.1 } as Tone,
  ]),
};

export function playCue(cue: Cue, volume: number, pack: SoundPack = "futuristic") {
  if (volume <= 0) return;
  try {
    ctx = ctx ?? new AudioContext();
    if (ctx.state === "suspended") void ctx.resume();
    const master = ctx.createGain();
    master.gain.value = Math.min(1, volume) * 1.6;
    master.connect(ctx.destination);
    for (const t of (pack === "subtle" ? SUBTLE : FUTURISTIC)[cue]) tone(ctx, master, t);
    window.setTimeout(() => master.disconnect(), 2500);
    invoke("sfx_log", { cue, pack }).catch(() => undefined);
  } catch {
    /* audio unavailable — cues are optional */
  }
}
