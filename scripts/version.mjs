#!/usr/bin/env node
// Keeps the app version in one place: package.json is the source of truth, tauri.conf.json reads it
// ("version": "../package.json"), and this script mirrors it into src-tauri/Cargo.toml + Cargo.lock.
//
//   node scripts/version.mjs            print the version
//   node scripts/version.mjs sync       copy package.json's version into Cargo.toml / Cargo.lock
//   node scripts/version.mjs check      exit 1 if they differ (CI)
//   node scripts/version.mjs set 0.2.0  write the version everywhere
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const PKG = join(root, "package.json");
const LOCK = join(root, "package-lock.json");
const CARGO = join(root, "src-tauri", "Cargo.toml");
const CARGO_LOCK = join(root, "src-tauri", "Cargo.lock");
const TAURI_CONF = join(root, "src-tauri", "tauri.conf.json");

// SemVer 2.0 (pre-release allowed, e.g. 0.2.0-beta.1).
export const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

const read = (p) => readFileSync(p, "utf8");
/** Preserve the file's line endings when rewriting it. */
const write = (p, text, original) =>
  writeFileSync(p, original.includes("\r\n") ? text.replace(/\r?\n/g, "\r\n") : text);

export function packageVersion() {
  return JSON.parse(read(PKG)).version;
}

function cargoVersion() {
  const m = read(CARGO).match(/^\[package\][^[]*?^version\s*=\s*"([^"]+)"/m);
  return m?.[1] ?? null;
}

function setJsonVersion(path, version, alsoRootPackage) {
  const text = read(path);
  const json = JSON.parse(text);
  json.version = version;
  if (alsoRootPackage && json.packages?.[""]) json.packages[""].version = version;
  write(path, JSON.stringify(json, null, 2) + "\n", text);
}

function setCargoVersion(version) {
  const text = read(CARGO);
  const next = text.replace(/^(\[package\][^[]*?^version\s*=\s*")[^"]+(")/m, `$1${version}$2`);
  if (next === text && cargoVersion() !== version) throw new Error("Could not find [package] version in Cargo.toml");
  write(CARGO, next, text);

  const lock = read(CARGO_LOCK);
  const nextLock = lock.replace(/(\[\[package\]\]\r?\nname = "soul"\r?\nversion = ")[^"]+(")/, `$1${version}$2`);
  write(CARGO_LOCK, nextLock, lock);
}

export function setVersion(version) {
  if (!SEMVER.test(version)) throw new Error(`"${version}" is not a semver version (x.y.z or x.y.z-beta.1)`);
  setJsonVersion(PKG, version, false);
  setJsonVersion(LOCK, version, true);
  setCargoVersion(version);
}

export function checkVersions() {
  const pkg = packageVersion();
  const cargo = cargoVersion();
  const conf = JSON.parse(read(TAURI_CONF)).version;
  const problems = [];
  if (cargo !== pkg) problems.push(`Cargo.toml has ${cargo}, package.json has ${pkg}`);
  if (conf !== "../package.json" && conf !== pkg) problems.push(`tauri.conf.json has ${conf}, package.json has ${pkg}`);
  return { pkg, problems };
}

const isMain = process.argv[1] && fileURLToPath(import.meta.url) === join(process.argv[1]);
if (isMain) {
  const [cmd, arg] = process.argv.slice(2);
  try {
    if (!cmd) {
      console.log(packageVersion());
    } else if (cmd === "sync") {
      setVersion(packageVersion());
      console.log(`Synced version ${packageVersion()} → Cargo.toml, Cargo.lock`);
    } else if (cmd === "set") {
      setVersion(arg ?? "");
      console.log(`Version set to ${arg}`);
    } else if (cmd === "check") {
      const { pkg, problems } = checkVersions();
      if (problems.length) {
        console.error(`Version mismatch:\n  ${problems.join("\n  ")}\nRun: npm run version:sync`);
        process.exit(1);
      }
      console.log(`Versions in sync: ${pkg}`);
    } else {
      throw new Error(`Unknown command "${cmd}" (use sync | check | set <version>)`);
    }
  } catch (e) {
    console.error(String(e.message ?? e));
    process.exit(1);
  }
}
