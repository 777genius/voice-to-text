# Changelog

All notable changes to VoicetextAI are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

---

## [0.14.0] - 2026-07-07

### Added
- Added quiet-session detection for recording sessions that receive microphone audio but produce no useful STT transcript.

### Changed
- Made Deepgram streaming more resilient when keep-alive, finalize, and close events overlap during longer recording sessions.
- Removed an unused hard-stop transcription path so recording shutdown follows the same safer drain flow.

### Fixed
- Preserved late finalize transcript text after drain acknowledgement so the last spoken words are less likely to disappear.
- Delivered transcript updates in order and claimed recording session ids more defensively to avoid stale session races.
- Improved macOS auto-paste by preferring clipboard paste with focus and window guards.
- Kept hotkey-stopped recording text visible through the short grace window while final STT events arrive.

## [0.13.0] - 2026-07-05

### Added
- Added an optional double Space recording hotkey that can start or stop recording from the focused app and removes the inserted spaces automatically.
- Added a landing-page release timeline based on recent GitHub releases, with static snapshot fallback when GitHub is slow or rate-limited.
- Added an isolated Tauri dev config with a separate dev bundle identifier, dev deep-link scheme, disabled debug updater, and prod-backend dev script.

### Changed
- Reduced recording CPU load by using a fast speech-oriented integer downsample path for 48 kHz to 16 kHz microphone capture.
- Throttled audio spectrum UI updates and paused visualizer rendering while inactive or hidden.
- Simplified the free pricing card on the landing page and made release-download data reuse a safer shared cache.

### Fixed
- Blocked microphone test startup while a recording session is active, preventing two microphone captures from racing each other.
- Show recording error details from the mini recording window when a recording failure happens.
- Avoided using the production bundle id while running local debug builds, reducing dev/prod focus and updater interference.

## [0.12.1] - 2026-06-12

### Fixed
- Recreate the selected microphone capture between recording starts so reconnected headphones are resolved again instead of reusing a stale audio handle or fallback input.

## [0.12.0] - 2026-06-10

### Added
- Added a hybrid auto-paste backend that uses clipboard paste for longer transcripts and keeps direct typing for short text.

### Changed
- Increased the free transcription plan messaging from 60 to 120 minutes across the landing page locales.
- Restored the user's clipboard after long transcript paste when the clipboard was not changed by the user.

### Fixed
- Fixed long transcript auto-paste reliability by using the platform paste shortcut after safely focusing the original target app.
- Fixed macOS release runtime lookup by adding an executable Frameworks rpath for bundled Swift libraries.
- Avoided reactivating the auto-paste target when it is already the frontmost app.

## [0.11.2] - 2026-06-06

### Changed
- Show the mini recording window immediately in the listening state while the STT stream is still starting.
- Keep mini-window profile, minimize, and settings controls hidden until the cursor is over the mini window.
- Keep long mini-window transcripts pinned to the latest text with a left fade instead of cutting the newest phrase.
- Add extra mini-window animation gutter space so the opening bounce is not clipped.

### Fixed
- Preserve speech captured immediately after the hotkey press while the backend STT stream is still connecting.
- Finalize and drain backend STT results before closing the stream, preventing short early phrases from being dropped.
- Emit audio level and spectrum during STT startup so the mini-window visualizer reacts immediately when speaking.
- Prevent stale stop/idle events from older sessions from hiding a newly reopened mini window.
- Restart VAD capture cleanly between sessions so the audio visualizer and speech detection do not get stuck.
- Use the green mini status indicator for the immediate listening state.

## [0.11.1] - 2026-05-31

### Fixed
- Fixed hold-to-record quick press/release/repress races where an old release could block the next start or hide the reopened mini window.
- Fixed mini recording window open animation so stale slide-out state is reset before the bounce animation starts.

---

## [0.11.0] - 2026-05-31

### Added
- Added cross-platform live translation audio routing adapters for macOS, Windows, and Linux.
- Added Windows VB-CABLE support for translated voice output and WASAPI loopback capture for incoming subtitles.
- Added Linux PulseAudio/PipeWire-Pulse virtual microphone support with `pactl`, `pacat`, and `parec`.
- Added platform setup status in Live translation settings so users know which virtual microphone to select.
- Added hold-to-record mode that records only while the hotkey is held and still drains final speech after release.

### Changed
- Live translation services now depend on audio ports and platform factories instead of concrete macOS audio implementations.
- The mini recording window now opens with a bouncy animation and closes by sliding toward the nearest screen edge.
- Release builds now run frontend and Rust quality gates before creating GitHub release assets.

### Fixed
- Prevent selecting virtual translation microphones as the app's real microphone input.
- Preserve final recording text more reliably when stopping from the hotkey.

---

## [0.10.30] - 2026-05-31

### Fixed
- Fixed the macOS launch hotfix workflow by preparing the Swift Concurrency runtime with a cross-platform Node script, so Linux and Windows release jobs do not fail while macOS still bundles the required runtime.
- Fixed the macOS bundle file mapping so `libswift_Concurrency.dylib` is copied into `Contents/Frameworks`.

---

## [0.10.28] - 2026-05-31

### Fixed
- Fixed macOS launch failure after `0.10.27` by bundling the Swift Concurrency runtime required by ScreenCaptureKit incoming translation.

---

## [0.10.27] - 2026-05-31

### Added
- Added OpenAI live translation mode for calls: microphone speech is translated with OpenAI realtime translation and played as English voice into the BlackHole 2ch virtual microphone.
- Added incoming system-audio subtitles on macOS: app and meeting audio can be transcribed and translated into Russian text in the recording popover.
- Added an OpenAI API key field in Settings for live translation, with `OPENAI_API_KEY` environment fallback.

### Changed
- Live translation keeps translated text in the recording popover and does not run dictation auto-copy, auto-paste, or history side effects.

### Fixed
- Improved translated audio draining and incoming translation queuing so speech is less likely to be clipped or skipped during longer sessions.

---

## [0.10.26] - 2026-05-19

### Changed
- Enable the mini recording window and automatic text paste by default for new app/settings state.
- Keep the mini recording text pinned to the latest visible transcript tail while dictating.

### Fixed
- Stop treating Deepgram `speech_final` as a frontend recording auto-stop signal, so long dictation keeps listening until the user stops it.
- Preserve the selected microphone when it is temporarily unavailable, while still recreating the default capture device after system default-device changes.

---

## [0.10.25] - 2026-05-18

### Added
- Show a concise hotkey prompt in the mini recording window when it is idle and empty.
- Reuse one shared hotkey display formatter across the recording window and settings.

### Fixed
- Prevent auto-paste from launching or reopening target apps while restoring focus, avoiding duplicate Electron instances.
- Refuse auto-paste when the original target app cannot be safely focused, instead of typing into a random active window.
- Avoid committing stale interim Deepgram text to the stable paste baseline when later segment corrections arrive.

---

## [0.10.24] - 2026-05-17

### Fixed
- Restored direct Enigo text input for auto-paste, removing the clipboard-based paste path and preserving the user's clipboard contents.

---

## [0.10.23] - 2026-05-17

### Fixed
- Made non-macOS clipboard paste use Enigo's physical `V` key instead of layout-dependent Unicode `v`, improving `Ctrl+V` reliability on non-English keyboard layouts.

---

## [0.10.22] - 2026-05-17

### Fixed
- Fixed a macOS crash during clipboard auto-paste by replacing Enigo's layout-dependent `Cmd+V` event with a direct CoreGraphics physical `Cmd+V` key event.
- Fixed paste sometimes selecting/focusing text instead of inserting when the active keyboard layout was not English.
- Increased the paste settle delay before restoring clipboard text so target apps have more time to consume the inserted transcript.

---

## [0.10.21] - 2026-05-17

### Fixed
- Switched auto-paste from per-character synthetic typing to clipboard paste, preventing pasted Russian text from triggering the recording hotkey.
- Shortened auto-paste hotkey suppression so intentional hotkey presses are not swallowed for several seconds after live paste.
- Kept hotkey-stopped sessions open briefly for late STT final events so the last spoken phrase is not dropped before paste/copy.

---

## [0.10.20] - 2026-05-17

### Fixed
- Fixed repeated recording toggles from single-key hotkeys such as `Backquote` when macOS delivers release/repeat events while the key is still being held, which looked like constant reconnecting.
- Stopped queueing an automatic restart when a repeated hotkey arrives while recording stop/finalize is still processing.
- Restored live auto-paste on finalized transcript segments so long dictation does not wait for the final endpoint before typing.
- Prevented programmatic mini-window resize/reposition from overwriting the user's saved mini-window position.

---

## [0.10.19] - 2026-05-17

### Fixed
- Fixed live auto-paste firing on Deepgram segment finals while the speaker is still talking, which could refocus the target app mid-recording and race the single-key recording hotkey.
- Added release-level logging for recording hotkey suppression during auto-paste.

---

## [0.10.18] - 2026-05-17

### Fixed
- Fixed recording hotkey self-triggering during the focus switch that happens before auto-paste starts typing.

---

## [0.10.17] - 2026-05-17

### Fixed
- Fixed recording hotkey self-triggering while auto-paste types text that contains the same physical key as the configured hotkey, such as `Backquote` on Russian layouts.

---

## [0.10.16] - 2026-05-17

### Fixed
- Fixed repeated recording toggles from hotkey key-repeat events that could look like constant reconnecting.

---

## [0.10.15] - 2026-05-17

### Fixed
- Fixed premature recording auto-stop when local VAD treated quiet speech as silence while Deepgram was still receiving amplified audio.

---

## [0.10.14] - 2026-05-16

### Changed
- Extended frontend backend keepalive settings so long pauses are less likely to trigger reconnects before the next recording.

---

## [0.10.13] - 2026-05-16

### Fixed
- Improved the mini recording visualizer and the hidden hotkey recording flow.

---

## [0.10.12] - 2026-05-15

### Fixed
- Open update details in a dedicated update window instead of relying only on the settings screen.

---

## [0.10.11] - 2026-05-15

### Fixed
- Cleaned up in-app release notes so installation instructions do not replace the actual update details.

---

## [0.10.10] - 2026-05-15

### Fixed
- Fixed update badge handling and hotkey restart behavior after recording stops.

---

## [0.10.9] - 2026-05-15

### Fixed
- Restored runtime handling for automatic copy and automatic paste actions.

---

## [0.10.8] - 2026-05-15

### Fixed
- Restored the settings toggles for automatic copy and automatic paste.

---

## [0.10.7] - 2026-05-15

### Changed
- Clarified Deepgram multilingual mode behavior in settings.
- Documented the Deepgram transcription workflow.

### Fixed
- Improved transcription recovery after connection issues, stop cleanup errors, and failed starts.
- Preserved repeated words across live transcription segments.
- Handled Deepgram finalize markers, endpoint finals, and speech-final events more reliably.
- Reconciled manual stop status and final audio draining before finalize.
- Respected auth refresh rate-limit backoff during transcription reconnects.

---

## [0.10.6] - 2026-05-12

### Changed
- The mini recording window is now enabled by default for new app configurations.
- Auto-paste now inserts finalized speech segments sooner instead of waiting for the whole utterance to finish.

### Fixed
- Auto-paste now avoids duplicating already inserted finalized text and ignores stale paste completions from older recording sessions.

---

## [0.10.5] - 2026-05-03

### Added
- Added an optional setting to start recording from the global hotkey without showing the recording window.

### Fixed
- Hidden hotkey recording now restores the recording window if starting the recording fails, so permission and device errors are still visible.

---

## [0.10.4] - 2026-05-03

### Added
- Added a setting to enable the completion sound after transcription finishes.

### Changed
- The completion sound is now disabled by default.

---

## [0.10.3] - 2026-05-03

### Fixed
- macOS app activation for auto-paste now uses the system `open -b` command and reports clearer activation errors.

---

## [0.10.2] - 2026-04-27

### Added
- Account profile now shows the user bonus balance directly in the UI.

### Fixed
- Free plan messaging on the landing page was corrected and consistently highlighted in header and hero sections.
- Startup STT config sync now avoids race and stale-device issues by guarding sync flow and serializing writes.
- Keep-alive TTL handling in Tauri was aligned with backend behavior.

---

## [0.10.1] — 2026-03-11

### Fixed
- Increased the auth window height on Windows so the bottom part of the login screen, including the registration action, fits without clipping.

---

## [0.10.0] — 2026-03-11

### Added
- Registration flow now returns the next required step (`verify_email` or `password_setup`) so the app can guide users more clearly.

### Changed
- Improved auth provider error handling during registration, password reset, and email verification flows.
- Updated auth copy for clearer verification code and account recovery messaging.

---

## [0.9.11] — 2026-03-11

### Changed
- Updated landing page testimonials and added the feedback email contact.

---

## [0.9.10] — 2026-02-26

### Added
- Settings navigation: scroll to specific settings sections when opening the settings window, with visual highlight feedback
- Error messages for unavailable audio devices in multiple languages

### Changed
- Enhanced settings store to manage pending scroll actions
- Improved keyterms normalization and persistence in streaming STT settings
- Added tests for streaming keyterms persistence

---

## [0.9.9] — 2026-02-25

### Added
- Microphone permission checks on macOS — the app now validates microphone access before starting audio capture
- Audio silence detection: monitors for consecutive zero audio samples and notifies users of potential microphone access issues
- Clear error messages when microphone access is denied by the system

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
- Streaming keyterms support: custom keywords improve STT recognition accuracy
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

[0.10.1]: https://github.com/777genius/voice-to-text/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/777genius/voice-to-text/compare/v0.9.11...v0.10.0
[0.9.11]: https://github.com/777genius/voice-to-text/compare/v0.9.10...v0.9.11
[0.9.10]: https://github.com/777genius/voice-to-text/compare/v0.9.9...v0.9.10
[0.9.9]: https://github.com/777genius/voice-to-text/compare/v0.9.8...v0.9.9
[0.9.8]: https://github.com/777genius/voice-to-text/compare/v0.9.7...v0.9.8
[0.9.7]: https://github.com/777genius/voice-to-text/compare/v0.9.6...v0.9.7
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
