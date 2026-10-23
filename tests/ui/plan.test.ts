// Plan usage parsing / labels (src/island/plan.ts). Run: npm run test:ui
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  creditsLabel,
  dominantPct,
  effectivePct,
  parseClaudePlan,
  parseCodexPlan,
  planColor,
  resetLabel,
} from "../../src/island/plan.ts";

const now = new Date(2026, 9, 23, 12, 0).getTime();
const H = 3600_000;

test("Claude statusLine rate limits", () => {
  const raw = {
    session_id: "s",
    updated_at_ms: now - 1000,
    rate_limits: {
      five_hour: { used_percentage: 73.4, reset_at: Math.floor((now + 2 * H) / 1000) }, // older spelling
      seven_day: { used_percentage: 120, resets_at: new Date(now + 50 * H).toISOString() },
    },
  };
  const u = parseClaudePlan(raw, now)!;
  assert.equal(u.fiveHour?.usedPct, 73.4);
  assert.equal(u.fiveHour?.resetsAt, now + 2 * H);
  assert.equal(u.sevenDay?.usedPct, 100, "clamped");
  assert.equal(u.updatedAt, now - 1000);
  assert.equal(dominantPct(u, now), 100);
  assert.equal(parseClaudePlan({ session_id: "s" }, now), null);
  assert.equal(parseClaudePlan(null, now), null);
});

test("Codex windows by duration, reset credits", () => {
  const u = parseCodexPlan(
    {
      rateLimits: {
        primary: { usedPercent: 18, windowDurationMins: 300, resetsAt: Math.floor((now + H) / 1000) },
        secondary: { usedPercent: 40, windowDurationMins: 10080, resetsAt: Math.floor((now + 90 * H) / 1000) },
        planType: "plus",
      },
      rateLimitResetCredits: { available: 2, expiresAt: Math.floor((now + 9 * H) / 1000) },
    },
    now,
  )!;
  assert.equal(u.fiveHour?.usedPct, 18);
  assert.equal(u.sevenDay?.usedPct, 40);
  assert.equal(u.planType, "plus");
  assert.equal(u.resetCredits, 2);
  assert.equal(u.resetCreditExpiresAt, now + 9 * H);
  assert.equal(parseCodexPlan({ rateLimits: {} }, now), null);
});

test("expired windows count as 0, colours by threshold", () => {
  assert.equal(effectivePct({ usedPct: 90, resetsAt: now - 1 }, now), 0);
  assert.equal(effectivePct({ usedPct: 90, resetsAt: Infinity }, now), 90);
  assert.equal(dominantPct(null, now), null);
  assert.equal(planColor(null), "#6B7079");
  assert.equal(planColor(49), "#22C55E");
  assert.equal(planColor(50), "#F59E0B");
  assert.equal(planColor(80), "#F4505E");
});

test("bilingual reset and credit labels", () => {
  assert.equal(resetLabel({ usedPct: 1, resetsAt: now + 80 * 60_000 }, now, "pl"), "za 1 godz. 20 min");
  assert.equal(resetLabel({ usedPct: 1, resetsAt: now + 80 * 60_000 }, now, "en"), "in 1 h 20");
  assert.equal(resetLabel({ usedPct: 1, resetsAt: now + 5 * 60_000 }, now, "pl"), "za 5 min");
  assert.equal(resetLabel({ usedPct: 1, resetsAt: now - 1 }, now, "en"), "Resetting…");
  const monday = new Date(2026, 9, 26, 9, 0).getTime();
  assert.equal(resetLabel({ usedPct: 1, resetsAt: monday }, now, "pl"), "pon. 9:00");
  assert.equal(resetLabel({ usedPct: 1, resetsAt: monday }, now, "en"), "Mon 9:00");
  assert.equal(resetLabel({ usedPct: 1, resetsAt: Infinity }, now, "pl"), "");
  assert.equal(creditsLabel(2, "pl"), "2 dostępne");
  assert.equal(creditsLabel(1, "pl"), "1 dostępny");
  assert.equal(creditsLabel(12, "pl"), "12 dostępnych");
  assert.equal(creditsLabel(2, "en"), "2 available");
});
