import { useEffect, useRef, type ReactNode } from "react";
import { motion } from "framer-motion";
import { useAnim } from "../anim/AnimationProvider";
import { readLevel, useLevels } from "../lib/levels";
import { monotonic, type ProgressAnchor } from "./progress";

/** Live waveform from mic or TTS level refs — transform-only bar updates in one rAF loop. */
export function Waveform({
  source,
  color,
  bars = 18,
  height = 22,
  className,
}: {
  source: "mic" | "voice";
  color: string;
  bars?: number;
  height?: number;
  className?: string;
}) {
  const anim = useAnim();
  useLevels();
  const els = useRef<Array<HTMLElement | null>>([]);
  const enabled = anim.on("waveform");

  useEffect(() => {
    if (!enabled) return;
    let raf = 0;
    let smooth = 0;
    let lastDraw = 0;
    const hist: number[] = new Array(bars).fill(0);
    const tick = (now: number) => {
      raf = requestAnimationFrame(tick);
      if (now - lastDraw < 1000 / anim.fps - 2) return;
      lastDraw = now;
      const lvl = readLevel(source);
      smooth += (lvl - smooth) * 0.35;
      // Scroll history: newest in the middle, mirrored outwards — reads as a voice ribbon.
      hist.pop();
      hist.unshift(smooth);
      const half = bars / 2;
      for (let i = 0; i < bars; i++) {
        const el = els.current[i];
        if (!el) continue;
        const d = Math.floor(Math.abs(i - half + 0.5));
        const v = hist[Math.min(d * 2, bars - 1)] ?? 0;
        const wobble = 0.75 + 0.25 * Math.sin(now / 140 + i * 0.9);
        const s = Math.max(0.12, Math.min(1, v * 2.2 * wobble + 0.08));
        el.style.transform = `scaleY(${s.toFixed(3)})`;
      }
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [enabled, bars, source, anim.fps]);

  if (!enabled) return null;
  return (
    <span className={`flex items-center gap-[3px] ${className ?? ""}`} style={{ height }} aria-hidden>
      {Array.from({ length: bars }, (_, i) => (
        <i
          key={i}
          ref={(el) => {
            els.current[i] = el;
          }}
          className="block w-[3px] rounded-full"
          style={{
            height,
            background: color,
            opacity: 0.55 + 0.45 * (1 - Math.abs(i - bars / 2) / (bars / 2)),
            transform: "scaleY(0.12)",
            transition: `background ${anim.dur(400)}s`,
          }}
        />
      ))}
    </span>
  );
}

export function EqBars({ playing, color }: { playing: boolean; color: string }) {
  const anim = useAnim();
  if (!anim.on("equalizer")) return null;
  return (
    <span className="island-eq" data-playing={playing} aria-hidden style={{ ["--eq" as string]: color }}>
      <i />
      <i />
      <i />
      <i />
    </span>
  );
}

export function Marquee({ text, className }: { text: string; className?: string }) {
  const anim = useAnim();
  const long = text.length > 22;
  if (!long || !anim.on("marquee")) {
    return <span className={`block min-w-0 truncate ${className ?? ""}`}>{text}</span>;
  }
  const secs = Math.max(8, text.length * 0.32) / anim.cfg.anim.speed;
  return (
    <div className={`island-marquee min-w-0 ${className ?? ""}`}>
      <span className="island-marquee-inner" style={{ animationDuration: `${secs}s` }}>
        {text}
        <span className="px-6 opacity-40">•</span>
        {text}
        <span className="px-6 opacity-40">•</span>
      </span>
    </div>
  );
}

/**
 * Playback progress driven by the interpolated anchor (`progress.ts`): rAF writes the bar
 * transform; `monotonic` guarantees it never moves backwards except on seek / track change.
 */
export function ProgressBar({
  anchor,
  durationMs,
  fallbackMs,
  playing,
  color,
  onTime,
}: {
  anchor: ProgressAnchor | null;
  durationMs: number;
  fallbackMs: number;
  playing: boolean;
  color: string;
  onTime?: (ms: number) => void;
}) {
  const fill = useRef<HTMLSpanElement>(null);
  const last = useRef({ epoch: 0, ms: 0 });
  const onTimeRef = useRef(onTime);
  onTimeRef.current = onTime;

  useEffect(() => {
    let raf = 0;
    let lastSec = -1;
    const draw = () => {
      const ms = anchor ? monotonic(last.current, anchor, Date.now()) : fallbackMs;
      const pct = durationMs > 0 ? Math.min(1, ms / durationMs) : 0;
      if (fill.current) fill.current.style.transform = `scaleX(${pct.toFixed(4)})`;
      const sec = Math.floor(ms / 1000);
      if (sec !== lastSec) {
        lastSec = sec;
        onTimeRef.current?.(ms);
      }
      if (playing) raf = requestAnimationFrame(draw);
    };
    draw();
    return () => cancelAnimationFrame(raf);
  }, [anchor, durationMs, fallbackMs, playing]);

  return (
    <div className="island-progress" aria-hidden>
      <span ref={fill} style={{ background: color }} />
    </div>
  );
}

export function Press({
  children,
  onClick,
  className,
  label,
  style,
  disabled,
}: {
  children: ReactNode;
  onClick?: () => void;
  className?: string;
  label?: string;
  style?: React.CSSProperties;
  disabled?: boolean;
}) {
  const anim = useAnim();
  const micro = anim.on("micro");
  return (
    <motion.button
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={(e) => {
        e.stopPropagation();
        onClick?.();
      }}
      whileHover={micro ? { y: -1 } : undefined}
      whileTap={micro ? { scale: 0.97 } : undefined}
      transition={anim.spring("ui")}
      className={className}
      style={style}
    >
      {children}
    </motion.button>
  );
}

export function PrevIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
      <path d="M6 6h2v12H6V6zm3.5 6 8.5 6V6l-8.5 6z" />
    </svg>
  );
}
export function NextIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
      <path d="M16 6h2v12h-2V6zM6 18l8.5-6L6 6v12z" />
    </svg>
  );
}
export function PlayIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
      <path d="M8 5v14l11-7L8 5z" />
    </svg>
  );
}
export function PauseIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
      <path d="M6 5h4v14H6V5zm8 0h4v14h-4V5z" />
    </svg>
  );
}
