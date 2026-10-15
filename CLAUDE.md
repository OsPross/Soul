# CLAUDE.md

## What Soul is

Soul is a cross-platform (Windows first, Linux X11 second) voice AI desktop assistant built with
**Tauri 2 + React 19 + TypeScript + Vite + Tailwind 4**, backed by the **Gemini API**. Features:

- **Dynamic Island** – frameless always-on-top pill at the top of the primary monitor. Expands on hover
  to show now-playing media, clipboard history, plans, quick apps, quick settings and assistant state.
- **Push-to-talk dictation** – hold hotkey, speak, release → cleaned-up transcript is pasted into the focused app.
- **Circle-to-Ask** – capture the monitor under the cursor, drag a region, ask by voice/text, get a markdown answer card with follow-ups.
- **Voice commands (agent)** – hold hotkey, speak → deterministic pre-router, then Gemini function calling
  over a fixed tool set (open/close/focus app, web/YouTube search, volume, media keys). No arbitrary shell.

`curo/` is an empty nested git repo unrelated to Soul — ignore it.

## Architecture

### Frontend – `src/`
One bundle, one `index.html`; `App.tsx` picks the view from the Tauri window label:
`island` → `island/Island.tsx`, `settings` → `settings/Settings.tsx`, `ask` → `ask/AskView.tsx`,
anything else (`overlay`) → `overlay/Overlay.tsx`. Windows are declared in `src-tauri/tauri.conf.json`
(island, overlay, settings; `ask` is created at runtime). Frontend talks to Rust via `invoke` commands and events;
settings persistence goes through Rust (`tauri-plugin-store`), not directly.

### Backend – `src-tauri/src/`
- `main.rs` → `lib.rs` (`run()`): app setup, tray, global hotkeys, `#[tauri::command]`s, PTT press/release flow,
  overlay emit, error log (`push_error_log`/`report_error`), model validation/healing on startup.
- `config.rs` – `Settings` struct, defaults, load/save (store file `soul-settings.json`), legacy migration,
  AltGr hotkey migration, `redact_settings` (use this for anything that leaves the backend).
- `audio.rs` – mic capture (cpal) → 16 kHz mono WAV, mic monitor/level.
- `capture.rs` – screen capture for Circle-to-Ask: xcap on Windows; on Linux X11 `platform/x11_capture.rs` (x11rb RandR +
  `GetImage`) — xcap always links PipeWire/libspa 0.10, which doesn't build against Ubuntu 22.04's PipeWire. `ask.rs` – Ask flow/state machine.
- `ai/gemini.rs` – REST client: dictation, Ask, model listing, key test, system prompts, `normalize_model_id`.
- `ai/live.rs` – Gemini Live API (WebSocket `BidiGenerateContent`) for dictation and commands: one async actor per
  feature on Tauri's tokio runtime (tokio-tungstenite + rustls), warm session, keepalive, reconnect/backoff, goAway,
  session resumption (commands). PTT = manual VAD (`activityStart`/`activityEnd`). Non-blocking handle API;
  `collect_dictation` waits for the final transcript. `live_smoke.rs` = opt-in real-API test (`SOUL_TEST_KEY`).
- `ai/cleanup.rs` – dictation cleanup: local filler removal + LLM pass only when a self-correction is detected.
- `ai/quota.rs` – per-model daily 429 exhaustion tracking until midnight Pacific.
- `agent/mod.rs` – voice-command session + tool dispatch (`close_app` requires Confirm/Cancel, 5 s hard limit), spoken-reply
  draining + barge-in. `local_dispatch` runs before any model, in order: context reset („nowy temat”/„zapomnij”) →
  text action → deterministic follow-up → chain (all parts deterministic) → pre-router. Model calls: up to
  `chain::MAX_ACTIONS` (4) per command, in order, stop at the first failure, every call id answered (later Live
  toolCalls in the same turn continue the chain via `LateTools`). `agent/router.rs` – deterministic Polish/English pre-router run before the LLM (returns `PreRoute` or `None`);
  strips filler openers („weź mi”, „czy możesz”, „ej”) first and keeps the user's original words in queries.
  „ścisz o 20” = `set_volume {delta}` (`Platform::get_volume`); `match_app_verb` (open/close/focus) is for chain parts only.
  `agent/context.rs` – conversation context: last 3 exchanges (user, tools+args, answer), in memory only, expire after
  `context_timeout_secs` (90) idle, toggle `context_enabled` (Settings → Głos i dusza → Kontekst rozmowy); `begin_turn`/`note_user`/`note_tool`/`commit(turn)` (a newer turn wins). Sent as a
  `[CONTEXT]` block: Live text turns (prefix), Live audio turns (`begin_turn_with_context` → `clientContent`
  `turnComplete:false` before `activityStart`), REST agent + `search_answer`. `resolve_followup` handles „głośniej”/„i zamknij go”/
  „anuluj go” locally; `looks_like_followup` gates Smart routing (≤ 6 words + connector/relative word/short question).
  Island dot `island://context`. `agent/chain.rs` – connector split (i, oraz, potem, a potem, następnie, and, then, commas)
  only where every part is a command; Island line „✓ … · ✓ …”. `agent/text_action.rs` – „popraw/przetłumacz/streść/
  zrób bardziej formalne|luźniejsze/rozwiń/wyjaśnij to”: strict parser (every word must be object/filler/language),
  `Platform::read_selection` → REST `GeminiClient::text_action` (Ask model + fallback) → paste over the selection with
  `text_action_undo` (10 s, re-focus + Ctrl+Z) or `island://text-result` (`text_result_copy`/`text_result_paste`). The
  selection is read only after an explicit match (no model tool), never enters the context, never goes to Live.
  `agent/intent.rs` – Smart-mode classifier: fuzzy wake word / command verb / short pre-router match / lone greeting /
  short „?” question (unless `Platform::text_input_focused`) → command, else dictation; `classify_smart` adds text actions,
  context resets and — only while context is alive — follow-ups. Dictation traps (`pulapka_*`) must stay dictation. Scene triggers match fuzzily
  (`scenes::match_trigger`: phonetic tokens + similarity ≥ 0.8).
  `agent/confirm.rs` – pending confirmation (`request`/`answer`): while pending, `emit_overlay`/`schedule_hide` never replace
  the card; the talk key (`talk.rs` `Mode::Confirm`, bypasses `busy`) or a separate-mode hotkey records a spoken answer on the
  dictation session (`parse_answer`: „tak”/„nie”, a negative wins). Island side: `island/stage.ts` (`pickStage`, `holdsCard`;
  tested by `npm run test:ui`). Rust UI strings: `i18n.rs` (`ui_lang` from `appearance.uiLang`, `tr`).
- `memory.rs` – local SQLite (`rusqlite` bundled, `soul-memory.db` in the app data dir): facts + notes (`notatka`),
  scenes, app launches. `prompt_facts` injects max 15 facts (keyword + recency; stable order for the warm Live session —
  a changed prompt reconnects it, so call `memory_changed` after edits). Refuses secrets; `extract_fact` = auto-remember.
- `scenes.rs` – scenes (macros): validated step list (open_app/open_url/set_volume/media/youtube_play/web_search/delay/dnd),
  trigger phrases (checked before the router and in Smart classification), Island step chips, launch-based suggestions.
- `timers.rs` – voice timers (ticker thread, `island://timers`, sound + toast + `agent::speak_notice` when done).
- `updater.rs` – auto-update (tauri-plugin-updater, signed GitHub Releases). Check 30 s after start + every 6 h when
  `auto_update`; channel `stable` = `releases/latest/download/latest.json`, `beta` = newest release incl. prereleases via
  the GitHub API. Install kind from `tauri::utils::platform::bundle_type()`: NSIS/MSI/AppImage = `auto` (background
  download, install only on click and only when `soul_is_busy` is false — no recording/speaking/Ask/busy), `.deb`/`.rpm`/dev
  = `manual` (opens the release page). State → `soul://update` (`UpdateStatus`); frontend `src/lib/update.ts`
  (`useUpdate`, `installUpdate` → `update_install` then `plugin-process` `relaunch()`). Island: once-per-version notice
  stage + peek row („Nowa wersja vX.Y — Zainstaluj”), shown only while quiet; Settings → `AboutPage.tsx`.
  `auto_update`/`update_channel` are owned by `update_save_config` (autosave) — `save_app_settings` keeps the stored values.
- `talk.rs` – Smart talk key worker: hook events → gated capture (Live turn opens after 200 ms hold), tap / chord /
  double-tap (command mode) / talk+drag (Circle-to-Ask). `voice.rs` – cpal playback of Live 24 kHz reply audio,
  generation counter for barge-in, `soul://voice-level` for the Island waveform.
- `wake.rs` – offline wake word „Soul” (rustpotter MFCC+DTW, user-trained `wakeword/soul.rpw` in the app data dir).
  `soul-wake` thread (below-normal priority) owns its own cpal stream → `Decimator` (16 kHz) → `WakeEngine`; the stream
  is closed while paused (disabled/untrained, Soul speaking + 0.7 s tail, any recording, `busy`, Ask, fullscreen if
  `wake_pause_in_games`, mic error). Detection → `talk::begin_capture` + energy `Endpointer` (1.2 s silence / 8 s cap,
  4 s no speech = likely false positive) → `on_smart_released`. 2 s cooldown. Status `soul://wake` (Island mic glyph,
  tray check item), Settings test meter `soul://wake-score`. `wake_*` settings are owned by `wake_save_config`
  (autosave, tray) — `save_app_settings` keeps the stored values. Thresholds: `wake::thresholds(sensitivity)`, tuned
  with `wake_regression.rs` (rustpotter's own defaults fire on most speech; its VAD is off — it blocks real detections).
- `island/` – `mod.rs` (window, positioning, assistant banner + `kind`, tool/caption events), `media.rs` (Windows SMTC / Linux MPRIS via zbus),
  `clipboard.rs` (history; skips password-looking strings), `debug_log.rs` (`soul_debug!`), `watchdog.rs`.
- `platform/` – OS abstraction (below).

### Platform trait (`platform/mod.rs`)
`trait Platform: Send + Sync` with `register_hotkey`, `type_text`, `paste_text`, `capture_screen`, `open_app`,
`search_apps`, `close_app`, `focus_app`, `open_url`, `web_search`/`youtube_search`/`youtube_play` (default impls via
`apps.rs` URL helpers), `set_volume`, `get_volume`, `media_control`, `read_selection` (Windows: Ctrl+C / Ctrl+Insert in
consoles, waits on `GetClipboardSequenceNumber`, restores text/image clipboard; X11: PRIMARY), `active_window` /
`activate_window` (refocus the app after an Island click), `send_undo`. `create_platform()` returns `Arc<dyn Platform>`:
`windows.rs` on Windows; on Linux `linux_x11.rs`, or `linux_wayland.rs` when `WAYLAND_DISPLAY`/`XDG_SESSION_TYPE=wayland`.
Platform-specific code lives only in these files (behind `#[cfg(target_os)]`); everything else must stay portable.
`platform::clipboard::with_temporary_text` pastes then restores the previous clipboard. `apps.rs` = fuzzy app index.
`close.rs` = portable `close_app`/`focus_app` target resolution over a process snapshot: known-apps alias map → exe stem /
product name → window title (last resort), then climb to the app's root process (never a helper); `tree` = kill order.
Windows: Toolhelp + EnumWindows (UTF-16 titles), quit URL (`steam://exit`) or WM_CLOSE, 3 s grace, then TerminateProcess
on the tree (tray apps skip WM_CLOSE). X11: /proc + SIGTERM/SIGKILL.
Input hook: `platform/hook.rs` holds the portable `HookCore` (swallow/replay/mask/chord/drag decisions, unit-tested);
`windows_hook.rs` (WH_KEYBOARD_LL/WH_MOUSE_LL) and `x11_hook.rs` (XRecord, observe-only) feed it; Wayland returns an error
and Soul falls back to the separate combos.
Adding a capability: add a trait method, implement it in **all three** backends (Wayland may stub with a clear error).
`duck_other_audio` (WASAPI sessions / `pactl` sink inputs) and `set_do_not_disturb` (toast registry / gsettings or dunst) are best effort.
Windows process spawns: only `windows::hidden_command` (sets `CREATE_NO_WINDOW`), `windows::powershell` (hidden, non-interactive)
and `windows::shell_open` (`ShellExecuteW`, for apps / `.lnk` / `shell:AppsFolder` / URLs) — never `cmd /c start`. The release
exe is GUI-subsystem, so any other spawn flashes a console window; `src-tauri/clippy.toml` disallows `Command::new` (Linux
backends opt out with a module-level allow). Check: `cargo test --release --lib console_probe -- --ignored` (detaches from its
console, watches for new console windows, includes a must-detect control).

## Run / build

```bash
npm install
npm run tauri dev     # starts Vite on :1420 + Rust app
npm run tauri build   # release bundle in src-tauri/target/release
npm run build         # tsc + vite build (frontend typecheck)
cargo check --manifest-path src-tauri/Cargo.toml   # fast Rust check
```
Unit tests: `cargo test --manifest-path src-tauri/Cargo.toml --lib`. Live end-to-end (real API, Polish TTS):
`SOUL_TEST_KEY=<key> cargo test --manifest-path src-tauri/Cargo.toml --lib live_smoke -- --ignored --nocapture`. Voice regression (real WAVs in `tests/voice/`, tools dry-run, writes `tests/voice/report.md`): `SOUL_TEST_KEY=<key> npm run test:voice`
(`voice_regression.rs`; `voice_synth.rs` = TTS samples, `dev_samples.rs` = Settings recorder; `SOUL_DRY_RUN=1` = agent tools logged, not run).
Wake word harness (offline, no key; `tests/voice/wake/`, writes `wake/report.md`): `npm run test:voice -- --wake` (`wake_regression.rs`;
`wake_tune` = parameter sweep; `wake::cpu_live` = live CPU on the default mic). `scripts/test-voice.mjs` picks the test.
`half` is pinned to 2.4.1 in `Cargo.toml`: rustpotter's candle-core 0.2 breaks with half ≥ 2.5 (rand_distr 0.5).
Test binaries have no Tauri comctl32 manifest — code reachable from a test must not call into Tauri windowing/events
(e.g. `audio::build_capture_stream` takes a `LevelEmitter` callback, not an `AppHandle`), or the test exe fails to load
with `STATUS_ENTRYPOINT_NOT_FOUND`. On first launch only the Island shows; open Settings from the tray, paste the Gemini key, Save.
`SOUL_DEBUG=1` enables verbose Island/media logs (`soul_debug!`).

### Packaging / releases
- Version: `package.json` is the source; `tauri.conf.json` has `"version": "../package.json"`; `npm run version:sync`
  mirrors it into `Cargo.toml`/`Cargo.lock` (`scripts/version.mjs`; CI runs `version:check`). Never edit versions by hand.
- Bundles (`tauri.conf.json`): NSIS (currentUser, EN+PL) + MSI, `.deb` + `.rpm` + `.AppImage`. Identifier `com.oskar.soul`
  must never change — it names the app data dir (settings, `soul-memory.db`, `wakeword/`), which survives updates and
  reinstalls (the NSIS uninstaller deletes it only if the user ticks the checkbox). Never store user data next to the exe.
- Updater: public key + endpoint in `plugins.updater`; `src-tauri/tauri.release.conf.json` turns on
  `createUpdaterArtifacts` (only CI / signed builds, so plain local `tauri build` works without the key). Private key:
  `~/.tauri/soul-updater.key` (+ `.password`), GitHub secrets `TAURI_SIGNING_PRIVATE_KEY(_PASSWORD)` — never commit it.
- `npm run release -- 0.2.0` (`scripts/release.mjs`): bump, CHANGELOG.md `[Unreleased]` → version, commit, tag; prints the
  push. Tag `v*` → `.github/workflows/release.yml`: draft release → tauri-action builds (betas: NSIS only, MSI can't encode
  `-beta`) → `scripts/latest-json.mjs` assembles `latest.json` once (notes from `scripts/changelog.mjs`). Publish the draft by hand.
- `.github/workflows/ci.yml` (push/PR, windows-latest + ubuntu-22.04): version check, `npm run build`, `cargo check`,
  `cargo clippy -D warnings`, unit tests. Linux deps: `scripts/ci-linux-deps.sh` (keep in sync when adding native deps).

## Models (Gemini)

Per-feature models, all stored in `Settings` (constants in `config.rs`):

| Feature | Setting | Default |
|---|---|---|
| Dictation (Live API) | `dictation_model` | `gemini-3.5-transcribe-live` |
| Voice commands (Live API) | `commands_model` | `gemini-3.8-live` |
| Circle-to-Ask (REST) | `ask_model` (`model` is legacy alias) | `gemini-3.1-flash-lite` |
| REST fallback | `fallback_model` | `gemini-3.5-flash-lite` |

Reply language: every reply prompt includes `gemini::reply_language_rule` ("Always reply in Polish… never switch language based
on the topic") and the commands session sets `speechConfig.languageCode` (`speech_language_code`; retried without it if a
model rejects it). Question searches use `GeminiClient::search_answer` (Google Search grounding).

`use_live_api` (default true) gates Live for dictation/commands; off = REST only. Any Live failure falls back to
REST with the recorded WAV (`ask_model`, then `fallback_model`). The release path must not hit the network for the
models list — use `gemini::cached_models()`.

Fallback/healing: `lib.rs::resolve_fallback_model` prefers the configured fallback (if available and ≠ primary),
then the default fallback, then the newest Flash-Lite from the live models list. A 404/"no longer available"
model is added to `retired_models` and replaced by `retire_model_and_heal`, which sets `model_notice` shown in
Settings. `validate_models_on_startup` heals saved ids against the live list. Daily-quota 429s are tracked in
`ai/quota.rs` so the exhausted model is skipped until midnight Pacific. Always pass ids through `normalize_model_id`.

## Hotkeys (defaults from `config.rs`)

`talk_mode` = `smart` (default: one hold-to-talk key `talk_key` = `ControlRight` via the low-level hook; transcript routed
by `agent::classify`; double-tap-hold = command mode; talk key + drag = Circle-to-Ask) or `separate` (the combos below).
The Ask combo is registered in both modes; dictation/command combos only in `separate` (or when the hook fails).

| Action | Default |
|---|---|
| Push-to-talk dictation (hold, separate mode) | `Ctrl+Super+Space` |
| Circle-to-Ask | `Ctrl+Shift+Space` |
| Voice commands (hold, separate mode) | `Super+Alt+Space` |

Avoid `Ctrl+Alt+<letter>` – on Polish layouts Right Alt is AltGr (= Ctrl+Alt on Windows) and breaks typing
characters like ć. `save_app_settings` rejects Ctrl+Alt+letter, `migrate_altgr_hotkeys` rewrites old saved values, and the
shortcut handler ignores *Pressed* while Right Alt (`VK_RMENU`) is down (never Released, or hold-to-talk would stick). Parsing: `parse_hotkey` in `lib.rs`.

## Island behaviour

State machine lives in the frontend only: `island/useIslandMode.ts` (collapsed | peek | expanded). Rust (`island/mod.rs`)
resizes on `island_set_mode` and runs a 100 ms cursor poll as safety net (`island://force-collapse`, then native hard collapse).
Window = panel + transparent margin (`PAD_X`/`PAD_B`, mirrored in `Island.tsx`); never draw chrome on the window itself.
Playback progress: `island/progress.ts` (anchor + local interpolation; resync only on track/play-pause change or >2 s drift).

## Visual architecture

- **Spirit** (`src/spirit/Spirit.tsx` + `particles.ts`): SVG droplet-ghost (body, inner light, eyes, mouth, nubs) + canvas
  particles. One rAF loop per instance eases parameters toward per-state targets and writes SVG attributes directly
  (no per-frame React renders). States: `idle | listening | thinking | speaking | success | error | music`; the
  Island maps assistant states to them (`spiritStateOf`). `size` = body width in px; particles only at ≥28 px.
  `SpiritOrDot` falls back to a status dot when the spirit feature is off.
- **Levels** (`src/lib/levels.ts`): `soul://mic-level` (Rust `audio::attach_level_emitter`, ~30 Hz while recording, own
  thread) and `soul://voice-level` (TTS) are kept in refs and read inside rAF loops; stale values decay.
- **Island stages** (`island/Island.tsx`): panel morphs width/height/radius with `anim.spring("morph")`; content is a
  keyed `AnimatePresence` crossfade sized to the target; the spirit glides via `layoutId`. Collapsed priority:
  confirm card > text result card > live stage (or compact pill when quiet / per-mode expand is off) > dictation caption > music > idle.
  Cards with buttons (text result, toast with an action) set `holdCard` so hover does not turn them into peek.
  Rust tags banners with `kind` (`island::set_activity`: dictation | command) and emits `island://tool` (tool chips)
  and `island://caption` (cleaned dictation text). Confirm/Cancel for `close_app` is rendered on the Island.
- **Accents / tokens** (`src/theme/tokens.ts`): `bg, bgElevated, border, textPrimary, textSecondary, accentDictation`
  (amber), `accentCommand` (sky), `accentAsk` (violet), `success, danger`, plus the user accent. Active mode accent >
  album-art accent (`island/albumAccent.ts`) > user accent. Components use `--soul-*` / `--accent` CSS vars.
- **Animation config** (`src/anim/config.ts`, `AnimationProvider.tsx`): one `AppearanceConfig` (accent, spirit size,
  glass, preset, per-feature toggles, speed, stiffness, perf, noise, UI language) stored as the opaque
  `appearance` setting (`save_appearance`, broadcast `soul://appearance`, applied live in every window).
  Effective preset = chosen preset clamped to Minimal by OS reduce-motion / battery saver (`system_power_state`,
  `Platform::power_state`). Components must use `useAnim()` (`on(feature)`, `spring(kind)`, `dur(ms)`) — never
  hardcoded durations. `save_app_settings` never overwrites `appearance`.
- **i18n** (`src/i18n.ts`): Polish default, English fallback; `useT()` for keys, `tr(lang, pl, en)` for one-offs.
- **Settings** (`src/settings/`): sidebar app; backend settings use a floating Save bar (dirty check), appearance autosaves.

## Logging conventions

- Rust logs use `eprintln!("[soul][module] message")`, where `module` is lowercase and matches the area:
  `agent`, `ask`, `audio`, `dictation`, `gemini`, `hook`, `island`, `live`, `memory`, `platform`, `quota`, `scene`, `sfx`, `timer`,
  `update`, `voice`, `wake`, `watchdog`, `error`.
  Reuse an existing tag; add a new one only for a new subsystem.
- Verbose/noisy diagnostics go through `soul_debug!` (needs `SOUL_DEBUG=1`).
- Wrap potentially slow commands in `watchdog::Watch::start("name")`; it warns when work exceeds 200 ms.
- User-visible failures go through `report_error` (error log shown in Settings) in addition to the console line.
- Agent tool use logs `[soul][agent] tool_call` / `tool_result`.

## Rules

1. **Polish UI/language support.** Polish is a first-class language: keep the language setting (`auto` | `pl` | …)
   flowing into every prompt (`language` params in `gemini.rs`), keep the router (`agent/router.rs`) bilingual
   (Polish + English, diacritics and ASCII-folded forms like „otwórz"/"otworz"), and never break Polish diacritics
   in text handling, paste, or hotkeys (see AltGr note). User-facing strings should work in Polish.
2. **Never log the API key** – not in `eprintln!`, error messages, URLs in errors (the key may be in a query string/header),
   debug output or events to the frontend. Log settings only via `redact_settings`. Strip the key from `reqwest`/tungstenite errors before surfacing them.
3. **Never block the main thread.** Hotkey handlers, Tauri commands and setup must return fast: do network,
   audio, capture and `reqwest::blocking` work in `std::thread::spawn` (Live WebSockets run as async tasks on
   `tauri::async_runtime`; never block inside them)
   (the pattern in `on_ptt_pressed/released`) and report back via events. Keep UI-thread window ops short.
4. **Windows first, Linux X11 second.** Develop and verify on Windows; every feature must also work on X11.
   Wayland is best-effort (stubbed capture, fallback island window, clipboard-only paste) – degrade gracefully
   with a clear message, never panic. Don't add platform-specific code outside `platform/` and `island/media.rs`.
5. **No arbitrary shell execution** in agent tools; only registered tools. Destructive tools (`close_app`) need user confirmation.
6. Clipboard history must keep skipping secrets/password-like content; paste must restore the user's previous clipboard.
7. Use `rustls-tls` for HTTPS (no native-tls). Keep new dependencies minimal and cross-platform.
8. Keep `README.md` in sync when hotkeys, models, setup, install or release steps change; add user-visible changes to
   `CHANGELOG.md` under `## [Unreleased]` (it becomes the release notes and the in-app changelog).
9. Clippy must stay clean (`cargo clippy --all-targets -- -D warnings` runs in CI on Windows and Linux).
