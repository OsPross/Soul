// Coding agent sessions / approval cards (src/island/codingAgents.ts). Run: npm run test:ui
import { test } from "node:test";
import assert from "node:assert/strict";
import { applyDecided, applyHook, emptyCoding, liveSession, parseQuestions, relativeTo, targetOf, type CodingState } from "../../src/island/codingAgents.ts";

const run = (s: CodingState, p: Record<string, unknown>, now = 1000) => applyHook(s, p, now);

test("a turn: prompt → tool steps with a diff → final line", () => {
  let s = emptyCoding();
  s = run(s, { hook_event_name: "UserPromptSubmit", session_id: "a", cwd: "C:\\code\\soul" }).state;
  assert.equal(s.sessions[0].state, "thinking");
  assert.equal(s.sessions[0].project, "soul");
  assert.equal(liveSession(s)?.key, "a");
  s = run(s, { hook_event_name: "PreToolUse", session_id: "a", tool_name: "Bash", tool_input: { command: "cargo   test" } }).state;
  s = run(s, { hook_event_name: "PreToolUse", session_id: "a", tool_name: "Edit", tool_input: { file_path: "C:\\code\\soul\\src\\mod.rs", old_string: "a", new_string: "b\nc" } }).state;
  s = run(s, { hook_event_name: "PostToolUse", session_id: "a", tool_name: "Edit", tool_input: { file_path: "C:\\code\\soul\\src\\mod.rs", old_string: "a", new_string: "b\nc" } }).state;
  const steps = s.sessions[0].steps;
  assert.deepEqual(steps.map((x) => `${x.kind}:${x.target}`), ["run:cargo test", "edit:mod.rs"]);
  assert.equal(steps[1].diff?.added, 2);
  assert.equal(steps[1].diff?.removed, 1);
  assert.equal(s.sessions[0].diffs.length, 1);
  s = run(s, { hook_event_name: "Stop", session_id: "a", last_assistant_message: "**Done.** Tests pass." }).state;
  assert.equal(s.sessions[0].state, "finished");
  assert.equal(s.sessions[0].finalLine, "Done. Tests pass.");
  assert.equal(liveSession(s), null);
});

test("approval: acked once, a second one and non-deciding agents are declined", () => {
  let r = run(emptyCoding(), { hook_event_name: "PermissionRequest", session_id: "a", cwd: "/p/soul", tool_name: "Bash", tool_input: { command: "rm -rf target" }, request_id: "r1" });
  assert.deepEqual(r.effects, [{ type: "ack", requestId: "r1" }]);
  assert.equal(r.state.approval?.target, "rm -rf target");
  assert.equal(r.state.sessions[0].state, "approval");
  const second = run(r.state, { hook_event_name: "PermissionRequest", session_id: "b", soul_agent: "codex", tool_name: "Bash", request_id: "r2" });
  assert.deepEqual(second.effects, [{ type: "decline", requestId: "r2" }]);
  assert.equal(second.state.approval?.requestId, "r1");
  const cursor = run(emptyCoding(), { hook_event_name: "PermissionRequest", soul_agent: "cursor", request_id: "r3" });
  assert.deepEqual(cursor.effects, [{ type: "decline", requestId: "r3" }]);
  // Without a request id (fire-and-forget) nothing happens.
  assert.deepEqual(run(emptyCoding(), { hook_event_name: "PermissionRequest", soul_agent: "gemini" }).effects, []);
  // Decided (voice / click) or the session moved on → the card goes away.
  assert.equal(applyDecided(r.state, "r1").approval, null);
  assert.equal(applyDecided(r.state, "nope").approval?.requestId, "r1");
  assert.equal(run(r.state, { hook_event_name: "Stop", session_id: "a" }).state.approval, null);
  assert.equal(run(r.state, { hook_event_name: "UserPromptSubmit", session_id: "b" }).state.approval?.requestId, "r1");
});

test("AskUserQuestion becomes a question card only when whole", () => {
  const input = { questions: [{ question: "Which DB?", header: "DB", options: [{ label: "SQLite" }, { label: "Postgres" }] }] };
  const r = run(emptyCoding(), { hook_event_name: "PermissionRequest", session_id: "q", tool_name: "AskUserQuestion", tool_input: input, request_id: "r9" });
  assert.equal(r.state.approval?.questions?.[0].options.length, 2);
  assert.equal(r.state.sessions[0].state, "question");
  assert.equal(parseQuestions({ questions: [{ question: "Cut…", options: [{ label: "a" }] }] }), null);
  assert.equal(parseQuestions({ questions: [] }), null);
  const cut = run(emptyCoding(), { hook_event_name: "PermissionRequest", tool_name: "AskUserQuestion", tool_input: { questions: [{ question: "x…" }] }, request_id: "r8" });
  assert.deepEqual(cut.effects, [{ type: "decline", requestId: "r8" }]);
});

test("targets and relative paths", () => {
  assert.equal(relativeTo("C:\\p\\soul\\src\\lib.rs", "C:\\p\\soul"), "src/lib.rs");
  assert.equal(relativeTo("/etc/hosts", "/p"), "/etc/hosts");
  assert.equal(targetOf("Read", { file_path: "/p/a/b.ts" }), "b.ts");
  assert.equal(targetOf("Grep", { pattern: "fn main" }), "fn main");
});
