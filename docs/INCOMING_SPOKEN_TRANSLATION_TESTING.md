# Incoming Spoken Translation Testing

This runbook covers the macOS `Text and audio` incoming translation path. It deliberately keeps
local deterministic tests separate from permission-gated native audio and paid OpenAI tests.

## Automated Gate

Run from `frontend`:

```bash
npm run typecheck
npm run test:run
npm run build
cd src-tauri
cargo fmt --all -- --check
cargo test
cargo clippy --lib --tests
```

The default Rust suite uses no real OpenAI credential. The local WebSocket E2E verifies:

- placeholder Authorization and target-language `session.update`;
- 24 kHz mono PCM append messages;
- source and translated transcript deltas;
- translated PCM playback and final tail drain;
- unknown event tolerance and graceful `session.close`;
- delayed or missing Ready, malformed JSON/base64, HTTP 401/429, abrupt/stalled close, and oversized messages;
- active-service drop cleanup for capture, translator, and output owners.

Run only that suite with:

```bash
cd src-tauri
cargo test --test realtime_translation_websocket_e2e_test
```

## Synthetic Soak

The ignored soak defaults to 30 minutes. It continuously sends translated text/audio through the
real WebSocket adapter and application runtime, checks that the service stays Recording, and proves
that output accumulation remains bounded by the generated event count.

Short verification:

```bash
cd src-tauri
SPOKEN_TRANSLATION_SOAK_SECONDS=10 cargo test \
  --test realtime_translation_websocket_e2e_test \
  spoken_runtime_long_soak_keeps_audio_flow_bounded_and_stops_cleanly \
  -- --ignored --nocapture
```

Release run:

```bash
cd src-tauri
SPOKEN_TRANSLATION_SOAK_SECONDS=1800 cargo test \
  --test realtime_translation_websocket_e2e_test \
  spoken_runtime_long_soak_keeps_audio_flow_bounded_and_stops_cleanly \
  -- --ignored --nocapture
```

Expected result: nonzero audio events, exactly three output samples per generated event, no runtime
error, and clean capture/output shutdown.

Runtime logs expose only structured diagnostics: session and route identifiers, duration, audio
milliseconds, first input/text/audio latency, queue and pending high-water marks, drop/underrun/
overflow counters, stop reason, and cleanup time. They never include credentials, transcripts,
base64, or PCM samples.

## macOS Native Audio Gate

Before running, grant the test host Screen and System Audio Recording permission and ensure a system
default output device is available.

Capture format and callback stop:

```bash
cd src-tauri
cargo test --test incoming_system_audio_translation_e2e_test \
  isolated_realtime_capture_emits_24khz_mono_and_stops_callbacks \
  -- --ignored --nocapture
```

Local playback and self-exclusion:

```bash
cd src-tauri
cargo test --test incoming_system_audio_translation_e2e_test \
  system_default_playback_is_drained_and_excluded_from_system_capture \
  -- --ignored --nocapture
```

The second test plays an external 440 Hz fixture and a same-process 880 Hz translated-output
fixture. ScreenCaptureKit must contain the external tone while self-tone power stays below 5% of
the external tone power.

## Paid OpenAI Gate

Use a new, dedicated, revocable test key. The spoken paid test intentionally ignores `.env` and
`OPENAI_API_KEY`; never add the key to source, logs, screenshots, or test artifacts.

```bash
cd src-tauri
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY="sk-..." \
  cargo test --test incoming_system_audio_translation_e2e_test \
  incoming_spoken_translation_returns_realtime_text_and_audio_from_system_capture \
  -- --ignored --nocapture
```

This test performs the paid chain:

```text
generated macOS speech
  -> ScreenCaptureKit 24 kHz mono capture
  -> OpenAI realtime translation
  -> translated text callback + translated PCM collector
  -> graceful final tail and shutdown
```

By default it runs the linguistic matrix: English to Russian, names/numbers, technical terms,
mixed English/Russian, already-Russian input, long context, pauses/silence, and overlapping system
speakers. Set `INCOMING_SPOKEN_E2E_SCENARIO=technical_terms` to run one case. Reviewable source
audio, translated PCM, both transcripts, first-input/text/audio timings, errors, and the human
reference are written under `src-tauri/target/e2e-artifacts`; override the directory with
`INCOMING_SPOKEN_E2E_ARTIFACTS`.

Run `incoming_spoken_translation_paid_stop_mid_phrase_is_bounded` with the same paid-key guard to
verify bounded shutdown against a real OpenAI session. Deterministic network interruption,
malformed frames, abrupt close, stalled close, 401/429, and oversized messages are covered by
`realtime_translation_websocket_e2e_test` without a paid key.

It must report nonempty source text, Russian translated text, nonempty translated PCM, no terminal
errors, and Idle after stop. The separate native output test proves that the same PCM output adapter
reaches the system default device without feeding back into ScreenCaptureKit.

## Manual Fault Checks

These hardware/OS transitions cannot be made deterministic in CI and remain a macOS release check:

1. Start spoken incoming translation with USB or Bluetooth output, disconnect it, and confirm the UI reports a terminal playback error while retaining translated text.
2. Change the system default output during a session, restart translation, and confirm playback uses the new default.
3. Revoke Screen and System Audio Recording permission and confirm capability blocks paid connect before OpenAI is contacted.
4. Sleep and wake macOS during a session; confirm an invalid stream terminates instead of silently showing Recording.
5. Run incoming and outgoing translation together with headphones; stopping either direction must not stop the other.
6. Repeat duplex with speakers only to document acoustic leakage. Speaker AEC is not a supported claim.

## Release Gate

Do not ship macOS spoken incoming translation unless all items are recorded for the release build:

- default Rust, frontend, typecheck, and build suites pass;
- local WebSocket happy path and fault matrix pass;
- 30-minute synthetic soak passes;
- native 24 kHz capture/callback stop passes;
- native 440/880 Hz self-exclusion passes;
- paid OpenAI text and PCM test passes with a dedicated key;
- output disconnect and sleep/wake manual checks produce terminal cleanup;
- captions-only remains the persisted default and works as the rollback path.
