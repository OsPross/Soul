#!/usr/bin/env node
// Fail if a Google / Gemini API key shows up in source or build output (CI: ci.yml, release.yml).
//
//   node scripts/secret-scan.mjs --git dist src-tauri/target/release/soul.exe
//
// --git   also scan every git-tracked file. Paths may be files or directories (scanned recursively,
//         binaries included). Prints only file names and match counts — never the matched text.
// Shapes: classic keys `AIza` + 35 chars, newer keys `AQ.` + 30 or more. Placeholders such as
// "AIza…" don't match.
import { execFileSync } from "node:child_process";
import { existsSync, lstatSync, readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";

const SHAPES = [
  ["AIza key", /(?<![0-9A-Za-z_-])AIza[0-9A-Za-z_-]{35}(?![0-9A-Za-z_-])/g],
  ["AQ. key", /(?<![0-9A-Za-z_.-])AQ\.[0-9A-Za-z_-]{30,}/g],
];

const args = process.argv.slice(2);
const files = new Set();
if (args.includes("--git")) {
  for (const f of execFileSync("git", ["ls-files", "-z"], { encoding: "utf8" }).split("\0")) if (f) files.add(f);
}
function walk(p) {
  const st = lstatSync(p);
  if (st.isSymbolicLink()) return;
  if (st.isDirectory()) for (const e of readdirSync(p)) walk(join(p, e));
  else if (st.isFile()) files.add(p);
}
for (const a of args.filter((x) => x !== "--git")) {
  if (existsSync(a)) walk(a);
  else console.warn(`secret-scan: ${a} does not exist — skipped`);
}

let bytes = 0;
const hits = [];
for (const f of files) {
  if (!existsSync(f)) continue;
  const buf = readFileSync(f);
  bytes += buf.length;
  // latin1 keeps every byte (binaries); also try the text with NULs dropped (UTF-16 strings).
  const text = buf.toString("latin1");
  const variants = text.includes("\0") ? [text, text.replace(/\0/g, "")] : [text];
  for (const [name, re] of SHAPES) {
    const n = Math.max(...variants.map((v) => v.match(re)?.length ?? 0));
    if (n) hits.push(`${relative(process.cwd(), f) || f}: ${n} × ${name}`);
  }
}
console.log(`secret-scan: ${files.size} files, ${(bytes / 1048576).toFixed(1)} MB`);
if (hits.length) {
  console.error(`::error::API key found — remove it, rotate the key, and do not ship this build:\n  ${hits.join("\n  ")}`);
  process.exit(1);
}
console.log("secret-scan: no API keys found");
