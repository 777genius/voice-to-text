# Changelog

All notable changes to VoicetextAI are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

---

## [0.7.1] — 2026-02-03

### Improved
- Enhanced error handling in transcription service
- Refactored transcription service architecture

### Added
- Changelog utilities

---

## [0.7.0] — 2026-02-02

### Added
- Support for 45 speech recognition languages (Deepgram Nova-3) instead of 6
- Separation of recognition language (STT) and interface language (UI) — when selecting a language without translation, UI falls back to the nearest available locale
- Multilingual mode with real-time auto-detection of 10 languages
- Hint when selecting multilingual mode listing supported languages
- System theme support in settings

### Changed
- `FlagIcon` component extended to work with any language code (not just UI locales)
- Language selection in settings now shows full list of STT languages with flags
- Improved settings panel and window close handling
- Updated microphone sensitivity handling
- Redesigned landing page components and localization

---

## [0.6.0] — 2026-02-02

### Added
- Enhanced transcription session management with real-time UI synchronization
- `FlagIcon` component — SVG flags for displaying supported languages
- Locales file `i18n.locales.ts` for centralized language management
- Render deployment configuration (`render.yaml`)
- Release process documentation (`docs/RELEASE.md`)

### Changed
- Project rebranding: renamed to VoicetextAI throughout the project
- Redesigned landing page: new design for pricing, FAQ, footer sections
- Updated `SupportedLanguages` component — switched to SVG flags
- Improved `HotkeySection` in settings
- Refactored `RecordingPopover` — improved state synchronization
- Refactored `transcription store` — extended session management
- Updated backend STT service: improved session and event handling
- Updated dependencies

### Fixed
- Correct display of language flags in language selector
- Transcription state synchronization between windows

## [0.5.1] — 2026-02-01

### Fixed
- Added production env variables to release workflow
- Changed production API domain to `api.voicetext.site`

## [0.5.0] — 2026-02-01

### Added
- Full-featured settings screen with audio device selection
- OAuth2 authentication (Google)
- State-Sync protocol for state synchronization between windows
- Landing page with support for 6 languages (EN, RU, ES, FR, DE, UK)
- Privacy Policy and Terms of Service pages
- E2E tests (WebDriverIO)
- Apache 2.0 license

### Changed
- Updated app icons for all platforms
- Updated dependencies

### Fixed
- Windows compatibility
- Race condition in authentication token handling
- `RunEvent::Reopen` compilation on Linux/Windows
- `.gitignore` patterns were blocking source files

## [0.4.1] — 2025-12-19

### Fixed
- False positives in keep-alive and connection quality indicator

## [0.4.0] — 2025-11-23

### Added
- Security updates

## [0.3.0] — 2025-10-25

### Added
- First public release with basic functionality
- Transcription via Deepgram (Nova-2/3)
- Global hotkeys
- Auto-copy to clipboard
- System tray
- Support for macOS, Windows, Linux

---

[0.7.1]: https://github.com/777genius/voice-to-text/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/777genius/voice-to-text/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/777genius/voice-to-text/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/777genius/voice-to-text/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/777genius/voice-to-text/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/777genius/voice-to-text/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/777genius/voice-to-text/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/777genius/voice-to-text/releases/tag/v0.3.0
