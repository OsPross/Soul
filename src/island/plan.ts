/**
 * Claude Code / Codex plan usage (5-hour and 7-day windows) for the Island gauges.
 * Claude: `claude_plan_status().usage` = what the statusLine relay saved (`claude-plan.json`).
 * Codex: `codex_plan_usage()` = `{ rateLimits, rateLimitResetCredits }` from `codex app-server`.
 *
 * Pure (no imports) so `tests/ui/plan.test.ts` can run it under plain Node.
 */
export interface PlanWindow {
  usedPct: number;
  /** Epoch ms; `Infinity` when unknown. */
  resetsAt: number;
}

export interface PlanUsage {
  fiveHour?: PlanWindow;
  sevenDay?: PlanWindow;
  updatedAt: number;
}

export interface CodexPlanUsage extends PlanUsage {
  resetCredits?: number;
  resetCreditExpiresAt?: number;
  planType?: string;
}

type Raw = Record<string, unknown>;
const obj = (v: unknown): Raw | undefined => (v && typeof v === "object" ? (v as Raw) : undefined);
const num = (v: unknown): number | undefined => {
  const n = typeof v === "string" && v.trim() !== "" ? Number(v) : v;
  return typeof n === "number" && Number.isFinite(n) ? n : undefined;
};

/** Epoch seconds / ms or an ISO date → epoch ms. */
export function toMs(v: unknown): number | undefined {
  if (typeof v === "string" && Number.isNaN(Number(v))) {
    const t = Date.parse(v);
    return Number.isNaN(t) ? undefined : t;
  }
  const n = num(v);
  return n === undefined ? undefined : n < 1e12 ? n * 1000 : n;
}

function planWindow(v: unknown): PlanWindow | undefined {
  const w = obj(v);
  if (!w) return undefined;
  const used = num(w.used_percentage ?? w.usedPercentage ?? w.used_percent ?? w.usedPercent);
  if (used === undefined) return undefined;
  return { usedPct: Math.min(100, Math.max(0, used)), resetsAt: toMs(w.resets_at ?? w.resetsAt) ?? Infinity };
}

export function parseClaudePlan(raw: unknown, now: number): PlanUsage | null {
  const r = obj(raw);
  const limits = obj(r?.rate_limits ?? r?.rateLimits);
  if (!r || !limits) return null;
  const fiveHour = planWindow(limits.five_hour ?? limits.fiveHour);
  const sevenDay = planWindow(limits.seven_day ?? limits.sevenDay);
  if (!fiveHour && !sevenDay) return null;
  return { fiveHour, sevenDay, updatedAt: num(r.updated_at_ms) ?? now };
}

export function parseCodexPlan(result: unknown, now: number): CodexPlanUsage | null {
  const r = obj(result);
  const limits = obj(r?.rateLimits) ?? r;
  if (!r || !limits) return null;
  const out: CodexPlanUsage = { updatedAt: now };
  for (const v of [limits.primary, limits.secondary]) {
    const w = planWindow(v);
    if (!w) continue;
    const mins = num(obj(v)?.windowDurationMins);
    if (mins !== undefined && mins <= 24 * 60) out.fiveHour ??= w;
    else out.sevenDay ??= w;
  }
  if (typeof limits.planType === "string") out.planType = limits.planType;
  const credits = r.rateLimitResetCredits;
  const c = obj(credits);
  const n = num(credits) ?? num(c?.available ?? c?.remaining ?? c?.count);
  if (n !== undefined) out.resetCredits = n;
  const exp = toMs(c?.expiresAt ?? c?.expires_at);
  if (exp !== undefined) out.resetCreditExpiresAt = exp;
  if (!out.fiveHour && !out.sevenDay) return null;
  return out;
}

/** A window that already reset counts as 0 % until fresh data arrives. */
export function effectivePct(w: PlanWindow, now: number): number {
  return w.resetsAt <= now ? 0 : w.usedPct;
}

export function dominantPct(u: PlanUsage | null, now: number): number | null {
  const pcts = [u?.fiveHour, u?.sevenDay].filter((w): w is PlanWindow => Boolean(w)).map((w) => effectivePct(w, now));
  return pcts.length ? Math.max(...pcts) : null;
}

export function planColor(pct: number | null): string {
  if (pct === null) return "#6B7079";
  return pct < 50 ? "#22C55E" : pct < 80 ? "#F59E0B" : "#F4505E";
}

const pick = (lang: string, pl: string, en: string) => (lang === "en" ? en : pl);
const DAYS_PL = ["niedz.", "pon.", "wt.", "śr.", "czw.", "pt.", "sob."];
const DAYS_EN = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/** „za 1 godz. 20 min” / „in 1 h 20” within a day, else „pon. 9:00” / „Mon 9:00”. */
export function resetLabel(w: PlanWindow, now: number, lang: string): string {
  if (!Number.isFinite(w.resetsAt)) return "";
  const left = w.resetsAt - now;
  if (left <= 0) return pick(lang, "Resetowanie…", "Resetting…");
  if (left < 24 * 3600_000) {
    const mins = Math.max(1, Math.ceil(left / 60_000));
    const h = Math.floor(mins / 60);
    const m = mins % 60;
    if (h === 0) return pick(lang, `za ${m} min`, `in ${m} min`);
    return pick(lang, `za ${h} godz.${m ? ` ${m} min` : ""}`, `in ${h} h${m ? ` ${m}` : ""}`);
  }
  const d = new Date(w.resetsAt);
  const time = `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
  return `${(lang === "en" ? DAYS_EN : DAYS_PL)[d.getDay()]} ${time}`;
}

/** „1 dostępny” / „2 dostępne” / „5 dostępnych”, „2 available”. */
export function creditsLabel(n: number, lang: string): string {
  if (lang === "en") return `${n} available`;
  const few = n % 10 >= 2 && n % 10 <= 4 && (n % 100 < 12 || n % 100 > 14);
  return `${n} ${n === 1 ? "dostępny" : few ? "dostępne" : "dostępnych"}`;
}
