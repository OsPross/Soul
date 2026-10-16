// npm run test:e2e:synth → Gemini TTS samples for tests/e2e/voice/manifest.json (+ local noise/quiet/pad variants)
// npm run test:e2e        → builds the dev app, starts Vite (Island UI), runs every sample through Soul + Vio
// Key: SOUL_TEST_KEY, else the key saved in Soul's settings (never printed). Extra env: see src-tauri/src/voice_e2e.rs.
// Needs Vio running with „Pozwól Soul sterować przeglądarką” on, and Soul itself closed.
import { spawn, spawnSync } from "node:child_process";
import { soulKey } from "./soul-key.mjs";

const synth = process.argv.includes("--synth");
const env = { ...process.env };
const cargo = (args) => spawnSync("cargo", args, { stdio: "inherit", env });
const test = (name) =>
  cargo(["test", "--manifest-path", "src-tauri/Cargo.toml", "--lib", name, "--", "--ignored", "--nocapture", "--exact"]);

if (synth) {
  env.SOUL_TEST_KEY = soulKey("test:e2e");
  if (!env.SOUL_TEST_KEY) {
    console.error("[test:e2e] set SOUL_TEST_KEY (or save a key in Soul's Settings first)");
    process.exit(1);
  }
  process.exit(test("voice_e2e::voice_e2e_synth").status ?? 1);
}

const build = cargo(["build", "--manifest-path", "src-tauri/Cargo.toml"]);
if (build.status !== 0) process.exit(build.status ?? 1);
const up = await fetch("http://localhost:1420").then(() => true, () => false);
const vite = up ? null : spawn("npx", ["vite", "--port", "1420", "--strictPort"], { stdio: "ignore", shell: true });
const r = test("voice_e2e::voice_e2e");
if (vite && process.platform === "win32") spawnSync("taskkill", ["/pid", String(vite.pid), "/T", "/F"], { stdio: "ignore" });
else vite?.kill();
process.exit(r.status ?? 1);
