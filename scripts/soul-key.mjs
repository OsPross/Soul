// SOUL_TEST_KEY, else the Gemini key saved in Soul's settings (never printed).
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export function soulKey(tag) {
  if (process.env.SOUL_TEST_KEY) return process.env.SOUL_TEST_KEY;
  const dirs = [
    process.env.APPDATA && join(process.env.APPDATA, "com.oskar.soul"),
    join(homedir(), ".local", "share", "com.oskar.soul"),
    join(homedir(), ".config", "com.oskar.soul"),
  ].filter(Boolean);
  for (const d of dirs) {
    const f = join(d, "soul-settings.json");
    if (!existsSync(f)) continue;
    try {
      const key = JSON.parse(readFileSync(f, "utf8")).apiKey;
      if (key) {
        console.log(`[${tag}] using the API key from Soul's settings`);
        return key;
      }
    } catch {}
  }
  return "";
}
