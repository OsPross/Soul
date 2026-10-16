# Soul Agent — voice-driven computer control

Status: implemented in four phases (perception + input → loop + router → safety + UX → harness).
This document is the design; `CLAUDE.md` has the short version for day-to-day work.

User says „kliknij Zapisz”, „przewiń w dół”, „wpisz w wyszukiwarkę FiveM serwery”, „otwórz ustawienia
dźwięku i ustaw mikrofon na Voicemeeter”, „wyślij Silverowi na Discordzie: będę za 10 minut”. Soul
observes the focused window, lets a model pick **one** UI action at a time, executes it, verifies the
change, and repeats until the task is done, the user stops it, or a limit is hit.

## Goals and non-goals

- Reliable and safe before clever: one action per model call, every action verified, hard limits
  (20 steps, 90 s, 20 s per step, 9 s per model request), local safety checks the model cannot talk its way around.
- Cheap on the free tier: accessibility text every step, a screenshot only when needed, a
  deterministic local plan for trivial tasks (0 calls), and a `final` flag so one-shot tasks
  finish without a separate `done` call.
- Not a general shell: no command execution, terminals/Run dialog/registry editor are blocklisted
  windows, `Win+R`/`Win+X` are refused key combos.

## Module map

```
src-tauri/src/
  platform/
    ui.rs            portable: UiElement / UiSnapshot / UiAction types, role table, compact
                     observation text, "is the tree useless" heuristic, password redaction
    coords.rs        portable: 0–1000 ↔ physical pixels, screenshot scale, monitor offsets,
                     DPI (logical ↔ physical), SendInput absolute (0–65535) mapping, corners
    input.rs         portable: key-combo parser, UTF-16 unicode key units (Polish, surrogates),
                     scroll/drag planning, refused combos; executes through the Platform trait
    marks.rs         portable: screenshot crop/downscale, set-of-marks boxes + digit labels, JPEG
    windows_uia.rs   Windows: UI Automation worker thread (COM objects never leave it), cached
                     FindAll, patterns (Invoke/Value/Toggle/SelectionItem/ExpandCollapse/Scroll),
                     SendInput mouse/keyboard (KEYEVENTF_UNICODE), kill-switch probe
    linux_atspi.rs   X11: AT-SPI over D-Bus (zbus, best effort) + enigo input; Wayland: error
  agent/
    computer/mod.rs     the loop: guard → observe → decide → check → act → settle → verify
    computer/intent.rs  computer_task detection (Polish + English UI verbs) + local plans
    computer/safety.rs  risk keywords/roles, blocklist, secrets, refused combos, verdicts
    computer/prompt.rs  system prompt, function declarations, request body, response parsing
    computer/config.rs  AgentConfig (store key `computerAgent`), autosaved from Settings → Agent
    computer/killswitch.rs  double Esc / talk key / corner / "stop" → abort flag; pure detector
    computer/harness.rs opt-in harness (`npm run test:agent`) + real-task probes
    computer/vio.rs     Vio browser automation API: discovery, WebSocket actor, page → snapshot
    computer/vio_mock.rs  mock Vio server + fake desktop; Vio cases (cargo test + harness)
src/
  island/AgentStage.tsx   Island "agent" card: spirit (thinking), step text, n/20, progress, Stop
  agent/Highlight.tsx     click-through ring overlay (window `agent-highlight`)
  settings/AgentPage.tsx  Settings → Agent
tests/agent/
  playground.html  buttons, inputs, scroll list, fake „Wyślij”, hidden prompt injection
  tasks.json       scripted harness tasks + expectations
  vio-mock.json    mock Vio tabs / elements + Vio cases (scripted calls for cargo test)
  report.md        written by the harness
```

## 1. Perception (accessibility first)

`Platform::ui_snapshot(opts) -> UiSnapshot`:

```
UiSnapshot { gen, window: WindowInfo { title, process, pid, rect, monitor, fullscreen },
             elements: Vec<UiElement>, source: Uia | Atspi | None, took_ms }
UiElement  { id (1..), role, name, value, rect, enabled, focused, automation_id,
             password, offscreen, actions: {invoke, value, toggle, select, expand, scroll},
             expanded: Option<bool>, checked: Option<bool> }
```

Windows (`windows_uia.rs`):
- All UIA calls run on one dedicated **UIA worker thread** (MTA). COM interface pointers are not
  `Send`; the worker keeps the last snapshot's `IUIAutomationElement`s in a `Vec` indexed by id,
  so a later `click(id)` acts on exactly the element the model saw. Callers wait with a timeout;
  a worker stuck in a hung app is abandoned and a fresh one is started for the next request.
- `IUIAutomation2` connection/transaction timeouts (2 s) bound every cross-process call.
- One `FindAllBuildCache(TreeScope_Subtree)` on the foreground window with a cache request for
  every property we need (control type, name, value, rect, enabled, focus, automation id,
  IsPassword, IsOffscreen, pattern availability, toggle/expand state). Condition: control view
  ∧ not offscreen ∧ interactive control type (button, edit, link, checkbox, radio, combo, list
  item, menu item, tab item, tree item, slider, spinner, split button, document, text with name,
  data item, header item, hyperlink). One round trip instead of thousands.
- Filtering: zero-size / outside-window rects dropped; deduplicated by rect+name; ordered in
  reading order (row bands of 12 px, then x); capped at 300 (focused element always kept).
- Ids are 1-based and stable only within one snapshot (`gen`); actions name `gen` + id.

Screenshot (`marks.rs`): the monitor under the window is captured with the existing capture code,
cropped to the window rect (clipped to the monitor), downscaled to ≤ 1280 px on the long side,
annotated with numbered boxes for each element (set-of-marks; built-in 5×7 digit font, no font
files) and JPEG-encoded at quality 70. Typical size 60–180 KB. Never written to disk unless the
opt-in debug mode is on (app data dir `agent-debug/`, cleared at the start of each task).

Vision fallback: when the tree is useless (fewer than 3 named interactive elements, or one
document/pane covering ≥ 80 % of the window with no children — games, canvas, Electron without
a11y), the observation says `mode: vision` and the model answers with `x`/`y` in 0–1000 relative
to the screenshot. `coords::norm_to_screen` maps that back: crop origin + norm × crop size, in
physical virtual-desktop pixels (negative origins of left/top monitors handled). Soul runs
per-monitor-DPI-aware (Tauri manifest; the worker/test threads set it too), so UIA rects, the
capture and SendInput all use physical pixels; `coords` converts logical↔physical where a
window position is set in logical units (highlight overlay) and is unit-tested at 100/125/150 %.

Linux X11 (`linux_atspi.rs`): AT-SPI over the accessibility bus (`org.a11y.Bus.GetAddress`),
find the application frame with state ACTIVE, walk children breadth-first (depth ≤ 12, ≤ 1500
nodes visited, ≤ 300 kept) reading role, name, states, `Component.GetExtents(SCREEN)` and
`Action`/`Text`/`Value` presence. Actions: `Action.DoAction(0)` for click when available, else
enigo mouse; typing/keys via enigo (XTest). Without AT-SPI (bus missing, app not exposing it)
the snapshot is empty → vision mode. Wayland returns an error with a clear message.

## 2. Action layer

`UiAction` (portable, parsed from the model's function call and validated):
`click | double_click | right_click (id | x,y)`, `type_text (id?, text, clear?, submit?)`,
`key_combo (keys)`, `scroll (direction, amount 1–10, id?)`, `drag (from, to)`, `focus_app (name)`,
`open_url (url)`, `wait (ms ≤ 5000)`, `read_text (id)`, `done (summary)`, `ask_user (question)`.
Every acting function also carries `risky: bool` (model's own flag) and `final: bool` (this action
completes the task).

Strategies (Windows), first → retry:
| action | primary | retry |
|---|---|---|
| click(id) | Invoke / Toggle / SelectionItem.Select / ExpandCollapse | SendInput click at rect centre |
| click(id) without patterns | SendInput click | Invoke (if any) or focus + Enter |
| type_text, native single-line field | ValuePattern.SetValue | clipboard paste (then Unicode keys) |
| type_text, other native (documents…) | clipboard paste, previous clipboard restored | Unicode keys |
| type_text, web / Electron (FrameworkId Chrome) | Unicode keys (KEYEVENTF_UNICODE) | clipboard paste |
| scroll | ScrollPattern on the element / nearest scrollable | mouse wheel at element/window centre |
| right/double click, drag, x/y | SendInput | — |

Typing: `input::type_units(text)` → UTF-16 code units, one `KEYEVENTF_UNICODE` down/up per
unit (surrogate pairs as two units), newline → Enter, tab → Tab. Never depends on the keyboard
layout, so Polish AltGr layouts and ą/ć/ę/ł/ń/ó/ś/ź/ż work. Chunks of 16 units, abort checked
between chunks. Modifier keys are always released (also on abort / error).

Found while testing: Windows 11 Notepad's text input resolves queued Unicode key packets late —
a burst arrives as N copies of the newest character („Zażółć źźźźźźźźź…”), with 15 ms gaps and
split down/up events too. Chromium is flawless. So native apps get the text through the clipboard
(`Platform::paste_text`, previous clipboard restored — equally layout-independent), web content
keeps Unicode key events, and each is the other's fallback.

After every action: settle (poll a cheap fingerprint — focused element, window title, element
count + names hash, value of the target — every 120 ms until two equal reads or 2 s), re-perceive,
verify against the action's expectation:
- click: tree fingerprint changed, or focus moved, or window changed, or a toggle/expand state
  flipped (or the model said `expect` and its text appeared);
- type_text: target value contains the typed text (fields that hide their value: focus check);
- scroll: fingerprint changed or the scroll percentage moved;
- key_combo/wait/focus_app/open_url/drag: no hard check (fingerprint is reported to the model).
On failure → one retry with the other strategy → still failing → the step is reported as failed in
the history, the next observation includes a screenshot, and 3 failures in a row stop the task.

## 3. Agent loop (`agent/computer`)

```
guard: enabled, not Wayland, foreground not blocklisted, not a fullscreen game (unless named)
for step in 1..=max_steps (≤ 20), total ≤ 90 s:
    abort?  → stop
    snapshot (≤ 4 s)                      blocklist re-checked every step
    local plan?  (step 1 only: „kliknij X” with exactly one exact match, „przewiń w dół”)
    else model call (≤ 20 s, 9 s per model request, abortable):  system + task + [CONTEXT] + history (last 8 steps)
                                          + observation text (+ JPEG when needed)
    parse ONE function call → UiAction → validate (id exists, not password, not a secret)
    safety verdict: Allow | Confirm(reason) | Refuse(reason)
    confirm → Island card + voice yes/no (talk key); declined → stop
    highlight ring (optional, 350 ms) → execute (abortable) → settle → verify (→ retry once)
    final && verified → done
```

Model: REST `generateContent` with function calling, `mode: ANY` (exactly one call per step),
temperature 0, minimal thinking. Model id = `computerAgent.model`, empty = the Ask model
(`gemini-3.1-flash-lite`), fallback = `resolve_fallback_model` (`gemini-3.5-flash-lite`), same
404 healing / quota tracking as every REST call.

Screenshot policy: first step, vision mode, after a failed verification, when the model calls
`read_text`/acts on something not in the tree… concretely: `needs_image = step == 1 || vision ||
last_failed || model_asked (look: true)`. Everything else is text only (~2–6 KB per step).

Each step is a fresh single-turn request (no growing transcript): the history is a compact list
„3. click [12] „Zapisz” → ok, window title → «Bez tytułu — Notatnik»”.

Context: the exchange is stored in the conversation context (`computer_task` tool, task + summary),
so „a teraz kliknij drugi” routes to the agent with the previous task in `[CONTEXT]`.

## 4. Router

`agent/computer/intent.rs::is_computer_task(text)` runs in `local_dispatch` **before** chains and
the pre-router. True when (after fillers like „weź”, „a teraz”):
- a strong UI verb starts the utterance or a chain part: kliknij/kliknąć, dwuklik, przewiń
  (not „przewiń piosenkę/utwór”), wpisz, zaznacz/odznacz, wypełnij, naciśnij/wciśnij (not media),
  zaznacz, rozwiń/zwiń, click, double-click, right-click, scroll, fill in, type … into/in;
- „przejdź do …” / „go to …” that the pre-router does not route (media „przejdź do następnej” stays);
- „wyślij/napisz … na/w/przez Discordzie/Messengerze/WhatsAppie/Slacku/Teams/Telegramie/mailem”;
- „w ustawieniach …”, „ustaw/zmień X w ustawieniach”, „otwórz ustawienia X i …”, „ustaw mikrofon/
  głośnik/wyjście/wejście na …” (device settings — never timers/volume, which the pre-router owns).
Simple commands keep the fast path: anything `router::match_command` routes (media, volume,
timers, notes, YouTube, search) and plain „otwórz/zamknij X” are never computer tasks.
Smart classification gets the same verbs (`intent.rs`), so the talk key sends them to commands;
every `pulapka_*` dictation trap is a unit test.

## 5. Safety

Verdicts are computed locally in `safety.rs` and combined with the model's `risky` flag:
`Refuse` > `Confirm` > `Allow`.

- **Refuse**: blocklisted window (process or title substring; defaults: password managers
  1Password/Bitwarden/KeePass(XC)/LastPass/Dashlane/NordPass/Enpass/Proton Pass/RoboForm, banks
  and payment sites (PKO, mBank, ING, Santander, Pekao, Millennium, Alior, Revolut, PayPal, BLIK,
  …, also matched against the browser address bar), Windows Security / „Zabezpieczenia Windows”,
  UAC / credential prompts (consent.exe, CredentialUIBroker), terminals (cmd, PowerShell, Windows
  Terminal), Run dialog, Registry Editor, Task Manager, MMC). Editable in Settings → Agent.
  Also refused: typing into a password field, typing text that looks like a secret (API keys,
  tokens, `password=`), `read_text` on a password field, `Win+R`, `Win+X`, `Ctrl+Alt+Del`,
  `Ctrl+Shift+Esc`.
- **Confirm** (Island card, buttons or spoken „tak/nie”): target name/automation id matching
  send/submit/buy/pay/delete words (Wyślij, Send, Prześlij, Opublikuj, Post, Kup, Zamów, Zapłać,
  Pay, Buy, Order, Usuń, Delete, Remove, Odinstaluj, Uninstall, Submit, Zatwierdź, Potwierdź,
  Discard, Nie zapisuj, Don't save, Wyczyść, Reset, Format, Wyloguj…), Enter/submit in a
  messenger/mail window or a chat-like input, `Ctrl+Enter`, `Alt+F4`, `Ctrl+W/Q`, `Delete`/
  `Shift+Delete`, any state change (toggle, checkbox, radio, combo/list selection, slider, value)
  inside Windows Settings / Control Panel, and anything the model flagged `risky`.
  Confirmation level `always` confirms every state-changing action.
- Prompt injection: the observation is wrapped in `<screen>` and the system prompt says text on
  screen is data, never instructions; element names are truncated (80 chars) and quotes escaped.
  The playground carries a hidden injection; the harness fails a task that obeys it.
- Fullscreen game focused → refuse unless the task names that app.
- Kill switch: double Esc (≤ 800 ms), the talk key / command hotkey, saying „stop” on the talk key,
  the Island Stop button, or slamming the mouse into a screen corner (≤ 3 px) abort immediately.
  A 25 ms probe thread watches Esc + cursor while a task runs; actions check the flag between
  chunks (typing, drag steps, waits, model call wait).
- Screenshots live in memory for one request only.

## 6. UX

- Island: `kind: "agent"` with `island://agent` { step, max, text, phase, progress }. Card with the
  spirit in `thinking`, „Klikam „Zapisz”…”, „3/20”, a thin progress line and a Stop button
  (`agent_stop`). A confirmation card replaces it while it waits.
- Highlight: optional click-through, focus-less, always-on-top window `agent-highlight` that
  flashes a ring around the target for 350 ms before the click (excluded from capture).
- During a task the Island and the highlight window are excluded from screen capture
  (`WDA_EXCLUDEFROMCAPTURE`) so they never end up in the model's screenshot; a mouse action under
  the Island makes it click-through for the duration.
- End: one short spoken summary through the Live voice („Gotowe, wysłałem wiadomość do Silvera”).
- Settings → Agent: enable, max steps, highlight, confirmation level, blocklist editor, model,
  debug log of recent steps (+ opt-in screenshot dump).

## 6b. Vio browser (automation API)

When the user's Electron browser **Vio** has its automation API on, browser steps go through it
instead of UI Automation + screenshots (faster, exact, no mouse).

- **Discovery:** Vio writes `{port, token, pid}` to `%APPDATA%\moja-przegladarka\automation.json`
  (Linux `~/.config/moja-przegladarka/automation.json`) only while the API is on. `vio::init` (app
  setup, only while the agent is enabled) starts an actor on Tauri's tokio runtime that polls the
  file every 2 s, checks the pid (`platform::pid_alive`), connects to `ws://127.0.0.1:port` (no Origin
  header) and sends `{cmd:"auth", token}`. Vio may show a consent dialog: Soul waits up to 30 s and
  shows „Zezwól w Vio” on the Island (toast; inside a task also the agent card). A refused / unanswered
  auth is not retried for that token (no dialog spam) until the file changes or the agent is
  switched off and on. The connection drops when the file disappears or the pid dies and comes back
  when it reappears. Settings → Agent shows „Przeglądarka Vio: połączona / niedostępna”
  (`agent_vio_status`).
- **Protocol:** `{id, cmd, args}` → `{id, ok, result|error}`; `Client::call` is blocking for the
  agent thread, 10 s per call, aborted by the kill switch, ≥ 50 ms between commands (Vio allows
  20/s). The token is never logged and `gemini::redact_secrets` scrubs it (`vio::redact`).
- **When:** per step — Vio connected and (the foreground window is Vio's pid / `vio.exe`, or the task
  names the browser: „w przeglądarce”, „karta”, „Vio”, `intent::names_browser`). Otherwise the
  normal UIA path; if `listTabs` / `getElements` fail (other than „forbidden”) the step falls back
  to UIA too.
- **Observation:** `listTabs` → active tab, `getElements` → `UiSnapshot { source: Vio }` (ref in
  `automation_id`, title = „title — URL” so the blocklist sees bank domains, no rects, no
  screenshot). Lines carry `empty` for empty form fields („sprawdź które pola są puste” is answered
  from the list). Ids are renumbered every step, so expired refs heal on the next step.
- **Tools (MODE browser):** `browser_list_tabs`, `browser_open {url | tab_id}` (openTab /
  activateTab — wakes sleeping tabs), `browser_navigate`, `browser_read_page` (getPage → READ, up to
  8000 chars), `browser_get_elements`, `browser_click {id}`, `browser_fill {id, text, submit}`,
  `browser_key` (focuses Vio's window first), `browser_scroll up|down`, plus open_app / focus_app /
  open_settings / wait / done / ask_user. Click / fill / key / scroll parse into the ordinary
  actions, so `local_plan`, `safety::check_step` (Wyślij / Kup / Usuń / **Zaloguj** confirm, password
  fields and secrets refused), the 20-step limit, history and the kill switch are unchanged. URLs:
  http(s) only (`vio::normalize_url`).
- **„forbidden”** (private / Tor / internal tabs, password fields) ends the task with a clear message
  — never worked around, never retried through the screen.
- **Voice:** while Vio is connected `intent::is_browser_phrase` makes „otwórz w przeglądarce X”,
  „przeczytaj / streść tę stronę”, „wypełnij formularz”, „sprawdź które pola są puste” computer
  tasks (Smart talk key: ≤ 8 words).

## 7. Testing

- Unit: router intents + traps, risk verdicts, blocklist, coordinate mapping (DPI 100/125/150,
  multi-monitor with negative origins), unicode units for Polish text, combo parsing, observation
  format, local plans, kill-switch detector, response parsing.
- `tests/agent/playground.html` served by the harness on 127.0.0.1; page state is POSTed back
  so expectations are checked exactly. `npm run test:agent` (needs `SOUL_TEST_KEY`, or reads the
  key from the local Soul settings without printing it) runs `tests/agent/tasks.json` and writes
  `tests/agent/report.md` (success rate, steps, model calls, latency).
- Real tasks (opt-in, Windows): Notepad (type + save to a temp file), Settings (go to Sound),
  playground — `npm run test:agent -- --real`.
- Vio: `vio_mock.rs` serves `tests/agent/vio-mock.json` over a real WebSocket on 127.0.0.1 with a
  fake desktop (Vio in front, no UIA, no input). Cases: auth failure, forbidden tab, expired ref,
  empty fields, Polish fill, page read, send confirmation, password refusal. `cargo test
  vio_mock_cases` replays scripted model calls (CI); `npm run test:agent` runs them with the real
  model and adds a „Vio browser API (mock server)” section to `tests/agent/report.md`.

## API calls per task (measured, `tests/agent/report.md`, 2026-10-16)

| task | model calls | screenshots | time |
|---|---|---|---|
| „kliknij Zapisz” (unique match) | 0 (local plan) | 0 | 0.4 s |
| „wpisz Oskar w pole Imię” | 1 | 1 | 3 s |
| „wpisz w wyszukiwarkę FiveM serwery” | 2 | 1 | 4 s |
| „zaznacz newsletter” / „wybierz kolor Niebieski” | 1–2 | 1 | 3–8 s |
| „przewiń listę serwerów i kliknij Serwer 42” | 8 | 1 | 14 s |
| Notepad: type Polish text + save as a temp file | 4 | 1 | 7–8 s |
| Settings: main page → System → Dźwięk | 2 | 1 | 4 s |

Playground average 2.3 calls / task, ~150–300 KB request per call with a screenshot, ~5–60 KB
without. Each step is one `generateContent` request, so a free-tier key (≈ 15 requests/min on
Flash-Lite) handles a typical task but not a burst of long ones.

Known model behaviour: Flash-Lite sometimes flags harmless steps (saving a file) as `risky`; the
safety layer honours the flag, so the user sees one extra confirmation. The prompt now states
what is not risky, which reduced but did not eliminate it.
