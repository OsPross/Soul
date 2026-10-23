# Changelog

All notable changes to Soul are listed here. `npm run release -- <version>` moves the
**Unreleased** section under the new version; that text becomes the GitHub Release notes and the
changelog shown in Settings → O aplikacji / About.

## [Unreleased]

### Added

- **Agenci kodujący na wyspie (Claude Code, Codex, Cursor, Gemini CLI, Copilot CLI).** Soul can
  install `soul --agent-hook` hooks into each agent's config (diff preview, dated backup, your other
  hooks and status line untouched). The hook never blocks the agent: without Soul it exits at once.
  Permission prompts from Claude Code / Codex / Copilot show on the Island: click Zezwól / Odrzuć or
  hold the talk key and say „tak” / „nie”; „W terminalu” hands the prompt back. Claude Code's
  questions get option chips. The Island shows what the agent is doing („Claude · Edytuje · mod.rs
  +12 −4”), and the new **Agenci** tab lists sessions with ↗ (jump to the terminal) and a Live Diff
  of every edited file.
- **Limity planów Claude Code i Codex (Settings → Wyspa → Limity planów).** Pills in the expanded
  Island header („● Claude 73%”, „● Codex 18%”) open a card with the 5-hour and weekly bars, reset
  times and Codex free resets. Claude Code reports limits only to its status line, so Soul offers a
  one-click statusLine relay with a preview of the `~/.claude/settings.json` change and a dated
  backup; your existing status line keeps working and comes back on uninstall.
- **Upuść plik na wyspę.** Drag a file or screenshot (image, PDF, text, code, CSV, JSON, logs, up to
  20 MB) from Explorer onto the Island, then ask about it by typing or by holding the talk key. The
  answer shows on the Island with Kopiuj / Wklej. Dropped files are copied into Soul's own inbox and
  removed after 7 days.
- **Kula 3D (Settings → Wygląd → Styl duszy).** A WebGL particle orb as an alternative to the droplet
  spirit: a rippling sphere while listening / speaking, a knot while thinking, sonar rings for music
  and a drawn-in checkmark on success. Falls back to the droplet without WebGL.
- **Słownik i snippety (Settings → Pamięć).** Dictation keeps your dictionary words spelled exactly,
  applies word replacements („cloud code” → „Claude Code”), and a spoken snippet phrase („mój podpis”)
  pastes its whole text unchanged.
- **Soul Agent + Vio browser.** When the Vio browser's automation API is on, browser tasks go
  through it instead of the screen: faster and exact, no mouse. „otwórz w przeglądarce
  youtube.com”, „przeczytaj / streść tę stronę”, „wypełnij formularz”, „sprawdź które pola są
  puste” work while Vio is connected. Vio may ask for consent on the first connection — the Island
  shows „Zezwól w Vio”. Sending, buying, deleting and **logging in** still need a confirmation;
  private / Tor / internal tabs and password fields are reported, never worked around. Settings →
  Agent shows „Przeglądarka Vio: połączona / niedostępna”.

- **Forms in Vio, faster.** The agent fills a whole form in one or two model calls
  (`browser_fill_many`), chooses `<select>` options (`browser_select`), sets time / date / number
  fields, and sees fields below the fold without scrolling. Dictation and „popraw / przetłumacz to”
  in Vio now insert text with Vio's own API instead of borrowing the clipboard.

- **Vio API v2.** Soul says `hello` to Vio, follows its events (active tab, focus, selection, focused
  field, playing media) and shows its state on the Soul icon in Vio (listening / thinking / speaking /
  acting). Clicking the icon starts listening; **STOP** in Vio stops everything at once (agent, voice,
  waiting browser commands). New voice commands, no model needed: „przeczytaj artykuł”, „znajdź na
  stronie X”, „przewiń do X”, „pauza” / „graj” / „przewiń o 30 sekund” / „szybciej” / „wolniej” /
  „głośniej” / „ciszej” on a Vio video, „wycisz / przypnij kartę”, „przywróć zamkniętą kartę”, „wstecz”,
  „dalej”, „znajdź w historii X”, „otwórz zakładkę X”, „stop”. The agent gets article reading, tab
  screenshots, find / scroll to, media, tab and history / bookmark tools, and frames the element in
  purple before every click. Older Vio versions keep working as before.
- **Price watches.** „Obserwuj cenę tej strony” — Soul checks the page every 3 hours by itself (no AI, no
  hidden browser tab) and tells you when the price drops; „pokaż obserwowane ceny”, „przestań obserwować
  cenę …”.
- **Tab scenes.** „Zapisz scenę praca” saves the open Vio tabs, „otwórz scenę praca” brings them back,
  „usuń scenę praca” deletes it.
- **REST AI counter.** Settings → O aplikacji shows how many REST AI calls Soul made today, so you can
  see nothing runs AI in the background.

### Fixed

- **Dictation paste:** the pasted text no longer shows up in Win+V / cloud clipboard or the Island's
  clipboard history, waits until you release the talk key's modifiers (no more Ctrl+Alt+V), and your
  previous clipboard is restored later (slow apps like Word got the old text) — unless you copied
  something new meanwhile.
- **Island (Windows):** clicking the Island no longer takes focus from your app, clicks next to it
  go through to the window below, and expanding / collapsing no longer resizes the native window
  (no more stutter or clipped panel). A click outside closes it; it fixes itself after a DPI change.
- Voice fixes found by the new end-to-end voice test suite:
  - „A teraz to samo na YouTube” repeats the previous search on YouTube (it searched for „samo”).
  - „Wyszukaj na YouTube … i włącz drugi wynik” no longer tries to open an app called „drugi wynik”.
  - Switching to a Vio tab by name no longer ends in „Zapętliłem się”: the tab list stays visible
    to the agent after a bad model answer, and a repeated read shows its result again.
  - Questions about the open page („W którym roku według tego artykułu…?”, „Streść mi ten
    artykuł”), long „Wypełnij formularz: …” commands and „Wybierz sos ostry” are commands, not
    dictation.
  - With Polish as the language, a transcript that is clearly another language is neither pasted
    nor executed — Soul asks to repeat.
  - Text actions („popraw to”, „przetłumacz to”) use the agent's model rotation, so one overloaded
    model (503) no longer fails them; a slow model is skipped after 9 s instead of failing the step.
  - Gemini 5xx errors show the short API message instead of a JSON dump.
- Soul Agent / model pool: retired models (404) are skipped for good, also before the ledger is written,
  and models missing from the key's list are never picked; no `/models` request per step. A model
  that fails (429 / 503 / 400 / network) rests for 60 s. A model that rejects the MINIMAL thinking
  level is retried with LOW, then without thinking, and the working setting is remembered. Only one
  model call per step is in flight.
- Soul Agent / Vio: identical repeated calls are not run again (the model gets a hint); „streść tę stronę”
  reads the active tab straight away. `browser_fill` on select / time / date fields no longer ends the task:
  radios and checkboxes are clicked, the rest is reported to the model as a note. Web tasks go to Vio
  even when another browser is in front (unless you point at the screen).
- „Weź mi przecież tego YouTube.” just opens YouTube; filler words and a leading „i” no longer end up in
  search queries.
- Soul Agent no longer fails on free-tier limits: model calls rotate through a pool (3.1 / 3.5 /
  2.5 flash-lite, 3 / 3.7 / 3.6 flash — only those your key lists), each with a client-side
  requests-per-minute limit (`modelRpm` in the agent config, defaults 15/15/10/5/5/5). A 429/503
  puts that model on a 60 s cooldown and the next one is tried at once; when all are busy the
  Island shows „Czekam na model…” (up to 8 s) instead of failing.
- Vio: tabs are now read correctly (Vio's tab ids are text), sleeping tabs are woken before reading,
  and „otwórz …” links open as a new Vio tab (with Vio brought to the front) whenever Vio is
  connected — the system browser is used only without Vio. Web searches and YouTube
  (search / play) open in Vio too, also from scenes.
- Voice commands no longer drift into Slovak, Spanish, Italian or French: the Live session is pinned
  to Polish, and a command that was clearly heard in another language makes Soul ask you to repeat
  instead of acting on it.

## [0.3.0] - 2026-10-16

### Added

- **Soul Agent — voice-driven computer control.** Say „kliknij Zapisz”, „przewiń w dół”, „wpisz w
  wyszukiwarkę FiveM serwery”, „otwórz ustawienia dźwięku i ustaw mikrofon na Voicemeeter” or
  „wyślij Silverowi na Discordzie: będę za 10 minut” and Soul works in the active window step by
  step: it reads the window's accessibility tree (a screenshot only when needed), does one action,
  checks it worked and continues — up to 20 steps / 90 s. The Island shows the current step, a
  progress line and a **Zatrzymaj** button; at the end Soul says what it did. Trivial tasks
  („kliknij Zapisz”, „przewiń w dół”) need no AI call at all.
- Safety first: sending, buying, deleting, discarding unsaved work and changing system settings
  always ask „Na pewno?” first (buttons or a spoken „tak” / „nie”), even if the AI didn't flag it.
  Soul never types passwords or keys, never reads password fields, refuses password managers,
  banking sites, Windows Security, terminals and the Run dialog (editable blocklist), pauses over
  fullscreen games and treats text on screen as data, never as instructions. Stop anytime: Esc
  twice, the talk key, the mouse in a screen corner, or **Zatrzymaj**.
- Settings → Agent: on/off, max steps, highlight ring before clicks, confirmation level (risky
  only / every action), model, blocklist editor and a log of recent steps.

- Follow-ups: Soul remembers the last 3 commands for 90 s („mów ciszej” → „a teraz głośniej”,
  „otwórz Discord” → „i zamknij go”, a question → „a jutro?”). A small dot on the Island shows the
  context is active; „nowy temat” / „zapomnij” clears it. Settings → Głos i dusza → Kontekst rozmowy
  (on/off, timeout).
- Chained commands: „zamknij Spotify i otwórz Discord”, „pauza i ścisz o 20”, „włącz scenę granie,
  a potem ustaw timer na 30 minut” — up to 4 actions in order, stops at the first failure, one
  combined result on the Island („✓ Spotify zamknięty · ✓ Discord otwarty”).
- Selected-text actions: „popraw to”, „przetłumacz to na angielski”, „zrób to bardziej formalne /
  luźniejsze”, „rozwiń to” replace the selection (with „Cofnij” for 10 s); „streść to” and
  „wyjaśnij to” show the result on the Island with „Kopiuj” / „Wklej”. Your clipboard is restored.
- Relative system volume: „ścisz o 20”, „podgłośnij o 10”.
- Answer „Na pewno?” by voice: hold the talk key (or the dictation / command hotkey) and say
  „tak” / „potwierdź” or „nie” / „anuluj”.

### Fixed

- Spoken requests like „wpisz w wyszukiwarkę…”, „zaznacz newsletter w formularzu” or „co jest na
  ekranie?” now always reach Soul Agent. Before, the voice model sometimes answered „Niestety, nie
  widzę Twojego ekranu” instead; it now hands such requests to the agent itself.
- A blank white „Soul” window with a title bar no longer appears and stays on screen while the agent
  works; the highlight ring is a proper click-through overlay again.
- Soul's spoken reply can no longer get stuck repeating its last sound („jasneeee…”): the audio
  output is stopped when a reply ends, and restarted if the sound device stalls.
- Soul's spoken summary after an agent task is no longer cut off before it starts.
- Soul's own windows never count as a fullscreen game for „Pause in games”.
- Closing apps („zamknij Discord”, „zamknij Steam”) works and takes under 4 s: Soul finds the app
  by its program (Discord.exe, steam.exe, Spotify.exe…), never a helper process like
  steamwebhelper, closes it gracefully and then ends the whole process tree. Apps that hide to the
  tray (Discord, Steam, Spotify) are really quit; Steam uses its own exit. A 5 s limit shows a clear
  error on the Island.
- The „Na pewno?” card no longer turns into the music view on hover: it stays on top until you
  answer or it times out. The question follows the UI language („Zamknąć Discord?”).

## [0.2.1] - 2026-10-15

### Fixed

- Windows: no more console window flashing on voice commands („włącz Spotify”, „włącz muzykę”),
  closing apps, media keys, volume and scenes. Apps and links open via ShellExecute instead of
  `cmd /c start`; every helper process runs with `CREATE_NO_WINDOW`.

## [0.2.0] - 2026-10-15

### Added

- Installers: Windows NSIS (`.exe`) + MSI, Linux `.deb`, `.rpm` and `.AppImage`.
- Automatic updates (signed, GitHub Releases): stable / beta channel, progress bar, changelog,
  Island notice „Nowa wersja — Zainstaluj”. On `.deb`/`.rpm` Soul links to the new package instead.
- Settings → O aplikacji / About: version, update check, auto-update toggle.

## [0.1.0] - 2026-10-15

- First version: Dynamic Island, push-to-talk dictation, Circle-to-Ask, voice commands, memory,
  scenes, offline wake word.
