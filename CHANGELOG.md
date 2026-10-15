# Changelog

All notable changes to Soul are listed here. `npm run release -- <version>` moves the
**Unreleased** section under the new version; that text becomes the GitHub Release notes and the
changelog shown in Settings → O aplikacji / About.

## [Unreleased]

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
