# Changelog

All notable changes to Soul are listed here. `npm run release -- <version>` moves the
**Unreleased** section under the new version; that text becomes the GitHub Release notes and the
changelog shown in Settings → O aplikacji / About.

## [Unreleased]

### Added

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
