/**
 * Soul palette tokens — the single source of colour for every window.
 *
 * | token            | use                                                        |
 * |------------------|------------------------------------------------------------|
 * | bg               | Island / card glass base (alpha is driven by Island opacity) |
 * | bgElevated       | raised surfaces inside glass (rows, inputs, chips)         |
 * | border           | 1px hairline on glass                                      |
 * | textPrimary      | titles, primary labels                                     |
 * | textSecondary    | captions, hints                                            |
 * | accentDictation  | dictation stage (warm amber)                               |
 * | accentCommand    | voice-command stage (cool sky)                             |
 * | accentAsk        | Circle-to-Ask (violet)                                     |
 * | success / danger | result states                                              |
 * | accent (user)    | idle spirit glow + controls; overridden by album art tint  |
 *
 * `applyTokens()` writes them as CSS variables (`--soul-*`) on :root; components use the
 * variables, never raw hex, so accent changes animate with CSS transitions.
 */
export const tokens = {
  bg: "rgb(12 12 14)",
  bgElevated: "rgba(255,255,255,0.06)",
  border: "rgba(255,255,255,0.10)",
  textPrimary: "#f5f5f7",
  textSecondary: "rgba(235,235,245,0.60)",
  accentDictation: "#ffb547",
  accentCommand: "#5ab8ff",
  accentAsk: "#b18cff",
  success: "#5ee6a0",
  danger: "#ff6b7a",
  /** Soul's own default glow: a pale spirit mint. */
  accentDefault: "#7cf2c8",
} as const;

export type ModeKind = "dictation" | "command" | "ask" | "agent";

export function accentFor(kind: ModeKind | null | undefined, fallback: string): string {
  if (kind === "dictation") return tokens.accentDictation;
  if (kind === "command" || kind === "agent") return tokens.accentCommand;
  if (kind === "ask") return tokens.accentAsk;
  return fallback;
}

export function applyTokens(root: HTMLElement = document.documentElement) {
  const set = (k: string, v: string) => root.style.setProperty(k, v);
  set("--soul-bg-rgb", "12 12 14");
  set("--soul-bg-elevated", tokens.bgElevated);
  set("--soul-border", tokens.border);
  set("--soul-text", tokens.textPrimary);
  set("--soul-muted", tokens.textSecondary);
  set("--soul-dictation", tokens.accentDictation);
  set("--soul-command", tokens.accentCommand);
  set("--soul-ask", tokens.accentAsk);
  set("--soul-success", tokens.success);
  set("--soul-danger", tokens.danger);
}

/** "#rrggbb" → "r g b" for `rgb(var(--x) / a)` usage. */
export function hexToRgbTriplet(hex: string): string {
  const m = hex.replace("#", "").match(/^([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i);
  if (!m) return "124 242 200";
  return `${parseInt(m[1], 16)} ${parseInt(m[2], 16)} ${parseInt(m[3], 16)}`;
}

export function withAlpha(hex: string, a: number): string {
  return `rgb(${hexToRgbTriplet(hex)} / ${a})`;
}
