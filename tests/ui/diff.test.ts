// Live Diff engine (src/island/diff.ts). Run: npm run test:ui
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildFileDiff, DIFF_MAX_BYTES, fromEdit, fromNew, toOneLine } from "../../src/island/diff.ts";

test("an edit becomes hunks with 3 lines of context and line numbers", () => {
  const old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
  const neu = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\n";
  const d = fromEdit(old, neu, "/p/x.ts");
  assert.equal(d.added, 2);
  assert.equal(d.removed, 1);
  assert.equal(d.hunks.length, 2);
  const [h1, h2] = d.hunks;
  assert.equal(h1.oldStart, 1);
  assert.deepEqual(
    h1.lines.map((l) => `${l.kind[0]}${l.text}`),
    ["ca", "rb", "aB", "cc", "cd", "ce"],
  );
  assert.deepEqual(h1.lines[1], { kind: "removed", text: "b", oldNo: 2 });
  assert.deepEqual(h1.lines[2], { kind: "added", text: "B", newNo: 2 });
  assert.equal(h2.oldStart, 8);
  assert.equal(h2.newStart, 8);
  assert.deepEqual(h2.lines.at(-1), { kind: "added", text: "k", newNo: 11 });
  assert.equal(fromEdit("same\n", "same\n", "x").hunks.length, 0);
});

test("new files, multi-edits and size guards", () => {
  const w = fromNew("one\ntwo\r\nthree", "/p/new.ts");
  assert.equal(w.isNew, true);
  assert.equal(w.added, 3);
  assert.deepEqual(w.hunks[0].lines.map((l) => l.newNo), [1, 2, 3]);
  const m = buildFileDiff("MultiEdit", { file_path: "/p/m.ts", edits: [{ old_string: "a", new_string: "b" }, { old_string: "c\nd", new_string: "c" }] });
  assert.equal(m?.added, 1);
  assert.equal(m?.removed, 2);
  assert.equal(m?.hunks.length, 2);
  assert.equal(buildFileDiff("Bash", { command: "ls" }), null);
  assert.equal(buildFileDiff("Edit", { old_string: "a" }), null);
  const big = fromEdit("x".repeat(DIFF_MAX_BYTES), "y\n", "/p/big");
  assert.equal(big.tooBig, true);
  assert.equal(big.hunks.length, 0);
  assert.equal(big.added, 1);
});

test("the final answer as one line", () => {
  assert.equal(toOneLine("## Done\n\n- Fixed **`lib.rs`**\n- See [docs](http://x)\n```rust\nfn x(){}\n```"), "Done Fixed lib.rs See docs");
  const long = toOneLine("ż".repeat(300), 10);
  assert.equal(Array.from(long).length, 10);
  assert.ok(long.endsWith("…"));
});
