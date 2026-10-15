#!/usr/bin/env node
// npm run release -- 0.2.0        (or 0.3.0-beta.1 for a beta / prerelease)
//
// Bumps package.json, package-lock.json, Cargo.toml, Cargo.lock; moves the "Unreleased" section of
// CHANGELOG.md under a new "## [0.2.0] - YYYY-MM-DD" heading; commits "Release v0.2.0"; tags v0.2.0.
// Nothing is pushed — the push command is printed at the end. Pushing the tag starts
// .github/workflows/release.yml, which builds, signs and creates a *draft* GitHub Release.
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { SEMVER, packageVersion, setVersion } from "./version.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const CHANGELOG = join(root, "CHANGELOG.md");

const git = (...args) => execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();
const fail = (msg) => {
  console.error(`release: ${msg}`);
  process.exit(1);
};

function cmpSemver(a, b) {
  const parse = (v) => {
    const [core, pre] = v.split("-", 2);
    return { nums: core.split(".").map(Number), pre: pre ? pre.split(".") : [] };
  };
  const x = parse(a);
  const y = parse(b);
  for (let i = 0; i < 3; i++) if (x.nums[i] !== y.nums[i]) return x.nums[i] - y.nums[i];
  if (!x.pre.length || !y.pre.length) return y.pre.length - x.pre.length; // release > prerelease
  for (let i = 0; i < Math.max(x.pre.length, y.pre.length); i++) {
    const p = x.pre[i];
    const q = y.pre[i];
    if (p === undefined) return -1;
    if (q === undefined) return 1;
    const pn = /^\d+$/.test(p);
    const qn = /^\d+$/.test(q);
    if (pn && qn && Number(p) !== Number(q)) return Number(p) - Number(q);
    if (pn !== qn) return pn ? -1 : 1;
    if (p !== q) return p < q ? -1 : 1;
  }
  return 0;
}

/** Move "## [Unreleased]" content under a new version heading. */
function updateChangelog(version, date) {
  const original = existsSync(CHANGELOG) ? readFileSync(CHANGELOG, "utf8") : "";
  let text = original.replace(/\r\n/g, "\n");
  if (!text.trim()) text = "# Changelog\n\n## [Unreleased]\n\n";
  if (new RegExp(`^## \\[${version.replace(/\./g, "\\.")}\\]`, "m").test(text)) {
    fail(`CHANGELOG.md already has a [${version}] section`);
  }
  const m = text.match(/^## \[Unreleased\][^\n]*\n([\s\S]*?)(?=^## \[|(?![\s\S]))/m);
  if (!m) fail('CHANGELOG.md has no "## [Unreleased]" section');
  let body = m[1].trim();
  if (!body) {
    // Nothing written by hand: fall back to the commit subjects since the previous tag.
    let range = "HEAD";
    try {
      range = `${git("describe", "--tags", "--abbrev=0")}..HEAD`;
    } catch {
      /* first release */
    }
    const subjects = git("log", range, "--no-merges", "--format=%s")
      .split("\n")
      .map((s) => s.trim())
      .filter((s) => s && !/^Release v/.test(s));
    body = subjects.length ? `### Changed\n\n${subjects.map((s) => `- ${s}`).join("\n")}` : "- Maintenance release.";
  }
  const section = `## [Unreleased]\n\n## [${version}] - ${date}\n\n${body}\n\n`;
  text = text.slice(0, m.index) + section + text.slice(m.index + m[0].length).replace(/^\n+/, "");
  writeFileSync(CHANGELOG, original.includes("\r\n") ? text.replace(/\n/g, "\r\n") : text);
}

const version = (process.argv[2] ?? "").replace(/^v/, "");
if (!version) fail("usage: npm run release -- <version>   e.g. npm run release -- 0.2.0");
if (!SEMVER.test(version)) fail(`"${version}" is not semver (x.y.z or x.y.z-beta.1)`);

const current = packageVersion();
if (cmpSemver(version, current) <= 0) fail(`${version} is not newer than the current ${current}`);
if (git("status", "--porcelain")) fail("working tree is not clean — commit or stash first");
const tag = `v${version}`;
if (git("tag", "--list", tag)) fail(`tag ${tag} already exists`);
const branch = git("rev-parse", "--abbrev-ref", "HEAD");

const date = new Date().toISOString().slice(0, 10);
setVersion(version);
updateChangelog(version, date);

git("add", "package.json", "package-lock.json", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock", "CHANGELOG.md");
git("commit", "-m", `Release ${tag}`);
git("tag", "-a", tag, "-m", `Soul ${tag}`);

const pre = version.includes("-");
console.log(`\n✔ ${current} → ${version}  (commit + tag ${tag} on ${branch}${pre ? ", prerelease → beta channel" : ""})`);
console.log("\nReview CHANGELOG.md, then publish with:\n");
console.log(`  git push origin ${branch} ${tag}\n`);
console.log("CI builds the installers and creates a DRAFT release — publish it on GitHub to ship the update.");
console.log(`To undo before pushing:  git tag -d ${tag} && git reset --hard HEAD~1`);
