// npm run test:voice            → voice regression (real API: SOUL_TEST_KEY)
// npm run test:voice -- --wake  → offline wake word harness (no API)
// Extra env vars are passed through (see src-tauri/src/voice_regression.rs, wake_regression.rs).
import { spawnSync } from "node:child_process";

const wake = process.argv.slice(2).includes("--wake");
const test = wake ? "wake_regression::wake_regression" : "voice_regression::voice_regression";
const r = spawnSync(
  "cargo",
  ["test", "--manifest-path", "src-tauri/Cargo.toml", "--lib", test, "--", "--ignored", "--nocapture", "--exact"],
  { stdio: "inherit" },
);
if (r.error) console.error(r.error.message);
process.exit(r.status ?? 1);
