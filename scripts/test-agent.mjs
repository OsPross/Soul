// npm run test:agent            → Soul Agent harness on tests/agent/playground.html (real Gemini API)
// npm run test:agent -- --real  → also the real Windows tasks (Notepad, Settings → Sound)
// npm run test:agent -- --only=search → one playground task (--only=vio → only the Vio mock-API cases)
// Always also runs the Vio browser cases against a mock Vio API (tests/agent/vio-mock.json, no desktop input).
// Key: SOUL_TEST_KEY, else the key saved in Soul's settings (never printed).
// Takes over mouse + keyboard for a few minutes. Esc twice / mouse in a corner stops a task.
import { spawnSync } from "node:child_process";
import { soulKey } from "./soul-key.mjs";

const args = process.argv.slice(2);
const env = { ...process.env, SOUL_TEST_KEY: soulKey("test:agent") };
if (!env.SOUL_TEST_KEY) {
  console.error("[test:agent] set SOUL_TEST_KEY (or save a key in Soul's Settings first)");
  process.exit(1);
}
if (args.includes("--real")) env.SOUL_AGENT_REAL = "1";
if (args.includes("--real-only")) Object.assign(env, { SOUL_AGENT_REAL: "1", SOUL_AGENT_SKIP_PLAYGROUND: "1" });
const only = args.find((a) => a.startsWith("--only="));
if (only) env.SOUL_AGENT_ONLY = only.slice("--only=".length);
const r = spawnSync(
  "cargo",
  ["test", "--manifest-path", "src-tauri/Cargo.toml", "--lib", "computer_harness", "--", "--ignored", "--nocapture", "--test-threads=1"],
  { stdio: "inherit", env },
);
if (r.error) console.error(r.error.message);
process.exit(r.status ?? 1);
