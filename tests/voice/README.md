# Voice regression samples

WAV samples (any sample rate, mono or stereo) replayed through Soul's real Smart talk-key pipeline:
Live transcription → `agent::classify_smart` → pre-router → Live tools → REST tools. Tools run in
**dry-run** (logged, never executed). Harness: `src-tauri/src/voice_regression.rs`.

```bash
SOUL_TEST_KEY=<key> npm run test:voice          # all cases → report.md, fails on any FAIL/ERROR
SOUL_TEST_KEY=<key> npm run test:voice:synth    # (optional) Gemini TTS samples from synth.json
```

PowerShell: `$env:SOUL_TEST_KEY="<key>"; npm run test:voice`.

- `cases.json` – `[{ "file", "expect": { "route", "tool", "args", "text_contains" }, "synthetic"? }]`.
  Every `expect` key is optional; string args pass when the actual value contains the expected
  text (case/diacritic-insensitive); `text_contains` is a string or a list. Missing WAVs are SKIPped.
- Record real samples in a dev build: Settings → Zaawansowane → *Nagraj próbkę testową*
  (saves 16 kHz mono WAV here and adds a case with the chosen route; edit `tool`/`args` afterwards).
- `synth.json` – phrases for synthetic samples (`voices` default Kore+Puck, `speeds` normal/fast,
  also `slow`, `casual`); files are `synth_<id>_<voice>_<speed>.wav`, cases get `"synthetic": true`.
- Env: `SOUL_VOICE_FILTER=<substring>`, `SOUL_VOICE_DELAY_MS` (default 4000 – free-tier pacing),
  `SOUL_TEST_LANGUAGE`, `SOUL_TEST_WAKE_WORD`, `SOUL_TEST_DICTATION_MODEL`, `SOUL_TEST_COMMANDS_MODEL`,
  `SOUL_SYNTH_FORCE=1` (regenerate synthetic WAVs).

The app itself also honours `SOUL_DRY_RUN=1`: agent tools are logged instead of executed.
