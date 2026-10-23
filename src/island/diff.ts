/**
 * Line diffs for the Island's Live Diff view: coding agent `Edit` / `MultiEdit` / `Write` inputs
 * → hunks with 3 lines of context and 1-based line numbers (relative to the edited snippet).
 * LCS over a compact `Uint16Array` table, guarded by size limits (too big → counts only).
 *
 * Pure (no imports) so `tests/ui/diff.test.ts` can run it under plain Node.
 */
export const DIFF_MAX_BYTES = 200 * 1024;
export const DIFF_MAX_LINES = 4000;
export const DIFF_MAX_CELLS = 1_000_000;
const CONTEXT = 3;

export interface DiffLine {
  kind: "added" | "removed" | "context";
  text: string;
  oldNo?: number;
  newNo?: number;
}

export interface DiffHunk {
  oldStart: number;
  newStart: number;
  lines: DiffLine[];
}

export interface FileDiff {
  path: string;
  hunks: DiffHunk[];
  added: number;
  removed: number;
  /** Over the limits: counts only, no hunks. */
  tooBig: boolean;
  isNew: boolean;
}

function splitLines(s: string): string[] {
  if (!s) return [];
  const l = s.replace(/\r\n/g, "\n").split("\n");
  if (l[l.length - 1] === "") l.pop();
  return l;
}

/** LCS line diff (`null` when the table would exceed `DIFF_MAX_CELLS`). */
export function diffLines(a: string[], b: string[]): DiffLine[] | null {
  let pre = 0;
  while (pre < a.length && pre < b.length && a[pre] === b[pre]) pre++;
  let suf = 0;
  while (suf < a.length - pre && suf < b.length - pre && a[a.length - 1 - suf] === b[b.length - 1 - suf]) suf++;
  const m = a.length - pre - suf;
  const n = b.length - pre - suf;
  if ((m + 1) * (n + 1) > DIFF_MAX_CELLS) return null;
  const w = n + 1;
  const t = new Uint16Array((m + 1) * w);
  for (let i = m - 1; i >= 0; i--)
    for (let j = n - 1; j >= 0; j--)
      t[i * w + j] = a[pre + i] === b[pre + j] ? t[(i + 1) * w + j + 1] + 1 : Math.max(t[(i + 1) * w + j], t[i * w + j + 1]);
  const out: DiffLine[] = [];
  let o = 1;
  let nn = 1;
  const keep = (text: string) => out.push({ kind: "context", text, oldNo: o++, newNo: nn++ });
  for (let k = 0; k < pre; k++) keep(a[k]);
  let i = 0;
  let j = 0;
  while (i < m || j < n) {
    if (i < m && j < n && a[pre + i] === b[pre + j]) {
      keep(a[pre + i]);
      i++;
      j++;
    } else if (i < m && (j === n || t[(i + 1) * w + j] >= t[i * w + j + 1])) {
      out.push({ kind: "removed", text: a[pre + i++], oldNo: o++ });
    } else {
      out.push({ kind: "added", text: b[pre + j++], newNo: nn++ });
    }
  }
  for (let k = a.length - suf; k < a.length; k++) keep(a[k]);
  return out;
}

/** Changed lines with `CONTEXT` lines around them; nearby changes share a hunk. */
export function toHunks(lines: DiffLine[]): DiffHunk[] {
  const changed = lines.flatMap((l, i) => (l.kind === "context" ? [] : [i]));
  const hunks: DiffHunk[] = [];
  for (let k = 0; k < changed.length; k++) {
    const start = Math.max(0, changed[k] - CONTEXT);
    while (k + 1 < changed.length && changed[k + 1] <= changed[k] + 2 * CONTEXT + 1) k++;
    const slice = lines.slice(start, Math.min(lines.length, changed[k] + CONTEXT + 1));
    const before = lines.slice(0, start);
    hunks.push({
      oldStart: before.filter((l) => l.kind !== "added").length + 1,
      newStart: before.filter((l) => l.kind !== "removed").length + 1,
      lines: slice,
    });
  }
  return hunks;
}

const count = (lines: DiffLine[], kind: DiffLine["kind"]) => lines.filter((l) => l.kind === kind).length;

export function fromEdit(oldText: string, newText: string, path: string): FileDiff {
  const a = splitLines(oldText);
  const b = splitLines(newText);
  const big = oldText.length + newText.length > DIFF_MAX_BYTES || a.length + b.length > DIFF_MAX_LINES;
  const lines = big ? null : diffLines(a, b);
  if (!lines) return { path, hunks: [], added: b.length, removed: a.length, tooBig: true, isNew: false };
  return { path, hunks: toHunks(lines), added: count(lines, "added"), removed: count(lines, "removed"), tooBig: false, isNew: false };
}

export function fromNew(content: string, path: string): FileDiff {
  const b = splitLines(content);
  const tooBig = content.length > DIFF_MAX_BYTES || b.length > DIFF_MAX_LINES;
  const hunks: DiffHunk[] = tooBig || b.length === 0
    ? []
    : [{ oldStart: 0, newStart: 1, lines: b.map((text, i) => ({ kind: "added" as const, text, newNo: i + 1 })) }];
  return { path, hunks, added: b.length, removed: 0, tooBig, isNew: true };
}

const str = (v: unknown): string => (typeof v === "string" ? v : "");

/** `Edit` / `MultiEdit` / `Write` tool input → a diff (`null` for other tools). */
export function buildFileDiff(tool: string, input: Record<string, unknown> | null | undefined): FileDiff | null {
  if (!input) return null;
  const path = str(input.file_path) || str(input.path);
  if (!path) return null;
  if (tool === "Edit") return fromEdit(str(input.old_string), str(input.new_string), path);
  if (tool === "Write") return fromNew(str(input.content), path);
  if (tool === "MultiEdit" && Array.isArray(input.edits)) {
    const parts = input.edits.map((e) => fromEdit(str((e as Record<string, unknown>)?.old_string), str((e as Record<string, unknown>)?.new_string), path));
    return {
      path,
      hunks: parts.flatMap((p) => p.hunks),
      added: parts.reduce((s, p) => s + p.added, 0),
      removed: parts.reduce((s, p) => s + p.removed, 0),
      tooBig: parts.some((p) => p.tooBig),
      isNew: false,
    };
  }
  return null;
}

/** An agent's markdown answer as one plain line (for the Island). */
export function toOneLine(markdown: string, maxChars = 200): string {
  const s = markdown
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/^\s{0,3}(#{1,6}|>|[-*+]|\d+\.)\s+/gm, "")
    .replace(/[*_`~]+/g, "")
    .replace(/\s+/g, " ")
    .trim();
  const chars = Array.from(s);
  return chars.length > maxChars ? chars.slice(0, maxChars - 1).join("").trimEnd() + "…" : s;
}
