#!/usr/bin/env node
// Print the CHANGELOG.md section of one version (release notes for GitHub + latest.json).
//   node scripts/changelog.mjs 0.2.0
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const version = (process.argv[2] ?? "").replace(/^v/, "");
if (!version) {
  console.error("usage: node scripts/changelog.mjs <version>");
  process.exit(1);
}
const text = readFileSync(join(root, "CHANGELOG.md"), "utf8").replace(/\r\n/g, "\n");
const esc = version.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const m = text.match(new RegExp(`^## \\[${esc}\\][^\\n]*\\n([\\s\\S]*?)(?=^## \\[|(?![\\s\\S]))`, "m"));
const body = m?.[1].trim();
process.stdout.write(`${body || `Soul v${version}`}\n`);
