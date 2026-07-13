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
cargo fmt --all --check
cargo test
cargo clippy --lib -- -D clippy::await_holding_lock
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
that event handling remains bounded by the generated event count. Its output and transcript probes
use constant-memory counters rather than retaining the full long-session PCM or text.

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

## Paid Incoming OpenAI Gate

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
  -> translated text callback + translated PCM tee
  -> real system default playback + artifact collector
  -> graceful final tail and shutdown
```

By default it runs the linguistic matrix: English to Russian, names/numbers, technical terms,
mixed English/Russian, already-Russian input, long context, pauses/silence, overlapping system
speakers, and a half-volume source preserving a task count and meeting time. Set
`INCOMING_SPOKEN_E2E_SCENARIO=technical_terms` to run one case. Reviewable source
audio, actual post-capture PCM with an independent transcription, translated PCM, translated
transcript, optional provider source transcript, first-input/text/audio timings, errors, and the
human reference are written under `src-tauri/target/e2e-artifacts`; override the directory with
`INCOMING_SPOKEN_E2E_ARTIFACTS`.
OpenAI documents `session.input_transcript.delta` as optional, so its availability is recorded in
metrics but is not a release assertion.
The already-target-language case accepts either translated output or silence because Realtime
Translation may intentionally suppress speech that is already in the selected output language.
Incoming system audio disables provider noise reduction because it is already a clean digital
stream. Outgoing physical-microphone translation keeps the `near_field` policy. The local
WebSocket E2E asserts this route-specific `session.update` contract.

Run `incoming_spoken_translation_paid_stop_mid_phrase_is_bounded` with the same paid-key guard to
stop while the source fixture is still speaking. It verifies captured source PCM, Russian text,
audible translated PCM, the derived 22-second outer stop bound, and zero text/audio/status/error
callbacks after terminal stop against a real OpenAI session. Deterministic network interruption,
malformed frames, abrupt close, stalled close, 401/429, and oversized messages are covered by
`realtime_translation_websocket_e2e_test` without a paid key.

It must report Russian translated text, nonempty translated PCM, no terminal errors, and Idle after
stop. The separate native output test proves that the same PCM output adapter reaches the system
default device without feeding back into ScreenCaptureKit.

## Paid Outgoing Virtual Microphone Gate

BlackHole 2ch must be installed. The test synthesizes Russian speech, translates it to English,
writes translated PCM through the production virtual-microphone output, and captures BlackHole's
input to prove another app can receive audible translated audio. The captured virtual-microphone
PCM is trimmed to its audible window, independently transcribed, and must retain the expected
English meaning. The untrimmed capture is still retained for debugging.

```bash
cd src-tauri
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY="sk-..." \
  cargo test --test openai_realtime_translation_e2e_test \
  live_translation_service_synthetic_voice_reaches_blackhole \
  -- --ignored --exact --nocapture
```

Short paid soak:

```bash
cd src-tauri
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY="sk-..." LIVE_AUDIO_SOAK_SECONDS=60 \
  cargo test --test openai_realtime_translation_e2e_test \
  live_translation_service_long_running_synthetic_voice_soak \
  -- --ignored --exact --nocapture
```

Both outgoing tests reject `.env` and `OPENAI_API_KEY`; only the explicitly acknowledged dedicated
test credential is accepted. The gate requires translated English text, nonzero translated audio
at BlackHole input, bounded stop, and Idle after cleanup.
Each short outgoing run writes full/audible virtual-microphone WAV files, both transcripts, and
metrics under `src-tauri/target/e2e-artifacts/outgoing-live-*`; override the directory with
`OUTGOING_TRANSLATION_E2E_ARTIFACTS`.

## Reproducible macOS Release Runners

The smoke runner combines BlackHole loopback, a nine-second no-drop incoming playback burst,
native capture format and self-exclusion, outgoing virtual-microphone translation, captions
regression, the full incoming linguistic/volume matrix, stop-mid-phrase tail preservation, and paid
full duplex:

```bash
cd frontend
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY="sk-..." npm run e2e:live-audio
```

The soak runner adds the constant-memory spoken WebSocket runtime soak, a native paid spoken
ScreenCaptureKit -> SystemDefault playback soak, plus the paid outgoing and captions long-session
checks. `LIVE_AUDIO_SOAK_SECONDS` applies to each long-running test:

```bash
cd frontend
VOICETEXT_RUN_PAID_E2E=1 OPENAI_E2E_API_KEY="sk-..." \
  LIVE_AUDIO_SOAK_SECONDS=1800 npm run e2e:live-audio-soak
```

Both runners reject `OPENAI_API_KEY` and do not read `.env`.

For a GitHub release, run the manual `macOS Audio Release Gate` workflow on the labeled self-hosted
test Mac. Pass its successful run ID to the manual `Release` workflow. Release publication verifies
the evidence artifact belongs to the exact tagged commit and records at least a 1,800-second soak;
a tag push alone cannot publish the feature. The release workflow also verifies the SHA-256
manifest for the complete paid matrix audio/transcript/metrics bundle.

The full-duplex gate starts real incoming and outgoing OpenAI sessions together. It stops incoming,
requires a second distinct outgoing phrase to reach BlackHole, restarts incoming, stops outgoing,
then requires a second distinct system-audio phrase to produce new Russian text and local PCM.
Independent transcription of the captured virtual-microphone WAV must retain both outgoing
phrases. Playback overflow is measured per incoming session and must stay below the production
overload threshold and one second of dropped audio.

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
- 30-minute native spoken ScreenCaptureKit -> realtime -> SystemDefault soak passes with durable RSS/queue/drop metrics;
- 25-cycle spoken restart stress leaves no capture, output, WebSocket, session, or task owner active;
- native 24 kHz capture/callback stop passes;
- native 440/880 Hz self-exclusion passes;
- paid OpenAI text and PCM test passes with a dedicated key;
- paid outgoing translation reaches the BlackHole virtual microphone;
- paid full duplex preserves both routes and both independent stop orders;
- a nine-second incoming playback burst reaches BlackHole without bounded-buffer overflow;
- output disconnect and sleep/wake manual checks produce terminal cleanup;
- captions-only remains the persisted default and works as the rollback path.
