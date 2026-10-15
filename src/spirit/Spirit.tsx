import { useEffect, useId, useRef } from "react";
import { useAnim } from "../anim/AnimationProvider";
import { readLevel, useLevels } from "../lib/levels";
import { hexToRgbTriplet } from "../theme/tokens";
import { ParticleField } from "./particles";

/**
 * Soul's spirit: a small translucent droplet-ghost with inner light, two eyes and tiny nubs.
 *
 * One rAF loop per instance eases a handful of parameters (scale, eye openness, mouth, glow…)
 * towards per-state targets and writes SVG attributes directly — transform/opacity style work
 * only, no React re-render per frame. Audio levels come from refs (`lib/levels`).
 * Everything honours the central animation config (`useAnim`).
 */
export type SpiritState =
  | "idle"
  | "listening"
  | "thinking"
  | "speaking"
  | "success"
  | "error"
  | "music";

interface Props {
  state: SpiritState;
  /** Glow / body colour (hex). */
  color: string;
  /** Body size in CSS px (before the user's spirit-size multiplier). */
  size: number;
  /** Freeze the loop (collapsed + idle for a while). */
  paused?: boolean;
  className?: string;
  /** Override which level drives `listening` (Settings mic preview uses the monitor level). */
  title?: string;
}

interface Params {
  scale: number;
  eye: number;
  pupil: number;
  mouth: number;
  smile: number;
  /** ^ ^ closed happy eyes */
  happy: number;
  sad: number;
  glow: number;
  dim: number;
  bob: number;
  sway: number;
  lookX: number;
  lookY: number;
  core: number;
  shimmer: number;
}

const REST: Params = {
  scale: 1,
  eye: 1,
  pupil: 1,
  mouth: 0,
  smile: 0,
  happy: 0,
  sad: 0,
  glow: 0.55,
  dim: 1,
  bob: 1,
  sway: 0,
  lookX: 0,
  lookY: 0,
  core: 0.5,
  shimmer: 0,
};

/** viewBox: body occupies x 20..80; aura and particles may spill past the box (overflow visible). */
export const VIEW_MIN = -5;
export const VIEW_SIZE = 110;

/** Ghost-droplet outline; `phase` flutters the soft hem. */
function bodyPath(phase: number, flutter: number): string {
  const hemY = 76;
  const w = 56 / 3;
  let d = "M 20 47 A 30 30 0 0 1 80 47 C 80 60 79.5 68 78 " + hemY;
  for (let i = 0; i < 3; i++) {
    const x0 = 78 - i * w;
    const x1 = x0 - w;
    const bulge = 6.5 + Math.sin(phase + i * 2.1) * 2.2 * flutter;
    d += ` C ${(x0 - w * 0.12).toFixed(2)} ${(hemY + bulge).toFixed(2)} ${(x1 + w * 0.12).toFixed(2)} ${(hemY + bulge).toFixed(2)} ${x1.toFixed(2)} ${hemY}`;
  }
  return d + " C 20.5 68 20 60 20 47 Z";
}

function targetsFor(state: SpiritState, t: number, mic: number, voice: number, micOn: boolean): Params {
  switch (state) {
    case "listening": {
      const m = micOn ? Math.min(1, mic * 1.6) : 0.35 + 0.15 * Math.sin(t * 3);
      return { ...REST, scale: 1.1 + m * 0.06, pupil: 1.22, glow: 0.7 + m * 0.8, bob: 0.4, lookY: -0.35, core: 0.6 + m * 0.4 };
    }
    case "thinking":
      return {
        ...REST,
        eye: 0.42,
        pupil: 0.92,
        glow: 0.6 + 0.12 * Math.sin(t * 2.4),
        bob: 0.7,
        lookX: Math.sin(t * 0.7) * 0.7,
        lookY: -0.6,
        core: 0.5 + 0.2 * Math.sin(t * 3),
        shimmer: 1,
      };
    case "speaking": {
      const v = Math.min(1, voice * 1.5);
      return { ...REST, scale: 1.04 + v * 0.03, mouth: 0.15 + v, glow: 0.6 + v * 0.8, bob: 0.6, core: 0.55 + v * 0.35 };
    }
    case "success":
      return { ...REST, scale: 1.06, smile: 1, happy: 1, glow: 0.95, core: 0.8 };
    case "error":
      return { ...REST, scale: 0.88, eye: 0.72, sad: 1, glow: 0.22, dim: 0.72, bob: 0.3, lookY: 0.3, core: 0.25 };
    case "music":
      return { ...REST, sway: 1, bob: 0.6, eye: 0.88, smile: 0.25, glow: 0.62 };
    default:
      return REST;
  }
}

export default function Spirit({ state, color, size, paused = false, className, title }: Props) {
  const anim = useAnim();
  useLevels();
  const uid = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const px = size * anim.cfg.spiritSize;
  // Body spans 60 of the 110 viewBox units; the rest is room for aura / particles.
  const box = (px * VIEW_SIZE) / 60;
  // Particles read as noise on tiny spirits (the collapsed pill).
  const particles = anim.on("particles") && px >= 28;

  const refs = {
    root: useRef<SVGGElement>(null),
    body: useRef<SVGPathElement>(null),
    shimmer: useRef<SVGPathElement>(null),
    aura: useRef<SVGCircleElement>(null),
    core: useRef<SVGEllipseElement>(null),
    eyes: useRef<SVGGElement>(null),
    eyeL: useRef<SVGGElement>(null),
    eyeR: useRef<SVGGElement>(null),
    happy: useRef<SVGGElement>(null),
    mouth: useRef<SVGEllipseElement>(null),
    smile: useRef<SVGPathElement>(null),
    frown: useRef<SVGPathElement>(null),
    nubL: useRef<SVGEllipseElement>(null),
    nubR: useRef<SVGEllipseElement>(null),
    shimmerGrad: useRef<SVGLinearGradientElement>(null),
  };
  const canvas = useRef<HTMLCanvasElement>(null);
  const field = useRef(new ParticleField());

  // Live inputs for the loop (read through a ref so the loop never restarts on prop changes).
  const live = useRef({ state, anim, paused });
  live.current = { state, anim, paused };
  const enteredAt = useRef(performance.now());
  const prevState = useRef(state);
  const kick = useRef<() => void>(() => undefined);

  useEffect(() => {
    if (prevState.current !== state) {
      prevState.current = state;
      enteredAt.current = performance.now();
      const a = live.current.anim;
      if (state === "success" && a.on("effects") && particlesRef.current) field.current.burst(4);
    }
    kick.current();
  }, [state]);

  useEffect(() => {
    field.current.setColor(hexToRgbTriplet(color));
  }, [color]);

  useEffect(() => {
    field.current.attach(particles ? canvas.current : null, box);
    if (!particles) field.current.clear();
  }, [box, particles]);
  const particlesRef = useRef(particles);
  particlesRef.current = particles;

  useEffect(() => {
    const cur: Params = { ...REST };
    let raf = 0;
    let last = performance.now();
    let lastDraw = 0;
    const t0 = performance.now();
    let nextBlink = t0 + 2500 + Math.random() * 3000;
    let blinkAt = -1;
    let nextLook = t0 + 4000 + Math.random() * 4000;
    let lookTarget = 0;
    let lookUntil = 0;

    const frame = (now: number) => {
      const { state: st, anim: a, paused: p } = live.current;
      const off = a.level === "off";
      const minFrame = 1000 / a.fps - 2;
      if (!off && now - lastDraw < minFrame) {
        raf = requestAnimationFrame(frame);
        return;
      }
      lastDraw = now;
      const dt = Math.min(0.05, (now - last) / 1000);
      last = now;
      const speed = a.cfg.anim.speed;
      const t = ((now - t0) / 1000) * speed;
      const idleAnims = a.on("spiritIdle");
      const effects = a.on("effects");

      const target = targetsFor(st, t, readLevel("mic"), readLevel("voice"), a.on("spiritMic"));
      if (!a.on("musicSway")) target.sway = 0;
      if (!idleAnims) {
        target.bob = 0;
      }

      // Blinks + glances (idle personality).
      let blink = 0;
      if (idleAnims && !off && st !== "thinking" && st !== "success" && st !== "error") {
        if (now >= nextBlink && blinkAt < 0) blinkAt = now;
        if (blinkAt >= 0) {
          const k = (now - blinkAt) / 150;
          blink = k < 1 ? Math.sin(k * Math.PI) : 0;
          if (k >= 1) {
            blinkAt = -1;
            // Occasional double blink.
            nextBlink = now + (Math.random() < 0.18 ? 180 : 2600 + Math.random() * 3800);
          }
        }
        if (st === "idle" || st === "music") {
          if (now >= nextLook) {
            lookTarget = (Math.random() < 0.5 ? -1 : 1) * (0.5 + Math.random() * 0.5);
            lookUntil = now + 900 + Math.random() * 900;
            nextLook = now + 3500 + Math.random() * 5000;
          }
          if (now < lookUntil) target.lookX = lookTarget;
        }
      }

      const ease = off ? 1 : 1 - Math.exp(-dt * 11 * speed);
      (Object.keys(cur) as (keyof Params)[]).forEach((k) => {
        cur[k] += (target[k] - cur[k]) * ease;
      });

      // One-shot effects measured from state entry.
      const since = (now - enteredAt.current) / 1000;
      let jumpY = 0;
      let shakeX = 0;
      if (effects && !off) {
        if (st === "success" && since < 1) jumpY = -9 * Math.exp(-since * 4) * Math.abs(Math.sin(since * 11));
        if (st === "error" && since < 0.6) shakeX = Math.sin(since * 46) * 3.2 * Math.exp(-since * 6);
      }

      const breathe = idleAnims && !off ? Math.sin(t * 1.6) * 0.025 : 0;
      const bobY = off ? 0 : Math.sin(t * 1.25) * 1.8 * cur.bob;
      const swayDeg = off ? 0 : Math.sin(t * Math.PI * 0.9) * 7 * cur.sway;
      const s = cur.scale * (1 + breathe);
      const flutter = off || !idleAnims ? 0 : 1;

      const r = refs;
      r.root.current?.setAttribute(
        "transform",
        `translate(${(shakeX).toFixed(2)} ${(bobY + jumpY).toFixed(2)}) rotate(${swayDeg.toFixed(2)} 50 70) translate(50 56) scale(${s.toFixed(4)}) translate(-50 -56)`,
      );
      r.body.current?.setAttribute("d", bodyPath(t * 2.2, flutter));
      r.shimmer.current?.setAttribute("d", bodyPath(t * 2.2, flutter));
      r.root.current?.setAttribute("opacity", cur.dim.toFixed(3));

      const glow = Math.max(0, cur.glow * a.cfg.glow);
      r.aura.current?.setAttribute("opacity", Math.min(1, glow).toFixed(3));
      r.aura.current?.setAttribute("r", (40 + glow * 8).toFixed(2));
      r.core.current?.setAttribute("opacity", Math.min(1, cur.core).toFixed(3));

      const open = Math.max(0.06, cur.eye * (1 - blink * 0.94));
      const ps = cur.pupil;
      const lx = cur.lookX * 3;
      const ly = cur.lookY * 2;
      const tilt = cur.sad * 14;
      r.eyeL.current?.setAttribute(
        "transform",
        `translate(${(39 + lx).toFixed(2)} ${(50 + ly + cur.sad).toFixed(2)}) rotate(${(-tilt).toFixed(2)}) scale(${ps.toFixed(3)} ${(ps * open).toFixed(3)})`,
      );
      r.eyeR.current?.setAttribute(
        "transform",
        `translate(${(61 + lx).toFixed(2)} ${(50 + ly + cur.sad).toFixed(2)}) rotate(${tilt.toFixed(2)}) scale(${ps.toFixed(3)} ${(ps * open).toFixed(3)})`,
      );
      r.eyes.current?.setAttribute("opacity", (1 - cur.happy).toFixed(3));
      r.happy.current?.setAttribute("opacity", cur.happy.toFixed(3));
      r.happy.current?.setAttribute("transform", `translate(${lx.toFixed(2)} ${ly.toFixed(2)})`);

      const mouthOpen = Math.max(0, cur.mouth);
      r.mouth.current?.setAttribute("ry", (0.6 + mouthOpen * 3).toFixed(2));
      r.mouth.current?.setAttribute("rx", (2.2 + mouthOpen * 0.8).toFixed(2));
      r.mouth.current?.setAttribute("opacity", Math.min(1, mouthOpen * 4).toFixed(3));
      r.smile.current?.setAttribute("opacity", (cur.smile * 0.9 * (1 - Math.min(1, mouthOpen * 3))).toFixed(3));
      r.frown.current?.setAttribute("opacity", (cur.sad * 0.85).toFixed(3));

      const nub = off ? 0 : Math.sin(t * (st === "listening" || st === "success" ? 7 : 2)) * (st === "success" ? 18 : 6);
      r.nubL.current?.setAttribute("transform", `rotate(${(-20 + nub).toFixed(2)} 20 60)`);
      r.nubR.current?.setAttribute("transform", `rotate(${(20 - nub).toFixed(2)} 80 60)`);

      r.shimmer.current?.setAttribute("opacity", (cur.shimmer * 0.55).toFixed(3));
      r.shimmerGrad.current?.setAttribute(
        "gradientTransform",
        `translate(${(((t * 0.6) % 2) - 1).toFixed(3)} 0)`,
      );

      const f = field.current;
      if (particlesRef.current) {
        f.set("swirl", st === "thinking");
        f.set("eq", st === "music" && a.on("equalizer"));
        f.set("motes", st === "idle" && idleAnims && a.level === "full");
        f.step(dt * speed, t);
      }

      const settled =
        off ||
        p ||
        (!idleAnims && st === "idle" && f.idle && Math.abs(cur.scale - target.scale) < 0.001);
      if (settled) {
        raf = 0;
        return;
      }
      raf = requestAnimationFrame(frame);
    };

    kick.current = () => {
      if (raf === 0) {
        last = performance.now();
        raf = requestAnimationFrame(frame);
      }
    };
    raf = requestAnimationFrame(frame);
    return () => {
      cancelAnimationFrame(raf);
      raf = 0;
      kick.current = () => undefined;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Paused / config changes: render at least one frame, resume if needed.
  useEffect(() => {
    kick.current();
  }, [paused, anim]);

  const colorTransition = anim.level === "off" ? "none" : `stop-color ${anim.dur(450)}s ease`;
  const stop = (offset: string, opacity: number, mixWhite = false) => (
    <stop
      offset={offset}
      style={{
        stopColor: mixWhite ? `color-mix(in srgb, ${color} 35%, white)` : color,
        stopOpacity: opacity,
        transition: colorTransition,
      }}
    />
  );

  return (
    <div
      className={`relative shrink-0 ${className ?? ""}`}
      style={{ width: box, height: box, marginInline: -(box - px) * 0.3 }}
      role="img"
      aria-label={title ?? `Soul: ${state}`}
    >
      {particles && (
        <canvas ref={canvas} className="pointer-events-none absolute inset-0" style={{ width: box, height: box }} />
      )}
      <svg viewBox={`${VIEW_MIN} ${VIEW_MIN} ${VIEW_SIZE} ${VIEW_SIZE}`} width={box} height={box} className="absolute inset-0 overflow-visible">
        <defs>
          <radialGradient id={`aura${uid}`}>
            {stop("0%", 0.55)}
            {stop("55%", 0.16)}
            {stop("100%", 0)}
          </radialGradient>
          <radialGradient id={`body${uid}`} cx="42%" cy="32%" r="75%">
            {stop("0%", 0.98, true)}
            {stop("50%", 0.78)}
            {stop("100%", 0.32)}
          </radialGradient>
          <radialGradient id={`core${uid}`}>
            <stop offset="0%" stopColor="#ffffff" stopOpacity={0.9} />
            <stop offset="100%" stopColor="#ffffff" stopOpacity={0} />
          </radialGradient>
          <linearGradient id={`shim${uid}`} ref={refs.shimmerGrad} x1="0" x2="1" y1="0" y2="0.4">
            <stop offset="0.35" stopColor="#fff" stopOpacity={0} />
            <stop offset="0.5" stopColor="#fff" stopOpacity={0.7} />
            <stop offset="0.65" stopColor="#fff" stopOpacity={0} />
          </linearGradient>
        </defs>

        <circle ref={refs.aura} cx="50" cy="52" r="46" fill={`url(#aura${uid})`} />

        <g ref={refs.root}>
          <ellipse ref={refs.nubL} cx="20" cy="60" rx="4" ry="2.6" fill={`url(#body${uid})`} />
          <ellipse ref={refs.nubR} cx="80" cy="60" rx="4" ry="2.6" fill={`url(#body${uid})`} />
          <path
            ref={refs.body}
            d={bodyPath(0, 0)}
            fill={`url(#body${uid})`}
            stroke="rgba(255,255,255,0.28)"
            strokeWidth="0.8"
          />
          <path ref={refs.shimmer} d={bodyPath(0, 0)} fill={`url(#shim${uid})`} opacity={0} />
          <ellipse ref={refs.core} cx="50" cy="55" rx="15" ry="16" fill={`url(#core${uid})`} opacity={0.5} />
          {/* gloss */}
          <ellipse cx="38" cy="30" rx="8" ry="4.5" fill="#fff" opacity={0.35} transform="rotate(-25 38 30)" />

          <g ref={refs.eyes}>
            <g ref={refs.eyeL} transform="translate(39 50)">
              <ellipse rx="4.1" ry="5.6" fill="#0c1b1a" />
              <circle cx="1.3" cy="-2.1" r="1.35" fill="#fff" opacity={0.95} />
            </g>
            <g ref={refs.eyeR} transform="translate(61 50)">
              <ellipse rx="4.1" ry="5.6" fill="#0c1b1a" />
              <circle cx="1.3" cy="-2.1" r="1.35" fill="#fff" opacity={0.95} />
            </g>
          </g>
          {/* happy closed eyes ^ ^ */}
          <g ref={refs.happy} opacity={0} fill="none" stroke="#0c1b1a" strokeWidth="2.4" strokeLinecap="round">
            <path d="M 34.5 51.5 Q 39 45 43.5 51.5" />
            <path d="M 56.5 51.5 Q 61 45 65.5 51.5" />
          </g>

          <ellipse ref={refs.mouth} cx="50" cy="61.5" rx="2.2" ry="0.6" fill="#0c1b1a" opacity={0} />
          <path
            ref={refs.smile}
            d="M 45.5 59.5 Q 50 64 54.5 59.5"
            fill="none"
            stroke="#0c1b1a"
            strokeWidth="1.8"
            strokeLinecap="round"
            opacity={0}
          />
          <path
            ref={refs.frown}
            d="M 46 63.5 Q 50 60 54 63.5"
            fill="none"
            stroke="#0c1b1a"
            strokeWidth="1.6"
            strokeLinecap="round"
            opacity={0}
          />
        </g>
      </svg>
    </div>
  );
}

/** Fallback when the spirit is turned off: a calm status dot in the same colour. */
export function StatusDot({ state, color }: { state: SpiritState; color: string }) {
  const anim = useAnim();
  const pulse = anim.level !== "off" && (state === "listening" || state === "thinking" || state === "speaking");
  return (
    <span className="relative inline-flex h-2.5 w-2.5 shrink-0 items-center justify-center" aria-label={`Soul: ${state}`}>
      {pulse && (
        <span
          className="absolute inset-0 rounded-full soul-dot-pulse"
          style={{ background: color, animationDuration: `${anim.dur(1400)}s` }}
        />
      )}
      <span
        className="relative h-2.5 w-2.5 rounded-full"
        style={{ background: color, boxShadow: `0 0 8px ${color}`, transition: `background ${anim.dur(300)}s` }}
      />
    </span>
  );
}

/** Spirit when enabled, StatusDot otherwise. */
export function SpiritOrDot(props: Props) {
  const anim = useAnim();
  if (!anim.on("spirit")) return <StatusDot state={props.state} color={props.color} />;
  return <Spirit {...props} />;
}
