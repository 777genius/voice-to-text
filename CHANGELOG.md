# Changelog

All notable changes to VoicetextAI are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

---

## [0.9.8] — 2026-02-21

### Changed
- Default microphone sensitivity set to 100 with enhanced audio processing
- Updated developer name and contact email across landing pages

---

## [0.9.7] — 2026-02-21

### Added
- Live audio device selection with real-time configuration updates in settings
- Error message display in profile when data fails to load

---

## [0.9.6] — 2026-02-19

### Added
- Deepgram keyterms support: custom keywords improve STT recognition accuracy
- Timeout handling for STT and app configuration updates
- macOS code signing and notarization in CI/CD pipeline
- Updated Tauri updater signing keypair

### Fixed
- macOS microphone access: added NSMicrophoneUsageDescription to Info.plist for signed/notarized builds

---

## [0.9.5] — 2026-02-17

### Added
- Audio device unavailable error messages in multiple languages
- Testimonials section on landing page
- Parallax effect and show more/less toggle for testimonials
- AudioVisualizer reuse in mic test section

### Changed
- Landing page: unified page background, wave dividers between sections
- Switched from @mdi/font webfont to @mdi/js SVG icons for better performance
- Lazy-load below-fold sections, replaced v-dialog with CSS
- Rewritten open source and providers section copy
- Switched state-sync to npm, ignore landing in vite watch

### Fixed
- CLS issues: SSR languages, removed v-app-bar, fixed hero demo width
- Hydration mismatch, added font-display swap, improved link text
- Smooth section transitions: removed background seams
- Hero demo flash during lazy component loading

---

## [0.9.4] — 2026-02-13

### Changed
- Updater release notes loader now starts from `CHANGELOG.md` in the release repository (fewer failed fetch attempts)
- Updated release process documentation

---

## [0.9.3] — 2026-02-11

### Added
- Hotkey management: normalized hotkey strings for cross-platform compatibility
- Session keep-alive: backend provider TTL increased to 5 minutes minimum for hotkey sessions
- Auto-session handling: incoming transcription events now ensure an active session exists

### Changed
- Improved logging for hotkey registration and session lifecycle
- HotkeySection component refactored for cleaner hotkey display and editing

### Fixed
- Aligned Tauri NPM packages (`@tauri-apps/api`, `@tauri-apps/plugin-updater`) with crate versions to fix version mismatch build warning

---

## [0.9.2] — 2026-02-11

### Changed
- Rate-limit backoff logic refactored: `LIMIT_EXCEEDED` exits immediately, `TOO_MANY_SESSIONS` and `RATE_LIMIT_EXCEEDED` use appropriate backoff with jitter

### Fixed
- WS error header parsing: parse `x-voicetext-error-code` header from handshake responses for reliable error classification when response body is unavailable
- Immediate limit detection: detect `LIMIT_EXCEEDED` server code during connect retries and show limit error immediately instead of retrying
- Auth recovery: clear stale recording errors (401/429) after successful token refresh or user change so the UI doesn't show outdated error messages
- Windows build: added missing `ConfigStore` import in updater module (`#[cfg(target_os = "windows")]`)

---

## [0.9.1] — 2026-02-11

### Changed
- Vite dev server port now configurable via `VITE_PORT` environment variable
- Refactored test error callbacks to use standardized logging helper
- Cleaned up unused imports in updater and backend modules

### Fixed
- Improved WebSocket error classification: server 429 responses now parsed for specific error codes (`LIMIT_EXCEEDED`, `TOO_MANY_SESSIONS`, `RATE_LIMIT_EXCEEDED`) instead of generic connection errors
- Fixed shared state for limit detection: `last_remaining_secs` promoted to `Arc<AtomicU32>` so `send_audio()` correctly returns `LimitExceeded` instead of `Closed` when connection drops due to usage limit
- Prevented duplicate API calls on limit exceeded: early return guard stops redundant `/api/v1/account/licenses` fetches when multiple limit events fire
- Fixed UI state flashing during 401 token refresh: `reconcileBackendStatus` no longer overwrites `Starting` with `Idle` during the race between window_shown event and start_recording
- Added error downgrade protection: once limit exceeded is set, subsequent non-critical errors cannot overwrite it

---

## [0.9.0] — 2026-02-10

### Added
- Profile window for user profile management (account info, license, gift codes)
- Demo mode for state synchronization between multiple windows

### Changed
- Settings window height reduced for better screen fit
- Settings window header now supports drag-to-move
- Added CodeRabbit configuration for automated code reviews
- Profile feature decomposed into clean architecture: domain types, composable, section components

### Fixed
- Smart HTTP 429 (rate limit) handling with automatic retry and proper error propagation
- Landing page visualizer bars: soft clamp with reduced amplitude for smoother animation

---

## [0.8.1] — 2026-02-07

### Added
- GA4 analytics events for landing page (nav clicks, downloads, theme/language switches, section views, FAQ expand)
- Always-visible scrollbar in Profile and Settings windows
- Border and box-shadow on recording popover for visual depth

### Changed
- Landing visualizer: stronger center-weighted bar distribution, dimmer bars for better text readability
- Profile dialog now fills available window height

### Fixed
- GA4 not detected on voicetext.site (missing env var in Render config)
- Dark scrim overlay removed from profile dialog
- Reduced padding gap between profile dialog header and content

---

## [0.8.0] — 2026-02-06

### Added
- Checkout success and payment pages with localization (6 languages)
- License activation — claim a license key from email to activate a plan
- Gift code redemption — redeem gift codes for bonus minutes
- Usage progress bar in profile popover
- Landing favicon and app logo in header
- Latest release version and date displayed in hero, download, and open source sections
- Sound preloading for reliable playback on recording window show

### Changed
- Theme selector redesigned: replaced switch + checkbox with a segmented control (Light / Dark / Auto)
- Landing features section: 4 columns per row instead of 3
- Landing download section: detected platform card centered and visually highlighted (scale)
- Updated language count from 50+ to 40+ across all locales
- Regenerated all app icons from new logo for macOS, Windows, Linux, iOS, Android
- Theme toggle button: proper icon button with tooltip and accessibility
- Improved sound playback reliability: AudioContext recreation on close, separate inflight/decoded caches, disconnect on ended
- Updated landing screenshots
- Updated README with current features and architecture

### Fixed
- Theme sync from settings: store watches now propagate to App.vue refs
- Sound decoding errors no longer permanently break playback (rejected promise cache issue)

---

## [0.7.2] — 2026-02-03

### Added
- Paddle integration for subscription payments
- Enhanced settings management and update handling

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

[0.9.6]: https://github.com/777genius/voice-to-text/compare/v0.9.5...v0.9.6
[0.9.5]: https://github.com/777genius/voice-to-text/compare/v0.9.4...v0.9.5
[0.9.4]: https://github.com/777genius/voice-to-text/compare/v0.9.3...v0.9.4
[0.9.3]: https://github.com/777genius/voice-to-text/compare/v0.9.2...v0.9.3
[0.9.2]: https://github.com/777genius/voice-to-text/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/777genius/voice-to-text/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/777genius/voice-to-text/compare/v0.8.1...v0.9.0
[0.8.1]: https://github.com/777genius/voice-to-text/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/777genius/voice-to-text/compare/v0.7.2...v0.8.0
[0.7.2]: https://github.com/777genius/voice-to-text/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/777genius/voice-to-text/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/777genius/voice-to-text/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/777genius/voice-to-text/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/777genius/voice-to-text/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/777genius/voice-to-text/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/777genius/voice-to-text/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/777genius/voice-to-text/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/777genius/voice-to-text/releases/tag/v0.3.0
