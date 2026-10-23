// Weekly coding recap (src/island/recap.ts). Run: npm run test:ui
import { test } from "node:test";
import assert from "node:assert/strict";
import { formatMinutes, mergedSeconds, summarize, weekStart, type RecapTurn } from "../../src/island/recap.ts";

const turn = (p: Partial<RecapTurn>): RecapTurn => ({
  agent: "claude",
  project: "soul",
  session: "a",
  start: 0,
  end: 0,
  filesChanged: 0,
  linesAdded: 0,
  linesRemoved: 0,
  commandsRun: 0,
  questions: 0,
  ...p,
});

test("parallel sessions are not double-counted", () => {
  assert.equal(mergedSeconds([]), 0);
  assert.equal(mergedSeconds([{ start: 0, end: 60 }, { start: 30, end: 90 }, { start: 200, end: 260 }]), 150);
  assert.equal(mergedSeconds([{ start: 10, end: 20 }, { start: 0, end: 100 }]), 100);
  assert.equal(mergedSeconds([{ start: 5, end: 5 }]), 0);
});

test("weekly summary", () => {
  const now = new Date(2026, 9, 23, 12, 0).getTime(); // Friday
  const ws = weekStart(now);
  assert.equal(new Date(ws * 1000).getDay(), 1);
  const last = weekStart(now, 1);
  assert.equal(new Date(last * 1000).getDay(), 1);
  assert.ok(Math.abs(ws - last - 7 * 86_400) <= 3600); // ± a DST hour
  const h = {
    enabled: true,
    turns: [
      // Tuesday: two parallel sessions, 60 + 30 min overlapping by 30.
      turn({ session: "a", start: ws + 86_400 + 3600, end: ws + 86_400 + 7200, filesChanged: 3, linesAdded: 40, linesRemoved: 5, commandsRun: 2 }),
      turn({ session: "b", agent: "codex", project: "web", start: ws + 86_400 + 5400, end: ws + 86_400 + 7200, linesAdded: 10 }),
      // Thursday, 20 min.
      turn({ session: "a", start: ws + 3 * 86_400, end: ws + 3 * 86_400 + 1200, filesChanged: 1 }),
      // Last week: ignored.
      turn({ session: "z", start: ws - 5000, end: ws - 4000, linesAdded: 999 }),
    ],
    decisions: [
      { at: ws + 100, agent: "claude", allowed: true },
      { at: ws + 200, agent: "codex", allowed: false },
      { at: ws - 100, agent: "claude", allowed: true },
    ],
  };
  const s = summarize(h, ws);
  assert.equal(s.totalMinutes, 80);
  assert.equal(s.sessionCount, 2);
  assert.equal(s.filesChanged, 4);
  assert.equal(s.linesAdded, 50);
  assert.equal(s.linesRemoved, 5);
  assert.equal(s.commandsRun, 2);
  assert.equal(s.permissionsAllowed, 1);
  assert.equal(s.permissionsDenied, 1);
  assert.equal(s.topAgent, "claude");
  assert.equal(s.topProject, "soul");
  assert.equal(s.busiestDay, 1);
  assert.equal(s.longestSessionMinutes, 80);
  const empty = summarize({ enabled: true, turns: [], decisions: [] }, ws);
  assert.equal(empty.busiestDay, null);
  assert.equal(empty.topAgent, null);
  assert.equal(empty.longestSessionMinutes, 0);
  assert.equal(formatMinutes(125), "2 h 05 min");
  assert.equal(formatMinutes(45), "45 min");
});
