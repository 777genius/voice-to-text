# E2E (Tauri) tests

Эти тесты запускают **реальное Tauri приложение** и управляют окнами через WebDriver.

## Важно про macOS

По текущей документации Tauri v2 WebDriver **не поддерживается на macOS** (нет WKWebView driver).
Поэтому WebDriver suite запускается на Linux/Windows. Для macOS ниже есть отдельный native self-runner без WebDriver.

## Как запустить (Linux)

1) Установить системный драйвер WebKit:

Debian/Ubuntu:

```bash
sudo apt-get update
sudo apt-get install -y webkit2gtk-driver
```

2) Установить tauri-driver:

```bash
cargo install tauri-driver --version 2.0.6 --locked
```

3) Запустить тесты:

```bash
cd frontend
pnpm e2e:tauri
```

## Native macOS recording-window regression

```bash
npm run e2e:native-window
```

The launcher creates a **new disposable source snapshot** under the system temporary directory,
reuses only installed dependencies, builds bundled assets and the debug-only `native-window-e2e`
Cargo feature, then launches its own executable. It never launches the installed application or
uses a real project as the native runtime workspace. Every WKWebView uses a nonpersistent data
store; the test has a unique application identifier, HOME and config directory. The inherited
environment is allowlisted: API credentials, tokens, `.env` files and runtime overrides are not
forwarded. No microphone, clipboard, paste, physical global hotkey registration or paid API is used.
The fixture pins `TAURI_DEBUG=true` so both build-time and runtime API validation retain its
dummy loopback endpoint. Production builds keep their normal HTTPS-only policy.

The actual NSPanel, WKWebView, Vue components, Pinia state, Tauri IPC/events, hotkey acceptance and
`TranscriptionService` execute normally. Only audio capture and STT provider adapters are deterministic.
A transcript appears only after the service sends captured test audio to the provider. This does
not prove physical key delivery, microphone permission, provider/network behavior, hardware sleep. It does include **180 seconds of real elapsed hidden idle** while WKWebView
JavaScript may naturally suspend. A native monotonic timer checks idle/resource invariants, writes
15-second heartbeats and sends one production hotkey press to wake the real panel. The resumed Vue
UI must show that new recording and fresh audio-derived text; no second JS press or fake clock is used.

Scenarios cover 22 complete record/transcript/stop/hide/reopen cycles, cold and keepalive sessions,
current/stale real native closes, a replacement press during the 220 ms close interval, stale status
and window events, session-scoped stop ownership, duplicate key callbacks, stop during slow Starting,
queued hold cancellation during Processing, failed start/retry, UI retry cancellation by a newer
native hotkey or manual stop, and mini/full layouts. It also proves automatic retry after a real
scoped Starting failure clears native ownership, provisional hold cancellation before session
allocation leaves the visible UI Idle, and duplicate direct starts preserve working UI minimize.
Mutation observers retain brief closing-class
transitions, while IPC samples check native visibility throughout protected intervals. Listening,
Recording, real UI-stop Processing, unique transcripts and balanced capture are positive controls.

Artifacts remain in the printed temporary directory: build logs, source/binary checksum manifest,
`native-progress.jsonl` (including native window number for screenshots), runtime log and final JSON.
App execution is bounded to 8 minutes (build has a separate deadline), with missing-progress deadlines.
Only the process started by this launcher is terminated; unsupported platforms and skips fail.
A failed app exit still prints its structured scenario failure. There is no arbitrary binary or
config-directory override. To repeat an already built, checksummed fixture only:

```bash
node e2e-tests/run-native-window-e2e.mjs --no-build /absolute/system/tmp/voicetext-native-e2e-ABC123
```

The existing disposable directory must be canonical and owned by the current user; the executable
must match its manifest and contain the native feature marker. A repeat gets a fresh result filename.
The unique application identifier must also be embedded in the binary; a concurrent unrelated
build in the shared Cargo cache fails closed instead of being attributed to this source snapshot.
`--no-build` repeats the recorded snapshot, not later workspace edits. Do not claim a native pass
from helper unit tests or a macOS WebDriver skip.

## Recording window lifecycle regression

`specs/recordingWindowLifecycle.e2e.mjs` uses the real native window, Vue component,
Tauri IPC and event bridge in the `webdriver-e2e` build. Run it only with disposable
test application data, never against a normal user profile.

It checks that a current epoch really hides the native window, an older epoch
cannot hide it after reopen, and a delayed old `recording:window-will-hide-for-hotkey-stop`
event cannot close the mini UI or suppress its transcript. A current event is a
positive control for the real Vue listener. Five hide/show cycles exercise pending
close animations; the final reopened window is observed for 800 ms, beyond the
220 ms native delay and 260 ms animation reset. Assertions cover the entire interval,
including short closing-class transitions, rather than only eventual visibility.

The transcript is an idle UI fixture, not microphone output. This test does not
exercise real recording start/stop, physical hotkeys, STT providers, audio devices,
long OS sleep, or macOS WebView suspension. It restores the changed configuration
and the harness's full recording layout in `finally`. A macOS runner skip is not
evidence that this native regression passed; run the supported Linux/Windows gate.

## Live audio smoke tests (macOS)

These tests use real local audio devices and OpenAI APIs. They are ignored by
default and must be run manually.

They cover:

- BlackHole output to BlackHole input loopback.
- A nine-second incoming spoken playback burst fits the device-rate-independent bounded buffer without dropping audio.
- ScreenCaptureKit 24 kHz mono capture, callback stop, and same-process playback exclusion.
- Outgoing live translation service: synthetic voice -> OpenAI realtime -> virtual microphone route.
- Incoming subtitles service: system output audio -> ScreenCaptureKit loopback -> OpenAI speech-to-text -> OpenAI text translation.
- Incoming spoken service: full linguistic/volume matrix -> OpenAI realtime -> Russian text and local translated playback, with independent transcription of translated PCM to verify meaning.
- Mid-phrase stop preserves the accepted translated text/audio tail and emits no callbacks after terminal stop.
- A controlled WebSocket relay interrupts a real paid translation session after the first PCM append and requires capture/output cleanup.
- Full duplex: incoming and outgoing paid routes run together, then each direction produces fresh evidence after the other is stopped.

Prerequisites:

- macOS with BlackHole 2ch installed.
- BlackHole 2ch available as an input and output device.
- Screen & System Audio Recording permission granted for the test binary or terminal.
- The macOS GUI session is unlocked for the entire ScreenCaptureKit run; a locked/headless session exposes no shareable display.
- A dedicated, revocable `OPENAI_E2E_API_KEY` in the environment.
- Explicit paid-test acknowledgement with `VOICETEXT_RUN_PAID_E2E=1`.

Run:

```bash
cd frontend
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY=... npm run e2e:live-audio
```

The runner intentionally ignores `OPENAI_API_KEY` and `.env` so a normal developer credential
cannot trigger paid audio tests accidentally. `pnpm e2e:live-audio` also works when the local pnpm
version is compatible with the lockfile.

The paid network interruption case cuts only its local relay after OpenAI confirms the session. It
does not disable the machine network or interfere with unrelated applications.

This does not launch Zoom. It proves the same local virtual audio route that Zoom/Meet use when
BlackHole 2ch is selected as the microphone. The full-duplex gate uses real ScreenCaptureKit,
system-default playback, OpenAI sessions in both directions, and independent BlackHole
transcription. Acoustic speaker leakage remains a manual check.

## Live audio soak tests (macOS)

The soak runner keeps the real audio services alive long enough to catch queue growth, stuck
stop/start cleanup, delayed OpenAI output, and system audio permission issues that a short smoke
test can miss. It also runs the constant-memory incoming spoken WebSocket runtime soak for the same
duration.

Default duration is 30 minutes per long test:

```bash
cd frontend
brew install switchaudio-osx
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY=... npm run e2e:live-audio-soak
```

For development, run a shorter pass:

```bash
cd frontend
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY=... \
  LIVE_AUDIO_SOAK_SECONDS=60 LIVE_AUDIO_ALLOW_SHORT_SOAK=1 npm run e2e:live-audio-soak
```

The release soak requires at least 30 minutes:

```bash
cd frontend
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY=... \
  LIVE_AUDIO_SOAK_SECONDS=1800 npm run e2e:live-audio-soak
```

The deterministic spoken runtime soak samples process RSS after warmup, enforces at most 16 MiB
growth, checks that the translated-event backlog stays bounded throughout the run, and requires it
to drain near real time before shutdown. A separate native spoken soak keeps the real
ScreenCaptureKit -> OpenAI Realtime -> SystemDefault CPAL chain active, measures RSS and playback
pending high-water, rejects dropped audio, and requires fresh text/audio near the end of the run.
The same runner also performs 25 complete spoken start/stop cycles and requires balanced capture,
output, WebSocket, translation-session, and task counters with bounded post-warmup RSS.

GitHub releases require a successful manual `macOS Audio Release Gate` run on the self-hosted
`voicetext-audio` Mac. The `Release` workflow accepts that run ID only when its evidence artifact
matches the exact tagged commit and records a soak of at least 1,800 seconds. The durable evidence
bundle includes the paid matrix WAV/transcript/metrics files and a SHA-256 manifest rechecked by
the release workflow.

Before starting that gate, the operator must complete and attest all three hardware checks on the
same commit:

1. Join a real Zoom call with a second participant, set Zoom Speaker Volume to 50%, and verify both
   incoming English -> Russian text/audio and outgoing Russian -> English virtual microphone audio.
2. During incoming spoken translation, disconnect the active USB/Bluetooth output, verify terminal
   cleanup, select a valid output, restart, and verify a fresh translation.
3. During incoming spoken translation, sleep and wake the Mac, verify that no stale session keeps
   producing events, restart, and verify a fresh translation.

Use a headset for the Zoom check so acoustic speaker leakage is not confused with a routing fault.
The workflow stores the GitHub actor, run ID, and all three attestations in the checksummed release
evidence. Normal pushes and pull requests use the shared keyless `Quality Gates` workflow and never
require an audio device or OpenAI credential.
