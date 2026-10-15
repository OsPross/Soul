# Changelog

All notable changes to Soul are listed here. `npm run release -- <version>` moves the
**Unreleased** section under the new version; that text becomes the GitHub Release notes and the
changelog shown in Settings → O aplikacji / About.

## [Unreleased]

### Security

- The Gemini API key is stored in the OS credential store (Windows Credential Manager, Linux Secret
  Service) instead of `soul-settings.json`; an existing key is moved there automatically on first start.
- Log/error redaction now also covers the newer `AQ.…` key format and every occurrence of a key.
- CI fails if an API key appears in the source, the built frontend, the binary or the installers.

## [0.2.0] - 2026-10-15

### Added

- Installers: Windows NSIS (`.exe`) + MSI, Linux `.deb`, `.rpm` and `.AppImage`.
- Automatic updates (signed, GitHub Releases): stable / beta channel, progress bar, changelog,
  Island notice „Nowa wersja — Zainstaluj”. On `.deb`/`.rpm` Soul links to the new package instead.
- Settings → O aplikacji / About: version, update check, auto-update toggle.

## [0.1.0] - 2026-10-15

- First version: Dynamic Island, push-to-talk dictation, Circle-to-Ask, voice commands, memory,
  scenes, offline wake word.
