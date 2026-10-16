# Voice E2E suite

Synthetic Polish speech → the **real Soul dev app** → Gemini Live / REST → router → tools / Soul Agent → **Vio**,
checked on the result (page state, tool calls, pasted text), not on `ok=true`.

```bash
npm run test:e2e:synth   # TTS samples + noise / quiet / padded variants → generated/ (gitignored), SCRIPT.md
npm run test:e2e         # builds the dev app, starts Vite, runs every sample → report.md (+ out/app.log, out/results.json)
```

Needs: Vio running with *Zaawansowane → Soul → „Pozwól Soul sterować przeglądarką”* on (accept „Zezwól” once per Vio
start), Soul itself closed (the suite starts `target/debug/soul.exe`), hands off mouse and keyboard (~25 min).
The Gemini key comes from `SOUL_TEST_KEY` or Soul's saved settings (synth only; never printed).

| File | What |
|---|---|
| `CAPABILITIES.md` | every capability, where it is decided, which case covers it, real vs dry-run |
| `manifest.json` | the cases: `text` (spoken), `transcript` (how Live writes it), `setup`, `expect`, `variants` |
| `SCRIPT.md` | generated table of phrases, voices, sample source and expectations |
| `report.md` | last run: per-case result, transcript, similarity, route, via, tools, agent steps / model calls, models, HTTP / Live errors, retries, screenshots, latency, Island states; failures with likely causes |
| `real/` | optional own recordings: `real/<case id>.wav` or `real/<case id>__anything.wav` run with the same expectations |
| `../pages/` | order form, long article, numbered links, notes textarea, neutral start page (served on 127.0.0.1) |

## How a case runs

1. **Reset**: the suite's Vio tabs are closed (your own tabs are never touched), the case's pages opened (last one
   active), Vio brought to the front; `focus` clicks into a field like a user (UI Automation rect + mouse click),
   `select_all` sends Ctrl+A, `foreground: calc` opens Calculator in front of Vio.
2. **Talk key**: the WAV is written to `<app data>/dev-utterance.json`; the dev build plays it into the recorder in
   real time (`audio::set_dev_input`) through `talk::begin_capture` (gated, armed after 200 ms) → `on_smart_released`
   — the same capture, silence trim, Live transcription, routing and tools as a real press.
3. **Trace**: with `SOUL_E2E=1` the app prints `[soul][e2e] {json}` lines (heard, route, stage/via, tool, agent,
   confirm, paste, overlay, command_done, done). Confirmation cards are answered from the case (`confirm`, default deny).
4. **Verify**: `listTabs` / `getElements` / `getPage` of the active tab, the trace, and log metrics.

Risky or system-changing tools run in dry-run (`dry_run_tools` in the manifest → `SOUL_DRY_RUN_TOOLS`): only the
intended call is checked. Soul's settings and memory DB are backed up before the run and restored after it.

Env: `SOUL_E2E_FILTER=<id substring>`, `SOUL_E2E_VARIANTS=0`, `SOUL_E2E_DELAY_MS` (default 5000, free-tier pacing),
`SOUL_SYNTH_FORCE=1`, `SOUL_E2E_FOCUS_DEBUG=1` (logs the foreground window while a case runs).
Diagnostics: `cargo test --manifest-path src-tauri/Cargo.toml --lib voice_e2e::vio_paste_probe -- --ignored --nocapture`.

## TTS notes

- Gemini TTS reads a style prompt aloud („Powiedz szybko po polsku: …”), so samples get the bare text; `fast` / `slow`
  are the same take played 12 % faster / 10 % slower.
- Free tier: **10 TTS requests per day per model**. The synth rotates over all TTS models, then falls back to the
  offline Windows Polish voices (Paulina / Adam), recorded in `generated/sources.json`; a later synth run upgrades
  those takes to Gemini. Non-Polish cases with an offline take are SKIPped (a Polish voice cannot speak Spanish).

## Vio changes that would help (not done here — Vio is a separate project)

1. **Keyboard focus**: `fill` / `click` move DOM focus inside the page but not Chromium's keyboard focus into the
   `<webview>`, so OS keystrokes (Soul's dictation paste, text actions, `Ctrl+A`) go to the address bar. Calling
   `webview.focus()` / `webContents.focus()` in `fill` and `click` would fix it.
2. **`<select>`, `type=time|date|number`**: `fill` refuses them („element is not a text field”); Soul has to click
   and send ArrowDown, and cannot set a time at all. A `select {ref, label|value}` command and `fill` support for
   time/date (set `value` + dispatch `input`/`change`) would cover forms.
3. **Viewport-only `getElements`**: fields below the fold are invisible to the agent until it scrolls. An option
   such as `getElements {all: true}` (with a `visible` flag) would avoid extra steps on long forms.
4. **`pressKey` needs the Vio window focused** (documented) — with another app in front, select-by-keys fails.
5. **`insertText {tabId, text}` into the focused element** would let Soul dictate into Vio without borrowing the
   clipboard, and **`getSelection {tabId}`** would let text actions work without a simulated Ctrl+C.
6. In dev, Vio runs as `electron.exe`, so window/process-name checks see „electron”; Soul matches Vio by the
   pid from `automation.json`. A real `Vio.exe` name (or the pid of the window's process in `listTabs`) is safer.
