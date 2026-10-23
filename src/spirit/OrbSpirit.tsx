import { useEffect, useRef } from "react";
import { useAnim } from "../anim/AnimationProvider";
import { readLevel, useLevels } from "../lib/levels";
import { hexToRgbTriplet } from "../theme/tokens";
import type { SpiritState } from "./Spirit";

/**
 * 3D particle orb: one WebGL point draw call per frame. Particles morph between four formations
 * (sphere with voice ripples, trefoil knot, sonar rings, ring + checkmark) with a per-particle
 * stagger; colour eases to the `color` prop. Same footprint and props as the droplet spirit.
 */
interface Props {
  state: SpiritState;
  color: string;
  size: number;
  paused?: boolean;
  className?: string;
  title?: string;
  /** No WebGL here: the caller falls back to the droplet. */
  onUnsupported?: () => void;
}

const TAU = Math.PI * 2;
const clamp01 = (k: number) => Math.min(1, Math.max(0, k));
const lerp = (a: number, b: number, k: number) => a + (b - a) * k;
const ease = (k: number) => (k < 0.5 ? 4 * k * k * k : 1 - Math.pow(-2 * k + 2, 3) / 2);
/** Seconds (speed-scaled) a formation change takes, before the per-particle stagger. */
const TRANSITION = 0.9;

const VERT = `
attribute vec3 aPos; attribute vec3 aCol;
uniform vec2 uRes; uniform vec2 uC; uniform float uR; uniform float uPt; uniform float uGain;
varying vec3 vCol;
void main(){
  float f = 3.4; float s = f / (f - aPos.z);
  vec2 p = uC + vec2(aPos.x, -aPos.y) * uR * s;
  vec2 c = p / uRes * 2.0 - 1.0; c.y = -c.y;
  gl_Position = vec4(c, 0.0, 1.0);
  gl_PointSize = uPt * s;
  vCol = aCol * (0.5 + 0.5 * s) * uGain;
}`;

const FRAG = `
precision mediump float;
varying vec3 vCol;
void main(){
  float r = length(gl_PointCoord - 0.5);
  float a = smoothstep(0.5, 0.08, r);
  vec3 c = vCol * a;
  gl_FragColor = vec4(c, max(c.r, max(c.g, c.b)));
}`;

/** Formation per Soul state: 0 sphere, 1 trefoil knot, 2 sonar, 3 ring + checkmark. */
function formationOf(state: SpiritState): number {
  return state === "thinking" ? 1 : state === "music" ? 2 : state === "success" ? 3 : 0;
}

function rgbOf(hex: string): [number, number, number] {
  const [r, g, b] = hexToRgbTriplet(hex).split(" ").map(Number);
  return [r / 255, g / 255, b / 255];
}

/** Rotate around y by `a`, then around x by `b`. */
function rot(v: Float32Array | number[], a: number, b: number) {
  const ca = Math.cos(a), sa = Math.sin(a);
  const x = v[0] * ca + v[2] * sa;
  let z = -v[0] * sa + v[2] * ca;
  const cb = Math.cos(b), sb = Math.sin(b);
  const y = v[1] * cb - z * sb;
  z = v[1] * sb + z * cb;
  v[0] = x;
  v[1] = y;
  v[2] = z;
}

const CA = [-0.38, 0.02], CB = [-0.11, -0.26], CC = [0.42, 0.3];
const L1 = Math.hypot(CB[0] - CA[0], CB[1] - CA[1]);
const L2 = Math.hypot(CC[0] - CB[0], CC[1] - CB[1]);
const LT = L1 + L2;

/** Deterministic particle tables for `n` particles. */
function makeEngine(n: number) {
  let seed = 0x50a1;
  const rnd = () => (seed = (seed * 1664525 + 1013904223) >>> 0) / 4294967296;
  const S = new Float32Array(n * 4);
  for (let i = 0; i < S.length; i++) S[i] = rnd();
  const SPH = new Float32Array(n * 3);
  const GA = Math.PI * (3 - Math.sqrt(5));
  for (let i = 0; i < n; i++) {
    const y = 1 - ((i + 0.5) / n) * 2;
    const r = Math.sqrt(1 - y * y);
    const t = GA * i;
    SPH[i * 3] = Math.cos(t) * r;
    SPH[i * 3 + 1] = y;
    SPH[i * 3 + 2] = Math.sin(t) * r;
  }
  const PERM = new Uint32Array(n);
  for (let i = 0; i < n; i++) PERM[i] = i;
  for (let i = n - 1; i > 0; i--) {
    const j = Math.floor(rnd() * (i + 1));
    [PERM[i], PERM[j]] = [PERM[j], PERM[i]];
  }
  const PERM2 = new Uint32Array(n);
  for (let i = 0; i < n; i++) PERM2[i] = PERM[(i * 7919) % n];

  /** Writes x, y, z, brightness of particle `i` in formation `st` into `v`. */
  const shape = (st: number, i: number, t: number, voice: number, v: Float32Array) => {
    const s1 = S[i * 4], s2 = S[i * 4 + 1], s3 = S[i * 4 + 2], s4 = S[i * 4 + 3];
    if (st === 0) {
      const j = PERM[i] * 3;
      const dx = SPH[j], dy = SPH[j + 1], dz = SPH[j + 2];
      const lat = Math.asin(dy);
      const lon = Math.atan2(dz, dx);
      const rip = Math.sin(lat * 6 - t * 3.2) * (0.03 + 0.07 * voice) + Math.sin(lon * 4 + t * 1.2) * 0.015;
      const R = (0.8 + rip) * Math.pow(s3, 0.12);
      v[0] = dx * R;
      v[1] = dy * R;
      v[2] = dz * R;
      rot(v, t * 0.22, 0.3);
      v[3] = 0.28 + Math.max(0, rip) * 4 + 0.18 * s4;
    } else if (st === 1) {
      const u = (s1 + t * 0.04) * TAU;
      const r = Math.cos(3 * u) + 2;
      const j = PERM2[i] * 3;
      const tube = 0.09 * Math.sqrt(s2);
      v[0] = r * Math.cos(2 * u) * 0.29 + SPH[j] * tube;
      v[1] = r * Math.sin(2 * u) * 0.29 + SPH[j + 1] * tube;
      v[2] = -Math.sin(3 * u) * 0.29 + SPH[j + 2] * tube;
      rot(v, t * 0.34, 0.55 + Math.sin(t * 0.3) * 0.35);
      v[3] = 0.26 + 0.4 * s4;
    } else if (st === 2) {
      const rho = s1 < 0.7 ? ((Math.floor(s2 * 5) + 1) / 5) * 0.92 + (s3 - 0.5) * 0.016 : Math.sqrt(s2) * 0.92;
      const ang = s4 * TAU;
      const dd = (((t * 2.2 - ang) % TAU) + TAU) % TAU;
      const beam = Math.exp(-dd * 1.6);
      v[0] = rho * Math.cos(ang);
      v[2] = rho * Math.sin(ang);
      v[1] = beam * 0.06 * (1 - rho * 0.5) + (s3 - 0.5) * 0.012;
      rot(v, 0.14 * t, -0.8);
      v[3] = (s1 < 0.7 ? 0.2 : 0.08) + 1.25 * beam;
    } else {
      const b = 1 + 0.015 * Math.sin(t * 1.6);
      if (s1 < 0.5) {
        const rho = 0.86 + (s2 - 0.5) * 0.045;
        const ang = s3 * TAU;
        v[0] = rho * Math.cos(ang) * b;
        v[1] = rho * Math.sin(ang) * b;
        v[2] = (s4 - 0.5) * 0.04;
        v[3] = 0.3 + 0.2 * s4;
      } else {
        const d = s2 * LT;
        const [a, c, len, k] = d < L1 ? [CA, CB, L1, d / L1] : [CB, CC, L2, (d - L1) / L2];
        const nx = -(c[1] - a[1]) / len, ny = (c[0] - a[0]) / len;
        const off = (s3 - 0.5) * 0.07;
        v[0] = (lerp(a[0], c[0], k) + nx * off) * b;
        v[1] = (lerp(a[1], c[1], k) + ny * off) * b;
        v[2] = (s4 - 0.5) * 0.06;
        v[3] = 0.55 + 0.3 * s4;
      }
      rot(v, Math.sin(t * 0.6) * 0.2, 0);
    }
  };

  /** Stagger (s) before particle `i` starts moving into formation `st`; the checkmark draws in. */
  const delayOf = (st: number, i: number) => {
    const s1 = S[i * 4], s2 = S[i * 4 + 1], s3 = S[i * 4 + 2];
    return st === 3 ? (s1 < 0.5 ? s3 * 0.35 : 0.3 + s2 * 0.4) : s1 * 0.22;
  };

  return { S, SPH, PERM2, shape, delayOf };
}

function compile(gl: WebGLRenderingContext): WebGLProgram | null {
  const prog = gl.createProgram();
  if (!prog) return null;
  for (const [type, src] of [[gl.VERTEX_SHADER, VERT], [gl.FRAGMENT_SHADER, FRAG]] as const) {
    const sh = gl.createShader(type);
    if (!sh) return null;
    gl.shaderSource(sh, src);
    gl.compileShader(sh);
    if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) return null;
    gl.attachShader(prog, sh);
  }
  gl.linkProgram(prog);
  return gl.getProgramParameter(prog, gl.LINK_STATUS) ? prog : null;
}

export default function OrbSpirit({ state, color, size, paused = false, className, title, onUnsupported }: Props) {
  const anim = useAnim();
  useLevels();
  const px = size * anim.cfg.spiritSize;
  // Same footprint as the droplet: body spans 60 of the 110-unit box.
  const box = (px * 110) / 60;
  const count = px < 28 ? 2400 : px < 44 ? 5000 : 10000;
  const pointSize = px < 28 ? 0.022 : 0.012;
  const canvas = useRef<HTMLCanvasElement>(null);

  const live = useRef({ state, color, anim, paused });
  live.current = { state, color, anim, paused };
  const kick = useRef<() => void>(() => undefined);
  const failed = useRef(onUnsupported);
  failed.current = onUnsupported;

  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    const gl = el.getContext("webgl", { alpha: true, premultipliedAlpha: true, antialias: false });
    const prog = gl && compile(gl);
    if (!gl || !prog) {
      failed.current?.();
      return;
    }
    const dpr = window.devicePixelRatio || 1;
    el.width = Math.max(1, Math.round(box * dpr));
    el.height = el.width;
    gl.viewport(0, 0, el.width, el.height);
    gl.useProgram(prog);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE);
    gl.clearColor(0, 0, 0, 0);

    const N = count;
    const eng = makeEngine(N);
    const pos = new Float32Array(N * 3);
    const col = new Float32Array(N * 3);
    const fromP = new Float32Array(N * 3);
    const bind = (name: string, data: Float32Array) => {
      const buf = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, buf);
      gl.bufferData(gl.ARRAY_BUFFER, data.byteLength, gl.DYNAMIC_DRAW);
      const loc = gl.getAttribLocation(prog, name);
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, 3, gl.FLOAT, false, 0, 0);
      return buf;
    };
    const posBuf = bind("aPos", pos);
    const colBuf = bind("aCol", col);
    const u = (name: string) => gl.getUniformLocation(prog, name);
    const uRes = u("uRes"), uC = u("uC"), uR = u("uR"), uPt = u("uPt"), uGain = u("uGain");
    gl.uniform2f(uRes, el.width, el.height);
    gl.uniform1f(uR, px * dpr * 0.5);
    gl.uniform1f(uPt, Math.max(1, pointSize * el.width));

    const v = new Float32Array(4);
    const t0 = performance.now();
    let st = formationOf(live.current.state);
    let shown = live.current.state;
    let T0 = -1e9; // last formation change (orb time)
    let enteredAt = t0;
    let rgbFrom = rgbOf(live.current.color);
    let rgbTo = rgbFrom;
    let shownColor = live.current.color;
    let colorAt = -1e9;
    let voice = 0.08;
    let raf = 0;
    let lastDraw = 0;
    let last = t0;
    let t = 0;
    let visible = true;
    let lost = false;

    const frame = (now: number) => {
      raf = 0;
      const { state: s, color: c, anim: a, paused: p } = live.current;
      const off = a.level === "off";
      if (!off && now - lastDraw < 1000 / a.fps - 2) {
        raf = requestAnimationFrame(frame);
        return;
      }
      lastDraw = now;
      const dt = Math.min(0.05, (now - last) / 1000);
      last = now;
      if (!off) t += dt * a.cfg.anim.speed;

      if (s !== shown) {
        shown = s;
        enteredAt = now;
        const next = formationOf(s);
        if (next !== st) {
          fromP.set(pos);
          st = next;
          T0 = t;
        }
      }
      const rgbNow = (k: number) => rgbFrom.map((x, i) => lerp(x, rgbTo[i], k)) as [number, number, number];
      if (c !== shownColor) {
        rgbFrom = rgbNow(ease(clamp01((t - colorAt) / 0.6)));
        rgbTo = rgbOf(c);
        shownColor = c;
        colorAt = t;
      }
      const rgb = rgbNow(off ? 1 : ease(clamp01((t - colorAt) / 0.6)));

      const target =
        s === "listening"
          ? a.on("spiritMic") ? Math.min(1, readLevel("mic") * 1.8) : 0.3
          : s === "speaking"
            ? Math.min(1, readLevel("voice") * 1.6)
            : s === "error"
              ? 0.6
              : 0.08;
      voice += (target - voice) * (off ? 1 : 1 - Math.exp(-dt * 12));

      const tt = t - T0;
      const inT = !off && tt < TRANSITION + 0.75;
      for (let i = 0; i < N; i++) {
        eng.shape(st, i, t, voice, v);
        const j = i * 3;
        if (inT) {
          const l = ease(clamp01((tt - eng.delayOf(st, i)) / TRANSITION));
          const k = eng.PERM2[(i + 17) % N] * 3;
          const burst = Math.sin(Math.PI * l) * 0.24 * (0.4 + eng.S[i * 4 + 1]);
          pos[j] = lerp(fromP[j], v[0], l) + eng.SPH[k] * burst;
          pos[j + 1] = lerp(fromP[j + 1], v[1], l) + eng.SPH[k + 1] * burst;
          pos[j + 2] = lerp(fromP[j + 2], v[2], l) + eng.SPH[k + 2] * burst;
        } else {
          pos[j] = v[0];
          pos[j + 1] = v[1];
          pos[j + 2] = v[2];
        }
        col[j] = rgb[0] * v[3];
        col[j + 1] = rgb[1] * v[3];
        col[j + 2] = rgb[2] * v[3];
      }

      // Error: a quick shake.
      const since = (now - enteredAt) / 1000;
      const shake = s === "error" && !off && a.on("effects") && since < 0.6 ? Math.sin(since * 46) * 0.06 * Math.exp(-since * 6) : 0;
      gl.uniform2f(uC, el.width / 2 + shake * el.width, el.height / 2);
      gl.uniform1f(uGain, 0.6 + 0.4 * Math.min(1.5, a.cfg.glow));
      gl.bindBuffer(gl.ARRAY_BUFFER, posBuf);
      gl.bufferSubData(gl.ARRAY_BUFFER, 0, pos);
      gl.bindBuffer(gl.ARRAY_BUFFER, colBuf);
      gl.bufferSubData(gl.ARRAY_BUFFER, 0, col);
      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.drawArrays(gl.POINTS, 0, N);

      if (!off && !p && visible && !document.hidden && !lost) raf = requestAnimationFrame(frame);
    };

    kick.current = () => {
      if (raf === 0 && !lost) {
        last = performance.now();
        raf = requestAnimationFrame(frame);
      }
    };
    const onVisibility = () => kick.current();
    document.addEventListener("visibilitychange", onVisibility);
    const io = new IntersectionObserver((entries) => {
      visible = entries.some((e) => e.isIntersecting);
      if (visible) kick.current();
    });
    io.observe(el);
    const onLost = (e: Event) => {
      e.preventDefault();
      lost = true;
    };
    el.addEventListener("webglcontextlost", onLost);
    raf = requestAnimationFrame(frame);

    return () => {
      cancelAnimationFrame(raf);
      raf = 0;
      kick.current = () => undefined;
      document.removeEventListener("visibilitychange", onVisibility);
      el.removeEventListener("webglcontextlost", onLost);
      io.disconnect();
      gl.deleteBuffer(posBuf);
      gl.deleteBuffer(colBuf);
      gl.deleteProgram(prog);
    };
  }, [box, count, pointSize, px]);

  // State / colour / config changes: draw at least one frame, resume if needed.
  useEffect(() => {
    kick.current();
  }, [state, color, paused, anim]);

  return (
    <div
      className={`relative shrink-0 ${className ?? ""}`}
      style={{ width: box, height: box, marginInline: -(box - px) * 0.3 }}
      role="img"
      aria-label={title ?? `Soul: ${state}`}
    >
      <canvas ref={canvas} className="pointer-events-none absolute inset-0" style={{ width: box, height: box }} />
    </div>
  );
}
