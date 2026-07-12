# E2E (Tauri) tests

Эти тесты запускают **реальное Tauri приложение** и управляют окнами через WebDriver.

## Важно про macOS

По текущей документации Tauri v2 WebDriver **не поддерживается на macOS** (нет WKWebView driver).
Поэтому локально на macOS эти тесты не запускаются — их нужно гонять в CI на Linux/Windows.

## Как запустить (Linux)

1) Установить системный драйвер WebKit:

Debian/Ubuntu:

```bash
sudo apt-get update
sudo apt-get install -y webkit2gtk-driver
```

2) Установить tauri-driver:

```bash
cargo install tauri-driver --locked
```

3) Запустить тесты:

```bash
cd frontend
pnpm e2e:tauri
```

## Live audio smoke tests (macOS)

These tests use real local audio devices and OpenAI APIs. They are ignored by
default and must be run manually.

They cover:

- BlackHole output to BlackHole input loopback.
- ScreenCaptureKit 24 kHz mono capture, callback stop, and same-process playback exclusion.
- Outgoing live translation service: synthetic voice -> OpenAI realtime -> virtual microphone route.
- Incoming subtitles service: system output audio -> ScreenCaptureKit loopback -> OpenAI speech-to-text -> OpenAI text translation.
- Incoming spoken service: half-volume system speech -> OpenAI realtime -> Russian text and local translated playback.

Prerequisites:

- macOS with BlackHole 2ch installed.
- BlackHole 2ch available as an input and output device.
- Screen & System Audio Recording permission granted for the test binary or terminal.
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

This does not launch Zoom. It proves the same local virtual audio route that
Zoom/Meet use when BlackHole 2ch is selected as the microphone.

## Live audio soak tests (macOS)

The soak runner keeps the real audio services alive long enough to catch queue growth, stuck
stop/start cleanup, delayed OpenAI output, and system audio permission issues that a short smoke
test can miss. It also runs the constant-memory incoming spoken WebSocket runtime soak for the same
duration.

Default duration is 10 minutes per long test:

```bash
cd frontend
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY=... npm run e2e:live-audio-soak
```

For development, run a shorter pass:

```bash
cd frontend
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY=... \
  LIVE_AUDIO_SOAK_SECONDS=60 npm run e2e:live-audio-soak
```

For release confidence, use 10-30 minutes:

```bash
cd frontend
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY=... \
  LIVE_AUDIO_SOAK_SECONDS=1800 npm run e2e:live-audio-soak
```
