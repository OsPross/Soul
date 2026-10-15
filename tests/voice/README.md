# Voice regression samples

WAV samples (any sample rate, mono or stereo) replayed through Soul's real Smart talk-key pipeline:
Live transcription → `agent::classify_smart` → pre-router → Live tools → REST tools. Tools run in
**dry-run** (logged, never executed). Harness: `src-tauri/src/voice_regression.rs`.

```bash
SOUL_TEST_KEY=<key> npm run test:voice          # all cases → report.md, fails on any FAIL/ERROR
SOUL_TEST_KEY=<key> npm run test:voice:synth    # (optional) Gemini TTS samples from synth.json
```

PowerShell: `$env:SOUL_TEST_KEY="<key>"; npm run test:voice`.

- `cases.json` – `[{ "file", "expect": { "route", "tool", "args", "steps", "text_contains", "text_equals" }, "context"?, "synthetic"? }]`.
  `steps` = every action of a chained command in order (`[{ "tool", "args" }]`); `context` = earlier exchanges
  (`[{ "user", "tool", "args", "answer" }]`) seeded as conversation context before the case (follow-ups),
  otherwise the context is empty. Text actions show up as tool `text_action` (`args.action`: fix, translate,
  summarize, formal, casual, expand, explain; `args.language` for translations) and never read a selection.
  Every `expect` key is optional; string args pass when the actual value contains the expected
  text (case/diacritic-insensitive); `text_contains` is a string or a list; `text_equals` ignores
  case and punctuation but not diacritics. Missing WAVs are SKIPped.
- `scenes.json` – fixture scenes loaded into an in-memory DB (your own `soul-memory.db` is never read).
- Record real samples in a dev build: Settings → Zaawansowane → *Nagraj próbkę testową*
  (saves 16 kHz mono WAV here and adds a case with the chosen route; edit `tool`/`args` afterwards).
- `synth.json` – phrases for synthetic samples (`voices` default Kore+Puck, `speeds` normal/fast,
  also `slow`, `casual`); files are `synth_<id>_<voice>_<speed>.wav`, cases get `"synthetic": true`.
- Env: `SOUL_VOICE_FILTER=<substring>`, `SOUL_VOICE_DELAY_MS` (default 4000 – free-tier pacing),
  `SOUL_TEST_LANGUAGE`, `SOUL_TEST_WAKE_WORD`, `SOUL_TEST_DICTATION_MODEL`, `SOUL_TEST_COMMANDS_MODEL`,
  `SOUL_SYNTH_FORCE=1` (regenerate synthetic WAVs), `SOUL_VOICE_REPORT=<name>.md`,
  `SOUL_TEST_DICTATION_PROMPT=1` (Polish transcription prompt).
- `ab-dictation.ps1` – dictation A/B (language `auto` vs `pl` vs Polish prompt) → `report_ab_*.md`.

The app itself also honours `SOUL_DRY_RUN=1`: agent tools are logged instead of executed.

## Wake word (`wake/`, offline — no API key)

```bash
npm run test:voice -- --wake          # → wake/report.md; fails below 80% same-speaker detection or on any false positive
```

Same `wake::WakeEngine` as the app. `train/` (6–8 takes of „Soul”, trimmed like the Settings wizard) builds the
model; `positive/` must fire (also replayed with white noise at 15 dB SNR and 12 dB quieter); `positive_other/`
(other speakers) and `negative_hard/` (similar words: sól, stół, sold, soup…) are reported but not gated;
`negative/`, the real recordings in `tests/voice/*.wav` and 60 s of noise must not fire.

- `make-samples.ps1` regenerates the synthetic set with the Windows TTS voices (OneCore Adam/Paulina, SAPI
  Paulina/Zira). TTS is far more consistent than a person — for numbers that mean something, copy your own
  Settings samples (`<app data>/wakeword/samples/*.wav`) into `train/` and a few fresh „Soul” takes into `positive/`.
- Env: `SOUL_WAKE_SENS=0.3,0.5,0.7` (sweep), `SOUL_WAKE_MIN_RATE=0.8`, `SOUL_WAKE_REPORT_ONLY=1`.
- Tuning sweep (score mode × DTW band × threshold × min hits):
  `cargo test --manifest-path src-tauri/Cargo.toml --lib wake_regression::wake_tune -- --ignored --nocapture`.
- Optimized run (realistic CPU column): prefix with
  `CARGO_TARGET_DIR=src-tauri/target/opt CARGO_PROFILE_DEV_OPT_LEVEL=3 CARGO_PROFILE_TEST_OPT_LEVEL=3`.
- Live CPU of the real listener path (mic → decimator → detector) on the default mic:
  `SOUL_WAKE_CPU_SECS=30 cargo test --manifest-path src-tauri/Cargo.toml --lib wake::cpu_live -- --ignored --nocapture`.
