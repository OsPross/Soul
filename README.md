# Soul

Cross-platform voice AI desktop assistant (Windows + Linux) built with **Tauri 2**, **React**, **TypeScript**, **Vite**, and **Tailwind**.

Features: **Dynamic Island** top bar, **one-key push-to-talk** (dictation + voice commands, routed automatically), **spoken replies** with selectable voice and personality, **Circle-to-Ask**, and **voice commands** (agent mode).

### Models

| Feature | Default | Transport |
|---------|---------|-----------|
| Dictation | `gemini-3.5-transcribe-live` | Live API (WebSocket), SMART transcription |
| Voice commands + spoken replies | `gemini-3.8-live` | Live API with function calling, native audio out |
| Circle-to-Ask | `gemini-3.1-flash-lite` | REST `generateContent` |
| REST fallback | `gemini-3.5-flash-lite` | REST |

With **Use Live API** on (Settings, default), audio streams in 100 ms chunks while you hold the hotkey over a warm session (keepalive, reconnect with backoff, goAway + session resumption); push-to-talk is manual VAD (`activityStart` on press, `activityEnd` on release). If Live fails, the recording goes to REST: `gemini-3.1-flash-lite`, then `gemini-3.5-flash-lite`. A 429 that says *per day* marks that model exhausted until midnight Pacific. Model ids are checked against `GET /v1beta/models` on startup; retired/missing ones are auto-replaced and a notice is shown in Settings. Turn off **Use Live API** to use REST only.

Dictation output stays in the spoken language (Polish by default) and is never translated; fillers and self-corrections are removed.

### Hotkeys (defaults)

Settings → **Talk key → Mode**: **Smart (one key)** (default) or **Separate keys**.

| Action | Smart (default) | Separate keys |
|--------|-----------------|---------------|
| Talk: dictation *or* command, routed automatically | hold **Right Ctrl** | — |
| Force command mode | double-tap and hold the talk key | — |
| Push-to-talk dictation | (talk key) | `Ctrl+Win+Space` (hold) |
| Voice commands (agent) | (talk key) | `Win+Alt+Space` (hold) |
| Circle-to-Ask | talk key + mouse drag, or `Ctrl+Shift+Space` | `Ctrl+Shift+Space` |
| Stop Soul talking (barge-in) | press the talk key | press either talk hotkey |

**Smart routing:** after you release, the transcript becomes a **command** when it starts with the wake word („Soul, …”, „Hej Soul …”), starts with a command verb (otwórz, zamknij, wyszukaj, znajdź, ścisz, podgłośnij, następna, pauza, przypomnij, pokaż, open, close, search… — up to 12 words), or is a short phrase the pre-router knows („następna piosenka”), or is a selected-text action („popraw to”). While the conversation context is alive (see below) a short follow-up („a teraz głośniej”, „a jutro?”, „a ten drugi?”) is a command too. Everything else is **dictation** and gets pasted.

The talk key is read with a low-level hook (`WH_KEYBOARD_LL`/`WH_MOUSE_LL` on Windows, XRecord on X11). It can be any single key or mouse button: Ctrl/Shift/Win/Left Alt, Caps Lock, F1–F24, Pause, Insert, Menu, middle or side mouse buttons (record it in Settings). Taps shorter than 200 ms keep the key's normal behaviour (a swallowed side button is replayed) unless you tick *Block the key's normal action on short taps*; pressing another key while holding it (Right Ctrl+C) cancels the talk. The mic starts on press, but nothing is sent to Gemini until the key has been held for 200 ms. **X11:** XRecord can only observe, so side buttons/F-keys also do their normal action and the drag gesture also reaches the app under the cursor. **Wayland:** no global hooks, so Soul falls back to the separate hotkeys.

**Polish AltGr:** on Polish layouts Right Alt (AltGr) is reported as Ctrl+Alt, so `Ctrl+Alt+<letter>` hotkeys would fire while typing ć, ą, ę… Soul therefore (a) ignores hotkey presses while Right Alt is held (Windows, `GetAsyncKeyState(VK_RMENU)`), (b) rejects `Ctrl+Alt+<letter>` in Settings, and (c) migrates old saved `Ctrl+Alt+…` hotkeys to the defaults above. The talk-key hook ignores the fake Left Ctrl that AltGr generates and refuses Right Alt as the talk key.

### Wake word „Soul” (offline)

Settings → **Głos i dusza → Słowo-klucz „Soul”**: say „Soul” instead of holding a key. Detection runs **only on this computer** ([rustpotter](https://github.com/GiviMAD/rustpotter) 3.0.2, MFCC + DTW against your own samples, no API key); the microphone reaches Gemini only after the word is detected.

1. **Trenuj słowo-klucz** → record 6–8 takes of „Soul” (1.8 s window each) → **Zbuduj słowo-klucz**. Each sample is scored against a model built from the others; re-record outliers. Saved as `wakeword/soul.rpw` in the app data dir.
2. Turn on the switch (or tray → *Listen for “Soul”*). A small mic glyph on the Island shows it's on (bright = listening, dim = paused).
3. **Test** shows the live score against the threshold without starting a turn. **Czułość** (sensitivity) trades misses for false alarms.

After „Soul” it behaves exactly like holding the talk key: start sound, listening state, recording until 1.2 s of silence (8 s cap), then the same smart routing (command vs dictation). Detection pauses while Soul speaks (+0.7 s echo tail), during any recording or processing, while Circle-to-Ask is open, for 2 s after a detection, and — with *Wstrzymuj w grach* (default on) — while a fullscreen app/game is focused (Windows `SHQueryUserNotificationState`, X11 `_NET_WM_STATE_FULLSCREEN`). The listener runs on its own below-normal-priority thread with its own mic stream; measured ≈0.5% of one core (optimized build, 0.02% of a 24-thread CPU). Detections, scores and likely false positives (no speech after the word) are logged as `[soul][wake]`. Offline harness: `npm run test:voice -- --wake` (see below).

### Voice replies & personality

Commands and questions are answered out loud with the Live API's native audio (`responseModalities: AUDIO`, 24 kHz PCM played via cpal). Short confirmations („Otwieram Spotify.”) and longer answers are spoken and shown on the Island with a waveform. Press the talk key while Soul speaks to interrupt it. Settings → **Głos i dusza** (Voice & soul): on/off, voice (Charon, Puck, Kore, Fenrir, Aoede, Leda, Orus, Zephyr) with **Preview**, speaking speed (slow/normal/fast, applied through the prompt; native-audio models have no rate setting), and *Only confirm briefly*; personality presets **Neutralny**, **Ziomek**, **Profesjonalny**, **Sarkastyczny**. They change only the tone of spoken replies; dictation stays neutral. **Głośność głosu Soul** (0–100%, only Soul's voice) and *Ściszaj muzykę, gdy Soul mówi* (other apps drop to 30% during a reply). Replies always use the configured language (Polish by default, `speechConfig.languageCode` = `pl-PL`) — never a language picked from the topic.

UI sound effects are synthesized locally with WebAudio (no bundled assets): listen start/end, success, error, notification, timer. Settings → **Wygląd** → *Zestaw dźwięków*: Futurystyczny / Subtelny / Wyłączony, plus volume and preview.

Circle-to-Ask captures the monitor under the cursor (`xcap`), dims it, lets you drag a region (spring selection ring, a small violet spirit guide docks at it), ask by voice/text, and shows a markdown answer card docked beside the selection with a follow-up thread. Wayland screen capture is stubbed (use X11 for now).

### Voice commands — how to test

1. Open Settings, set your Gemini API key, **Save**.
2. Hold **Right Ctrl** and say „Soul, …” or start with a command verb (or, in *Separate keys* mode, hold **Win+Alt+Space**), then release. Island: Listening… → Thinking… → spoken reply with waveform.
3. Try these (Polish examples):

| Say | Expected |
|-----|----------|
| „otwórz Spotify” | Opens Spotify (Start Menu / `.desktop` fuzzy match) |
| „wyszukaj na YouTube jak ugotować spaghetti” | Opens YouTube search results |
| „ścisz do 30 procent” | Sets system volume to 30% |
| „następna piosenka” | Media next-track key |
| „weź mi znajdź jak ugotować spaghetti” | Filler stripped → search; questions get a short spoken + Island answer (Gemini + Google Search) and, if enabled, the browser opens |
| „mów ciszej” / „mów głośniej” / „wycisz się” | Soul's own voice volume −20% / +20% / 0% |
| „nastaw 10 minut na makaron” | Island countdown, sound + spoken notice when it ends; „anuluj minutnik” |
| „co gra?” | Says the current track |
| „Soul, dzień dobry” | Morning briefing: time, date, timers, notes, now playing |
| „zapamiętaj, że gram na serwerze CityRP” / „zapomnij o serwerze” / „co pamiętasz o grach” | Local memory (SQLite); facts are injected into prompts (max 15) |
| „zanotuj kupić mleko” | Voice note (Plans tab, Settings → Pamięć) |
| „stwórz scenę granie: otwórz Discord, FiveM i Spotify, głośność 40%” / „uruchom scenę granie” | Scenes (macros) with per-step chips on the Island |
| „ścisz o 20” / „podgłośnij o 10” | System volume relative to the current level |
| „zamknij Spotify i otwórz Discord” / „pauza i ścisz o 20” / „włącz scenę granie, a potem ustaw timer na 30 minut” | Chained commands: up to 4 actions in order, stops at the first failure; Island: „✓ Spotify zamknięty · ✓ Discord otwarty” |
| „mów ciszej” → „a teraz głośniej” · „otwórz Discord” → „i zamknij go” · a question → „a jutro?” | Follow-ups (conversation context) |
| „nowy temat” / „zapomnij” | Clears the conversation context |
| select text, then „popraw to” / „przetłumacz to na angielski” / „zrób to bardziej formalne” / „zrób to luźniejsze” / „rozwiń to” | Selected text replaced by the result; „Cofnij” on the Island for 10 s |
| select text, then „streść to” / „wyjaśnij to” | Result on the Island with „Kopiuj” / „Wklej” |

**Conversation context:** Soul keeps the last 3 exchanges (what you said, the tools it ran with their arguments, its answer) in memory only, and forgets them after 90 s without a command (Settings → **Głos i dusza → Kontekst rozmowy**: on/off + timeout). The block is sent with the next command to the Live model and the REST paths, so follow-ups resolve; simple ones („głośniej” after a volume change, „i zamknij go” after opening an app, „anuluj go” after a timer) are resolved locally without the model. A small sky-blue dot on the Island shows it is alive; „nowy temat” / „zapomnij” clears it.

**Chained commands** are split on „i”, „oraz”, „potem”, „a potem”, „następnie”, "and", "then" (and commas) only when every part is a command on its own („otwórz Discord i Spotify” or „timer na godzinę i 20 minut” stay one command). When every part is deterministic Soul runs them itself; otherwise the model gets the whole sentence and may return several function calls. Max 4 actions per utterance; dictation is never split.

**Selected-text actions** read the selection only when you say one of these phrases (never otherwise; the models have no tool for it): Windows simulates Ctrl+C (Ctrl+Insert in terminals) and restores your clipboard right after; X11 reads the PRIMARY selection (clipboard untouched); Wayland is best effort. The text goes to Gemini (Ask model, default `gemini-3.1-flash-lite`, fallback `gemini-3.5-flash-lite`) with formatting and language preserved unless translating; it is never stored in the conversation context or spoken by the Live voice. „Cofnij” switches back to the app and sends its own undo (Ctrl+Z).

`close_app` asks for Confirm/Cancel on an Island card (Esc = Cancel, or hold the talk key and say „tak” / „nie”); the card stays on top until answered. Apps are matched by their program (Discord.exe, steam.exe), closed gracefully, then their whole process tree is ended (tray apps like Discord/Steam/Spotify are quit, not hidden). While a command runs, the Island shows animated tool chips (Otwieram, Głośność…). No arbitrary shell — only the registered tools. Watch the console for `[soul][agent] tool_call` / `tool_result` logs.

### Voice regression tests

`tests/voice/` holds WAV samples + `cases.json` (expected route / tool / args / text). The harness streams each
sample through the real Smart pipeline (Live transcription → smart router → pre-router / Live / REST tools) with
tools in **dry-run** and writes `tests/voice/report.md` (file | transcript | route | tool+args | expected | PASS/FAIL | latency).

```bash
SOUL_TEST_KEY=<key> npm run test:voice
SOUL_TEST_KEY=<key> npm run test:voice:synth   # optional: synthetic Polish samples via Gemini TTS (synth.json)
```

Record real samples in a dev build: Settings → Zaawansowane → *Nagraj próbkę testową*. Details: `tests/voice/README.md`.

Wake word (offline, no key): `npm run test:voice -- --wake` trains on `tests/voice/wake/train/`, replays `positive/`
(clean, +noise, quieter), `positive_other/`, `negative/`, `negative_hard/` and the real recordings above, sweeps
sensitivities and writes `tests/voice/wake/report.md` (detection rate, false positives / hour, CPU).
`SOUL_DRY_RUN=1` makes the running app log agent tools instead of executing them.

Soul Agent harness (real API, real desktop): `npm run test:agent` serves `tests/agent/playground.html` locally,
runs `tests/agent/tasks.json` in your default browser and writes `tests/agent/report.md` (success rate, steps,
model calls, latency); `-- --real` adds Notepad (type + save a temp file) and Settings → Sound, `-- --only=<id>`
runs one task. Uses `SOUL_TEST_KEY` or the key saved in Soul's settings (never printed).

### Dynamic Island — how to test

The Island is a frameless always-on-top pill at the top of the **primary** monitor (replaces the old floating status overlay when enabled).

1. `npm run tauri dev` → Island appears as a black pill (~236×40) with the spirit + clock.
2. **Hover 300 ms** → peek strip (transport controls / status). **Click** or **scroll down** → expanded panel (~640×340) with spring morph. **Click the header**, **mouse leave 400 ms**, **Esc**, **blur** or **scroll up** → collapses.
3. **Now Playing:** start Spotify/Edge media → title/artist/art appear; use Play/Pause/Next/Prev.  
   - Windows: Global System Media Transport Controls  
   - Linux: MPRIS over D-Bus (`zbus`)
4. **Clipboard:** copy ordinary text in another app → item appears; click to re-copy; **Clear** empties history. Paste a fake password (`Sk-abc123XYZ!@#`) → should be skipped.
5. **Plans:** running timers (live countdown, ✕ to cancel) and the latest voice notes.
6. **Quick apps:** type `Spotify` → Add → click chip to launch (same fuzzy index as voice commands).
7. **Quick settings:** drag volume; change mic; toggle quiet mode.
8. **Assistant state:** hold the talk key (Right Ctrl) → amber *Dyktowanie* stage with live waveform → thinking → success + caption strip with the pasted text. Commands use a sky-blue stage; spoken replies show the spirit speaking with a waveform and the reply text.
9. **Settings → Wyspa:** disable, change position (left/center/right), toggle tabs → Save.
10. **Settings → Wygląd:** accent, spirit size, glow, opacity, blur, animation preset (Pełne / Zrównoważone / Minimalne / Wyłączone), per-feature toggles, speed/stiffness, battery saver, FPS cap, quiet mode, result duration, sound effects. Applied live in every window and saved automatically.

**Wayland:** Island falls back to a normal top-anchored always-on-top window. Expect limited transparency/blur, no true click-through outside the pill, and compositor-dependent always-on-top behavior.

### Memory & scenes

- **Settings → Pamięć:** list / search / edit / delete / clear all remembered facts and notes, toggle *Zapamiętuj automatycznie* (facts stated in commands, e.g. „gram na serwerze X”, are saved with a „Zapamiętałem: …” chip and **Cofnij**). Stored in `soul-memory.db` in the app data dir; passwords and keys are refused.
- **Settings → Sceny:** ordered steps (open app / link, YouTube, search, system volume, media, delay, Do Not Disturb) and trigger phrases („gramy w FiveM”). When the same 2+ apps are opened together by voice 3 times, the Island suggests a scene. Do Not Disturb = Windows toast banners off / GNOME `show-banners` or dunst on Linux.

### Soul Agent — computer control

Say a UI task: „kliknij Zapisz”, „przewiń w dół”, „wpisz w wyszukiwarkę FiveM serwery”, „zaznacz newsletter”,
„wypełnij formularz: imię Anna, miasto Łódź”, „w ustawieniach przejdź do strony Dźwięk”, „wyślij Silverowi na
Discordzie: będę za 10 minut”, then „a teraz kliknij drugi”. Soul reads the active window's accessibility tree
(Windows UI Automation; Linux X11: AT-SPI, best effort), picks one action at a time (Gemini function calling,
`gemini-3.1-flash-lite` → fallback `gemini-3.5-flash-lite`, configurable), verifies it and continues (≤ 20 steps,
90 s). A screenshot with numbered boxes is sent only on the first step, in vision mode (games / canvas apps without
accessibility) or after a failed step. Simple media / volume / timer / open-close commands keep their fast path.

- **Confirmations** before sending, buying, deleting, discarding work or changing system settings (button names
  like Wyślij / Kup / Usuń force one even if the AI didn't ask). Answer with the buttons or a spoken „tak” / „nie”.
- **Never:** passwords or API keys, password fields, blocklisted apps / sites (password managers, banks, Windows
  Security, UAC, terminals, Run dialog — Settings → Agent), Win+R / Win+X, fullscreen games unless you name them.
  On-screen text is treated as data, never as instructions.
- **Stop:** Esc twice, the talk key, the mouse jammed into a screen corner, or **Zatrzymaj** on the Island.
- Wayland: not supported (clear message). Test harness: `npm run test:agent [-- --real]` → `tests/agent/report.md`
  (takes over mouse and keyboard for a few minutes). Design: [`docs/agent.md`](docs/agent.md).

## Install

Download the latest installer from **[GitHub Releases](https://github.com/OsPross/Soul/releases/latest)**.

| System | File | Updates |
|--------|------|---------|
| Windows 10/11 | `Soul_<version>_x64-setup.exe` (recommended) | automatic |
| Windows (IT / Group Policy) | `Soul_<version>_x64_en-US.msi` | automatic |
| Any Linux (X11) | `Soul_<version>_amd64.AppImage` — `chmod +x`, run | automatic |
| Ubuntu / Debian / Mint | `Soul_<version>_amd64.deb` — `sudo apt install ./Soul_<version>_amd64.deb` | download the new `.deb` (Soul shows a link) |
| Fedora / RHEL / openSUSE | `Soul-<version>-1.x86_64.rpm` — `sudo dnf install ./Soul-<version>-1.x86_64.rpm` | download the new `.rpm` (Soul shows a link) |

After installing, open **Settings** from the tray icon, paste your Gemini API key and **Save**.

**Windows SmartScreen:** the builds are not code-signed yet, so Windows may show *“Windows protected your PC”*.
Click **More info → Run anyway**. The installer is per-user (no admin prompt) and installs to
`%LOCALAPPDATA%\Soul`. The update files themselves are signed with Soul's updater key, so the
in-app updater only accepts genuine releases.

**Linux runtime packages:** the `.deb`/`.rpm` pull in WebKitGTK 4.1, ALSA and libayatana-appindicator
automatically; the AppImage bundles them. On GNOME you need the *AppIndicator* extension to see the tray icon.

### Updates

Settings → **O aplikacji / About**: current version, **Sprawdź aktualizacje** (check now), the changelog of
the new release, a download progress bar and **Uruchom ponownie** (restart to install).

- **Automatyczne aktualizacje** (default on): checks 30 s after start and every 6 h, downloads in the
  background, then shows *„Nowa wersja vX.Y — Zainstaluj”* on the Island. Nothing installs until you click,
  and never while Soul is recording, speaking or answering — the install waits until it's idle.
- **Kanał** *Stabilny* / *Beta*: beta also receives prereleases (`v0.3.0-beta.1`).
- `.deb` / `.rpm` installs are owned by the package manager, so Soul only links to the new package.
- Updates are signed (minisign); a download with a wrong signature is rejected.

### Your data

Settings (`soul-settings.json`, incl. the API key), memory (`soul-memory.db`) and the trained wake word
(`wakeword/`) live in the app data dir, never next to the program, so they survive updates and reinstalls:

- Windows: `%APPDATA%\com.oskar.soul\`
- Linux: `~/.local/share/com.oskar.soul/`

The Windows uninstaller keeps them unless you tick **“Delete the application data”**; MSI, `.deb`, `.rpm`
and the AppImage never touch them. To reset Soul completely, delete that folder.

## Prerequisites (building from source)

### All platforms

- [Node.js](https://nodejs.org/) 20+
- [Rust](https://rustup.rs/) (stable)
- A [Gemini API key](https://aistudio.google.com/apikey)

### Windows

- [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the **Desktop development with C++** workload (MSVC + Windows SDK)
- [WebView2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) (preinstalled on Windows 11)

### Linux

System packages vary by distro. On Debian/Ubuntu:

```bash
sudo apt update
sudo apt install -y \
  libwebkit2gtk-4.1-dev \
  build-essential \
  curl \
  wget \
  file \
  libxdo-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libasound2-dev \
  pkg-config
```

Full list used by CI (Ubuntu 22.04): `scripts/ci-linux-deps.sh`. On Fedora:

```bash
sudo dnf install -y webkit2gtk4.1-devel openssl-devel curl wget file libappindicator-gtk3-devel \
  librsvg2-devel alsa-lib-devel libxdo-devel rpm-build
sudo dnf group install -y c-development
```

Optional paste helpers:

- **X11:** `xdotool` (fallback if `enigo` is flaky)
- **Wayland:** `wtype` or `ydotool` (without these, Soul copies text to the clipboard and shows a notice)

## Develop

```bash
npm install
npm run tauri dev
```

On first launch the overlay stays hidden. Open **Settings** from the system tray (left-click the tray icon, or tray menu → Settings), paste your Gemini API key, optionally click **Test key**, then **Save**.

Default talk key: **Right Ctrl**. Hold to speak, release to paste or run a command. In *Separate keys* mode dictation is **Ctrl+Win+Space** and commands are **Win+Alt+Space**.

## Build

```bash
npm run tauri build                                   # all installers for this OS, unsigned updater artifacts
npm run tauri build -- --bundles nsis                 # just one format (nsis, msi, deb, rpm, appimage)
```

Installers land in `src-tauri/target/release/bundle/` (`nsis/`, `msi/`, `deb/`, `rpm/`, `appimage/`).
Building `.rpm` needs the `rpm` tool (`sudo apt install rpm`); `scripts/ci-linux-deps.sh` installs everything
CI uses on Ubuntu 22.04. Updater `.sig` files are only produced with the release config and the signing key:

```bash
TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/soul-updater.key)" \
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="…" \
npm run tauri build -- --config src-tauri/tauri.release.conf.json
```

## Release

Version lives in `package.json`; `tauri.conf.json` reads it (`"version": "../package.json"`) and
`npm run version:sync` mirrors it into `src-tauri/Cargo.toml` / `Cargo.lock` (CI fails if they differ).

1. Write what changed under `## [Unreleased]` in `CHANGELOG.md` (or leave it empty to use the commit subjects).
2. `npm run release -- 0.2.0` (beta: `npm run release -- 0.3.0-beta.1`) — bumps all versions, moves the
   changelog section, commits `Release v0.2.0` and tags `v0.2.0`. Nothing is pushed.
3. Push what it prints: `git push origin main v0.2.0`.
4. **Release** workflow (`.github/workflows/release.yml`) builds Windows (NSIS + MSI) and Linux
   (`.deb`, `.rpm`, `.AppImage`) on `windows-latest` / `ubuntu-22.04`, signs the updater artifacts and creates a
   **draft** GitHub Release with the installers, `.sig` files and `latest.json`.
5. Check the draft on GitHub → **Publish release**. From then on installed apps see the update
   (`https://github.com/OsPross/Soul/releases/latest/download/latest.json`; betas via the GitHub API).

Betas are marked *prerelease* and ship only the NSIS installer on Windows (MSI can't encode `-beta.1`).

**One-time setup — updater signing key** (already generated; the public key is in `tauri.conf.json`):

- Repository → Settings → Secrets and variables → Actions → *New repository secret*:
  - `TAURI_SIGNING_PRIVATE_KEY` = the full contents of `~/.tauri/soul-updater.key`
  - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` = the contents of `~/.tauri/soul-updater.key.password`
- Back up both files (password manager + offline copy). **If the private key is lost, installed copies can
  never auto-update again** — users would have to reinstall manually. Never commit it.

CI (`.github/workflows/ci.yml`, every push / PR, Windows + Ubuntu 22.04): version check, frontend
typecheck + build, `cargo check`, `cargo clippy -D warnings`, router + all unit tests.

### Code signing (later, optional)

Unsigned Windows builds trigger SmartScreen until they gain reputation. To sign:

- Buy an OV/EV code-signing certificate (or use Azure Trusted Signing / SignPath for open source).
- Set `bundle.windows.certificateThumbprint` + `digestAlgorithm: "sha256"` + `timestampUrl`
  (local `.pfx` imported on the runner), **or** `bundle.windows.signCommand` for a cloud signer
  (e.g. `"signCommand": "trusted-signing-cli -e https://… -a <account> -c <profile> %1"`).
- Add the certificate / credentials as GitHub secrets and import them in `release.yml` before `tauri-action`.

This is independent of the updater key above (that one only signs update packages).

## Architecture

```
src/                     React UI (island + overlay + settings + ask)
src-tauri/src/
  lib.rs                 App setup, tray, hotkeys, dictation / smart routing
  talk.rs                Smart talk key worker (tap / hold / double-tap / chord / drag)
  voice.rs               Spoken-reply playback (cpal), barge-in, waveform level
  island/                Dynamic Island window, media, clipboard
  agent/                 Voice-command session, tool dispatch, pre-router, smart intent
  memory.rs              Local SQLite memory (facts, notes, scenes, app launches)
  scenes.rs              Scenes (macros), runner, suggestions
  timers.rs              Voice timers / reminders
  ask.rs                 Circle-to-Ask flow
  config.rs              Local settings (tauri-plugin-store)
  audio.rs               Mic capture → 16 kHz mono WAV + streamed PCM chunks (cpal)
  ai/gemini.rs           REST: dictation fallback, Ask, agent function calling, models list
  ai/live.rs             Live API sessions (tokio-tungstenite, rustls)
  ai/cleanup.rs          Dictation cleanup (fillers, self-corrections)
  platform/              paste, open_app, volume, media, URLs, input hooks (hook.rs + backends)
```

## Settings

Stored locally in the app data dir as `soul-settings.json` (API key, hotkey, model, language) — see
[Your data](#your-data). The API key is never written to logs.

## License

Private / WIP.
