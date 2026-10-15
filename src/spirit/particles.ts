/**
 * Canvas particles around the spirit: thinking swirl, success sparkles, music equalizer
 * dots and sparse idle motes. Pure drawing — the Spirit's rAF loop calls `step()`.
 * Coordinates are spirit units (viewBox -5..105, centre 50,50).
 */

export type Emitter = "swirl" | "eq" | "motes";

interface P {
  kind: "swirl" | "spark" | "mote";
  x: number;
  y: number;
  vx: number;
  vy: number;
  a: number; // angle for swirl
  r: number; // orbit radius / size
  life: number;
  max: number;
  seed: number;
}

export class ParticleField {
  private ps: P[] = [];
  private ctx: CanvasRenderingContext2D | null = null;
  private px = 0;
  private dpr = 1;
  /** 0–1 intensity of each continuous emitter (eased in/out). */
  private on: Record<Emitter, number> = { swirl: 0, eq: 0, motes: 0 };
  private want: Record<Emitter, boolean> = { swirl: false, eq: false, motes: false };
  private moteTimer = 0;
  private rgb = "124 242 200";

  attach(canvas: HTMLCanvasElement | null, cssPx: number) {
    if (!canvas) {
      this.ctx = null;
      return;
    }
    this.dpr = Math.min(2, window.devicePixelRatio || 1);
    this.px = cssPx;
    canvas.width = Math.round(cssPx * this.dpr);
    canvas.height = Math.round(cssPx * this.dpr);
    this.ctx = canvas.getContext("2d");
  }

  setColor(rgbTriplet: string) {
    this.rgb = rgbTriplet;
  }

  set(e: Emitter, want: boolean) {
    this.want[e] = want;
    if (e === "swirl" && want && !this.ps.some((p) => p.kind === "swirl")) {
      for (let i = 0; i < 9; i++) {
        this.ps.push({
          kind: "swirl",
          x: 0,
          y: 0,
          vx: 0,
          vy: 0,
          a: (i / 9) * Math.PI * 2,
          r: 36 + (i % 3) * 5,
          life: 0,
          max: Infinity,
          seed: Math.random() * 10,
        });
      }
    }
  }

  burst(n = 4) {
    for (let i = 0; i < n; i++) {
      const a = -Math.PI / 2 + (i - (n - 1) / 2) * 0.75 + (Math.random() - 0.5) * 0.3;
      const sp = 55 + Math.random() * 25;
      this.ps.push({
        kind: "spark",
        x: 50 + Math.cos(a) * 26,
        y: 46 + Math.sin(a) * 26,
        vx: Math.cos(a) * sp,
        vy: Math.sin(a) * sp,
        a: Math.random() * Math.PI,
        r: 3.2 + Math.random() * 1.6,
        life: 0,
        max: 0.9 + Math.random() * 0.25,
        seed: Math.random(),
      });
    }
  }

  get idle(): boolean {
    return this.ps.length === 0 && this.on.swirl < 0.01 && this.on.eq < 0.01;
  }

  clear() {
    this.ps = [];
    this.on = { swirl: 0, eq: 0, motes: 0 };
    this.draw(0);
  }

  step(dt: number, t: number) {
    const ease = 1 - Math.exp(-dt * 6);
    (Object.keys(this.on) as Emitter[]).forEach((k) => {
      this.on[k] += ((this.want[k] ? 1 : 0) - this.on[k]) * ease;
    });

    if (this.want.motes) {
      this.moteTimer -= dt;
      if (this.moteTimer <= 0) {
        this.moteTimer = 1.6 + Math.random() * 2.4;
        this.ps.push({
          kind: "mote",
          x: 30 + Math.random() * 40,
          y: 74,
          vx: (Math.random() - 0.5) * 4,
          vy: -9 - Math.random() * 6,
          a: 0,
          r: 0.9 + Math.random() * 0.8,
          life: 0,
          max: 3 + Math.random() * 1.5,
          seed: Math.random() * 10,
        });
      }
    }

    for (const p of this.ps) {
      p.life += dt;
      if (p.kind === "swirl") {
        p.a += dt * (1.6 + (p.seed % 1) * 0.6);
        p.x = 50 + Math.cos(p.a) * p.r;
        p.y = 50 + Math.sin(p.a) * p.r * 0.62 - 4;
      } else if (p.kind === "spark") {
        p.vx *= Math.exp(-dt * 3.2);
        p.vy = p.vy * Math.exp(-dt * 3.2) + 30 * dt;
        p.x += p.vx * dt;
        p.y += p.vy * dt;
        p.a += dt * 4;
      } else {
        p.x += (p.vx + Math.sin(t * 1.3 + p.seed) * 3) * dt;
        p.y += p.vy * dt;
      }
    }
    this.ps = this.ps.filter(
      (p) => p.life < p.max && !(p.kind === "swirl" && !this.want.swirl && this.on.swirl < 0.02),
    );
    this.draw(t);
  }

  private draw(t: number) {
    const c = this.ctx;
    if (!c) return;
    const s = (this.px / 110) * this.dpr;
    c.setTransform(1, 0, 0, 1, 0, 0);
    c.clearRect(0, 0, c.canvas.width, c.canvas.height);
    c.setTransform(s, 0, 0, s, 5 * s, 5 * s);
    const rgb = this.rgb;

    for (const p of this.ps) {
      if (p.kind === "swirl") {
        // Behind/in-front depth cue: particles at the back are dimmer and smaller.
        const depth = (Math.sin(p.a) + 1) / 2;
        const a = this.on.swirl * (0.25 + 0.6 * depth) * (0.75 + 0.25 * Math.sin(t * 6 + p.seed));
        c.fillStyle = `rgb(${rgb} / ${a.toFixed(3)})`;
        c.beginPath();
        c.arc(p.x, p.y, 1 + depth * 1.3, 0, Math.PI * 2);
        c.fill();
      } else if (p.kind === "spark") {
        const k = 1 - p.life / p.max;
        drawStar(c, p.x, p.y, p.r * (0.6 + 0.4 * k), p.a, `rgb(255 255 255 / ${(k * 0.95).toFixed(3)})`);
        drawStar(c, p.x, p.y, p.r * 1.6 * k, p.a, `rgb(${rgb} / ${(k * 0.35).toFixed(3)})`);
      } else {
        const k = Math.sin((p.life / p.max) * Math.PI);
        c.fillStyle = `rgb(${rgb} / ${(k * 0.45).toFixed(3)})`;
        c.beginPath();
        c.arc(p.x, p.y, p.r, 0, Math.PI * 2);
        c.fill();
      }
    }

    if (this.on.eq > 0.01) {
      // Equalizer dots in the aura below the spirit.
      for (let i = 0; i < 5; i++) {
        const x = 32 + i * 9;
        const h =
          (0.35 + 0.65 * Math.abs(Math.sin(t * (2.1 + i * 0.37) + i * 1.7))) * 9 * this.on.eq;
        c.fillStyle = `rgb(${rgb} / ${(0.55 * this.on.eq).toFixed(3)})`;
        roundRect(c, x - 1.4, 101 - h, 2.8, h, 1.4);
      }
    }
  }
}

function drawStar(c: CanvasRenderingContext2D, x: number, y: number, r: number, rot: number, fill: string) {
  c.save();
  c.translate(x, y);
  c.rotate(rot);
  c.fillStyle = fill;
  c.beginPath();
  for (let i = 0; i < 8; i++) {
    const rr = i % 2 === 0 ? r : r * 0.28;
    const a = (i / 8) * Math.PI * 2;
    c.lineTo(Math.cos(a) * rr, Math.sin(a) * rr);
  }
  c.closePath();
  c.fill();
  c.restore();
}

function roundRect(c: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  c.beginPath();
  c.roundRect(x, y, w, Math.max(h, 0.1), r);
  c.fill();
}
