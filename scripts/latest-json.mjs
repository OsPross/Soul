#!/usr/bin/env node
// CI (release.yml): build the updater manifest `latest.json` from the signed installers attached to
// a (draft) GitHub Release and upload it. Done once after all build jobs — letting every matrix
// job merge its own platform into latest.json races.
//
//   GITHUB_TOKEN=… GITHUB_REPOSITORY=OsPross/Soul node scripts/latest-json.mjs <release-id> <tag>
//
// URLs point at /releases/download/<tag>/… — valid once the draft is published (draft asset URLs
// are temporary). Notes = the CHANGELOG.md section of this version.
import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const [releaseId, tag] = process.argv.slice(2);
const repo = process.env.GITHUB_REPOSITORY;
const token = process.env.GITHUB_TOKEN;
if (!releaseId || !tag || !repo || !token) {
  console.error("usage: GITHUB_TOKEN=… GITHUB_REPOSITORY=owner/repo node scripts/latest-json.mjs <release-id> <tag>");
  process.exit(1);
}
const version = tag.replace(/^v/, "");
const api = `https://api.github.com/repos/${repo}`;
const headers = { Authorization: `Bearer ${token}`, "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "soul-release" };

async function gh(path, init = {}) {
  const res = await fetch(path.startsWith("http") ? path : `${api}${path}`, { ...init, headers: { ...headers, ...init.headers } });
  if (!res.ok) throw new Error(`${init.method ?? "GET"} ${path}: ${res.status} ${await res.text()}`);
  return res;
}

const assets = await (await gh(`/releases/${releaseId}/assets?per_page=100`)).json();
const byName = new Map(assets.map((a) => [a.name, a]));

async function signatureOf(name, suffix) {
  // `<installer>.sig`; fall back to any signature with the same extension in case the action renamed assets.
  const sig = byName.get(`${name}.sig`) ?? assets.find((a) => a.name.endsWith(`${suffix}.sig`));
  if (!sig) return null;
  const res = await gh(sig.url, { headers: { Accept: "application/octet-stream" } });
  return (await res.text()).trim();
}

const find = (suffix) => {
  const name = assets.find((a) => a.name.endsWith(suffix))?.name;
  return name ? { name, suffix } : null;
};
const artifacts = {
  nsis: find("-setup.exe"),
  msi: find(".msi"),
  appimage: find(".AppImage"),
  deb: find(".deb"),
  rpm: find(".rpm"),
};

const platforms = {};
async function add(keys, artifact) {
  if (!artifact) return;
  const { name, suffix } = artifact;
  const signature = await signatureOf(name, suffix);
  if (!signature) {
    console.warn(`no signature for ${name} — skipped`);
    return;
  }
  const url = `https://github.com/${repo}/releases/download/${tag}/${encodeURIComponent(name)}`;
  for (const k of keys) platforms[k] = { signature, url };
}
// Plain `windows-x86_64` / `linux-x86_64` = the self-updating installer (NSIS, AppImage); the
// bundle-specific keys let an MSI install update itself via MSI.
await add(["windows-x86_64", "windows-x86_64-nsis"], artifacts.nsis);
await add(["windows-x86_64-msi"], artifacts.msi);
await add(["linux-x86_64", "linux-x86_64-appimage"], artifacts.appimage);
await add(["linux-x86_64-deb"], artifacts.deb);
await add(["linux-x86_64-rpm"], artifacts.rpm);

if (!platforms["windows-x86_64"] || !platforms["linux-x86_64"]) {
  console.error(`missing signed installers: ${JSON.stringify(artifacts)}`);
  process.exit(1);
}

const notes = execFileSync(process.execPath, [join(root, "scripts", "changelog.mjs"), version], { encoding: "utf8" }).trim();
const manifest = { version, notes, pub_date: new Date().toISOString(), platforms };
writeFileSync(join(root, "latest.json"), JSON.stringify(manifest, null, 2) + "\n");

const old = byName.get("latest.json");
if (old) await gh(`/releases/assets/${old.id}`, { method: "DELETE" });
const release = await (await gh(`/releases/${releaseId}`)).json();
const uploadUrl = release.upload_url.replace(/\{.*\}$/, "") + "?name=latest.json";
await gh(uploadUrl, {
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify(manifest, null, 2) + "\n",
});
console.log(`latest.json uploaded for ${tag}: ${Object.keys(platforms).join(", ")}`);
