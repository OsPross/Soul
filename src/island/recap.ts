/**
 * Weekly coding recap from `coding_recap_history` (Rust `coding_agents/recap.rs`, times in
 * seconds). Overlapping turns (parallel sessions) are merged so time is never double-counted.
 *
 * Pure (no imports) so `tests/ui/recap.test.ts` can run it under plain Node.
 */
export interface RecapTurn {
  agent: string;
  project: string;
  session: string;
  start: number;
  end: number;
  filesChanged: number;
  linesAdded: number;
  linesRemoved: number;
  commandsRun: number;
  questions: number;
}

export interface RecapDecision {
  at: number;
  agent: string;
  allowed: boolean;
}

export interface History {
  enabled: boolean;
  turns: RecapTurn[];
  decisions: RecapDecision[];
}

export interface Summary {
  totalMinutes: number;
  sessionCount: number;
  filesChanged: number;
  linesAdded: number;
  linesRemoved: number;
  commandsRun: number;
  permissionsAllowed: number;
  permissionsDenied: number;
  topAgent: string | null;
  topProject: string | null;
  /** 0 = Monday … 6 = Sunday; `null` without activity. */
  busiestDay: number | null;
  longestSessionMinutes: number;
}

const DAY = 86_400;
const WEEK = 7 * DAY;

/** Local Monday 00:00 of this week (`weeksBack` = 1 → last week), in seconds. */
export function weekStart(nowMs: number, weeksBack = 0): number {
  const d = new Date(nowMs);
  d.setHours(0, 0, 0, 0);
  d.setDate(d.getDate() - ((d.getDay() + 6) % 7) - 7 * weeksBack);
  return Math.floor(d.getTime() / 1000);
}

/** Total seconds covered by the intervals (overlaps counted once). */
export function mergedSeconds(turns: { start: number; end: number }[]): number {
  const iv = turns.filter((t) => t.end > t.start).sort((a, b) => a.start - b.start);
  let total = 0;
  let cs = 0;
  let ce = -Infinity;
  for (const t of iv) {
    if (t.start > ce) {
      if (ce > -Infinity) total += ce - cs;
      cs = t.start;
      ce = t.end;
    } else ce = Math.max(ce, t.end);
  }
  return ce > -Infinity ? total + ce - cs : total;
}

function topBy<T>(items: T[], key: (t: T) => string, value: (group: T[]) => number): string | null {
  const groups = new Map<string, T[]>();
  for (const it of items) groups.set(key(it), [...(groups.get(key(it)) ?? []), it]);
  let best: string | null = null;
  let bestV = 0;
  for (const [k, g] of groups) {
    const v = value(g);
    if (k && v > bestV) [best, bestV] = [k, v];
  }
  return best;
}

const clip = (turns: RecapTurn[], from: number, to: number) =>
  turns.filter((t) => t.end >= from && t.start < to).map((t) => ({ ...t, start: Math.max(t.start, from), end: Math.min(t.end, to) }));

/** The week starting at `weekStartSec` (see `weekStart`). */
export function summarize(h: History, weekStartSec: number): Summary {
  const turns = clip(h.turns, weekStartSec, weekStartSec + WEEK);
  const decisions = h.decisions.filter((d) => d.at >= weekStartSec && d.at < weekStartSec + WEEK);
  const sum = (f: (t: RecapTurn) => number) => turns.reduce((s, t) => s + f(t), 0);
  const days = Array.from({ length: 7 }, (_, i) => mergedSeconds(clip(turns, weekStartSec + i * DAY, weekStartSec + (i + 1) * DAY)));
  const busiest = days.indexOf(Math.max(...days));
  const sessions = new Set(turns.map((t) => t.session));
  const longest = Math.max(0, ...[...sessions].map((s) => mergedSeconds(turns.filter((t) => t.session === s))));
  return {
    totalMinutes: Math.round(mergedSeconds(turns) / 60),
    sessionCount: sessions.size,
    filesChanged: sum((t) => t.filesChanged),
    linesAdded: sum((t) => t.linesAdded),
    linesRemoved: sum((t) => t.linesRemoved),
    commandsRun: sum((t) => t.commandsRun),
    permissionsAllowed: decisions.filter((d) => d.allowed).length,
    permissionsDenied: decisions.filter((d) => !d.allowed).length,
    topAgent: topBy(turns, (t) => t.agent, mergedSeconds),
    topProject: topBy(turns, (t) => t.project, mergedSeconds),
    busiestDay: days[busiest] > 0 ? busiest : null,
    longestSessionMinutes: Math.round(longest / 60),
  };
}

/** „2 h 05 min” / „45 min”. */
export function formatMinutes(m: number): string {
  return m >= 60 ? `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, "0")} min` : `${m} min`;
}
