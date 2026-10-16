// Island stage priority (src/island/stage.ts). Run: npm run test:ui
import { test } from "node:test";
import assert from "node:assert/strict";
import { holdsCard, mustCollapseFor, pickStage, type StageInput } from "../../src/island/stage.ts";

const quiet: StageInput = {
  state: "idle",
  kind: null,
  textResult: false,
  toastKind: null,
  expandAllowed: true,
  showCaption: false,
  updateNotice: false,
  mediaActive: false,
};

test("a pending confirmation beats everything else", () => {
  const busy: StageInput = {
    ...quiet,
    state: "confirm",
    kind: "command",
    textResult: true,
    toastKind: "timer",
    showCaption: true,
    updateNotice: true,
    mediaActive: true,
  };
  assert.equal(pickStage(busy), "confirm");
  // Media playing (the bug: hover showed the player instead of the card).
  assert.equal(pickStage({ ...quiet, state: "confirm", mediaActive: true }), "confirm");
  // Even with expanding turned off for commands (quiet mode).
  assert.equal(pickStage({ ...quiet, state: "confirm", expandAllowed: false }), "confirm");
});

test("the confirm card holds: no peek on hover, no expand", () => {
  assert.equal(holdsCard("confirm", false), true);
  assert.equal(holdsCard("result", false), true);
  assert.equal(holdsCard("toast", true), true);
  assert.equal(holdsCard("toast", false), false);
  assert.equal(holdsCard("music", false), false);
  assert.equal(holdsCard("stage", false), false);
});

test("an open Island folds back onto a new confirmation", () => {
  assert.equal(mustCollapseFor("confirm", "peek"), true);
  assert.equal(mustCollapseFor("confirm", "expanded"), true);
  assert.equal(mustCollapseFor("confirm", "collapsed"), false);
  assert.equal(mustCollapseFor("music", "peek"), false);
});

test("the rest of the order is unchanged", () => {
  assert.equal(pickStage(quiet), "idle");
  assert.equal(pickStage({ ...quiet, mediaActive: true }), "music");
  assert.equal(pickStage({ ...quiet, mediaActive: true, showCaption: true }), "caption");
  assert.equal(pickStage({ ...quiet, state: "thinking", mediaActive: true }), "stage");
  assert.equal(pickStage({ ...quiet, state: "thinking", expandAllowed: false }), "compact");
  assert.equal(pickStage({ ...quiet, state: "done", kind: "dictation", showCaption: true }), "caption");
  assert.equal(pickStage({ ...quiet, state: "thinking", textResult: true }), "result");
  assert.equal(pickStage({ ...quiet, state: "thinking", toastKind: "timer" }), "toast");
  assert.equal(pickStage({ ...quiet, toastKind: "memory", updateNotice: true }), "toast");
  assert.equal(pickStage({ ...quiet, updateNotice: true, mediaActive: true }), "update");
});

test("a running agent task shows its card; its confirmation and question win over it", () => {
  const running: StageInput = { ...quiet, state: "thinking", kind: "agent", agent: true, mediaActive: true };
  assert.equal(pickStage(running), "agent");
  assert.equal(pickStage({ ...running, state: "confirm" }), "confirm");
  assert.equal(pickStage({ ...running, state: "question" }), "question");
  assert.equal(pickStage({ ...running, toastKind: "timer" }), "agent");
  // The Stop button / answer card hold: no peek on hover, and an open Island folds back.
  assert.equal(holdsCard("agent", false), true);
  assert.equal(holdsCard("question", false), true);
  assert.equal(mustCollapseFor("agent", "expanded"), true);
  assert.equal(mustCollapseFor("question", "peek"), true);
  // Task over → the usual result stage.
  assert.equal(pickStage({ ...running, agent: false, state: "done" }), "stage");
});
