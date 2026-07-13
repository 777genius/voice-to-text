# Live Translation

VoicetextAI supports outgoing translated speech, incoming translated captions, and incoming
translated speech on macOS.

## What It Does

- Outgoing voice: your microphone speech goes to OpenAI realtime translation, then English speech is played into a platform virtual microphone.
- Incoming text: system audio goes through transcription and translation, then captions appear in the recording popover.
- Incoming text and audio on macOS: system audio goes directly through OpenAI realtime translation; translated text appears in the popover and translated speech plays through the system default output.
- Dictation mode stays separate. Auto-paste, auto-copy, and history do not run while live translation is active.

## Requirements

- VoicetextAI account authorization in the app.
- OpenAI API key for translation.
- macOS: `BlackHole 2ch` is required only for outgoing translated voice. Incoming translation requires Screen and System Audio Recording permission. Incoming spoken playback uses the current system default output.
- Windows: VB-CABLE. VoicetextAI writes to `CABLE Input`; meeting apps should use `CABLE Output` as microphone.
- Linux: PulseAudio or PipeWire-Pulse tools: `pactl`, `pacat`, `parec`. VoicetextAI creates `VoicetextAI Virtual Microphone`.

## Setup

1. Download and install the app from [voicetext.site](https://voicetext.site).
2. Install the virtual audio dependency for your OS:
   - macOS: install `BlackHole 2ch`, then restart macOS if the device does not appear.
   - Windows: install VB-CABLE, then restart Windows if the device does not appear.
   - Linux: make sure PulseAudio or PipeWire-Pulse tools are available.
3. Open VoicetextAI Settings.
4. Select `Live translation` in Recording mode.
5. Paste your OpenAI API key in the OpenAI API Key field. If the field is empty, the app falls back to `OPENAI_API_KEY`.
6. In Google Meet, Zoom, or another call app, choose the virtual microphone:
   - macOS: `BlackHole 2ch`
   - Windows: `CABLE Output`
   - Linux: `VoicetextAI Virtual Microphone`
7. Press the VoicetextAI recording hotkey to start or stop outgoing translation.
8. In Incoming translation settings, choose `Text only` or `Text and audio`. The paid spoken mode is never enabled automatically after an upgrade.
9. Use the incoming translation button in the recording popover to start or stop system-audio translation. The speaker button mutes only translated playback and does not reconnect OpenAI.

## Current Limits

- Outgoing voice is fixed to English for this MVP.
- Incoming spoken playback is enabled only on macOS. Windows contracts are kept separate for a future adapter; Linux spoken playback is not implemented.
- Incoming spoken translation supports these targets: English, Spanish, Portuguese, French, Japanese, Russian, Chinese, German, Korean, Hindi, Indonesian, Vietnamese, and Italian.
- The original call audio remains audible. VoicetextAI does not duck it and does not provide speaker
  acoustic echo cancellation. Realtime translated speech can be quieter than the original; for the
  current mix, keep incoming translated volume at 100% and set the Zoom/Meet speaker volume to about
  40-50%. The paid incoming matrix includes a half-volume source regression case.
- ScreenCaptureKit excludes audio produced by VoicetextAI from incoming capture. With simultaneous outgoing translation, headphones are recommended so translated speaker audio does not acoustically enter the physical microphone.
- The first macOS version follows the system default output. Changing or disconnecting the output device can stop the session; select the new device and restart incoming translation.
- There is no incoming output-device selector yet.
- The OpenAI key is stored locally in app config. Future hardening should move this to Keychain or a backend proxy.

Operational and release verification commands are documented in
[`INCOMING_SPOKEN_TRANSLATION_TESTING.md`](./INCOMING_SPOKEN_TRANSLATION_TESTING.md).

## Code Entry Points

- Outgoing service: `src-tauri/src/application/services/live_translation_service.rs`
- OpenAI realtime translation client: `src-tauri/src/infrastructure/openai/realtime_translation.rs`
- Audio routing port: `src-tauri/src/domain/ports/translation_audio_output.rs`
- Platform factory: `src-tauri/src/infrastructure/audio/platform_factory.rs`
- macOS/Windows virtual microphone output: `src-tauri/src/infrastructure/audio/cpal_output.rs`
- Linux virtual microphone output: `src-tauri/src/infrastructure/audio/linux_pulse.rs`
- Incoming delivery facade: `src-tauri/src/application/services/incoming_translation_facade.rs`
- Incoming captions service: `src-tauri/src/application/services/incoming_caption_translation_service.rs`
- Incoming spoken service: `src-tauri/src/application/services/incoming_spoken_translation_service.rs`
- macOS system audio capture: `src-tauri/src/infrastructure/audio/macos_system_audio_capture.rs`
- Windows system loopback capture: `src-tauri/src/infrastructure/audio/windows_wasapi_loopback_capture.rs`
- OpenAI text translation client: `src-tauri/src/infrastructure/openai/text_translation.rs`
- Incoming settings UI: `src/features/settings/presentation/components/sections/IncomingTranslationSection.vue`
