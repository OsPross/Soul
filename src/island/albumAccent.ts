import { useEffect, useState } from "react";

/**
 * Dominant, readable accent from album art: downsample to 24×24, weight pixels by
 * saturation × mid-lightness, then lift lightness so it glows on dark glass.
 * Returns null while loading / for grey artwork (caller falls back to the user accent).
 */
export function useAlbumAccent(dataUrl: string | null | undefined, enabled: boolean): string | null {
  const [accent, setAccent] = useState<string | null>(null);
  useEffect(() => {
    if (!enabled || !dataUrl) {
      setAccent(null);
      return;
    }
    let alive = true;
    const img = new Image();
    img.onload = () => {
      if (!alive) return;
      try {
        setAccent(dominant(img));
      } catch {
        setAccent(null);
      }
    };
    img.onerror = () => alive && setAccent(null);
    img.src = dataUrl;
    return () => {
      alive = false;
    };
  }, [dataUrl, enabled]);
  return accent;
}

function dominant(img: HTMLImageElement): string | null {
  const N = 24;
  const c = document.createElement("canvas");
  c.width = N;
  c.height = N;
  const ctx = c.getContext("2d", { willReadFrequently: true });
  if (!ctx) return null;
  ctx.drawImage(img, 0, 0, N, N);
  const { data } = ctx.getImageData(0, 0, N, N);
  // Hue histogram (36 bins) weighted by saturation and mid lightness.
  const bins = new Array(36).fill(0).map(() => ({ w: 0, r: 0, g: 0, b: 0 }));
  for (let i = 0; i < data.length; i += 4) {
    const r = data[i] / 255;
    const g = data[i + 1] / 255;
    const b = data[i + 2] / 255;
    const [h, s, l] = rgbToHsl(r, g, b);
    const w = s * s * (1 - Math.abs(l - 0.5) * 1.6);
    if (w <= 0.02) continue;
    const bin = bins[Math.floor(h * 36) % 36];
    bin.w += w;
    bin.r += r * w;
    bin.g += g * w;
    bin.b += b * w;
  }
  const best = bins.reduce((a, b) => (b.w > a.w ? b : a));
  if (best.w < 0.6) return null;
  const [h, s] = rgbToHsl(best.r / best.w, best.g / best.w, best.b / best.w);
  // Readable on near-black glass: high lightness, capped saturation.
  return hslToHex(h, Math.min(0.85, Math.max(0.45, s)), 0.68);
}

function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h = 0;
  if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
  else if (max === g) h = (b - r) / d + 2;
  else h = (r - g) / d + 4;
  return [h / 6, s, l];
}

function hslToHex(h: number, s: number, l: number): string {
  const f = (n: number) => {
    const k = (n + h * 12) % 12;
    const a = s * Math.min(l, 1 - l);
    const v = l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    return Math.round(v * 255)
      .toString(16)
      .padStart(2, "0");
  };
  return `#${f(0)}${f(8)}${f(4)}`;
}
