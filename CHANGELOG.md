# Changelog

All notable changes to Soul are listed here. `npm run release -- <version>` moves the
**Unreleased** section under the new version; that text becomes the GitHub Release notes and the
changelog shown in Settings → O aplikacji / About.

## [Unreleased]

### Added

- **Soul Agent + Vio browser.** When the Vio browser's automation API is on, browser tasks go
  through it instead of the screen: faster and exact, no mouse. „otwórz w przeglądarce
  youtube.com”, „przeczytaj / streść tę stronę”, „wypełnij formularz”, „sprawdź które pola są
  puste” work while Vio is connected. Vio may ask for consent on the first connection — the Island
  shows „Zezwól w Vio”. Sending, buying, deleting and **logging in** still need a confirmation;
  private / Tor / internal tabs and password fields are reported, never worked around. Settings →
  Agent shows „Przeglądarka Vio: połączona / niedostępna”.

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
