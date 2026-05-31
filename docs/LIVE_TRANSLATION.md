# Live Translation

VoicetextAI `0.10.30` adds a live translation mode for calls and a first incoming subtitle mode.

## What It Does

- Outgoing voice: your microphone speech goes to OpenAI realtime translation, then English speech is played into a platform virtual microphone.
- Incoming subtitles: system audio goes through transcription and OpenAI text translation, then Russian subtitles appear in the recording popover.
- Dictation mode stays separate. Auto-paste, auto-copy, and history do not run while live translation is active.

## Requirements

- VoicetextAI account authorization in the app.
- OpenAI API key for translation.
- macOS: `BlackHole 2ch` for outgoing translated voice, plus Screen and System Audio Recording permission for incoming subtitles.
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
8. Use the incoming translation button in the recording popover to show translated system-audio subtitles.

## Current Limits

- Outgoing voice is fixed to English for this MVP.
- Incoming translation is text-only subtitles, not translated voice in headphones yet.
- The virtual microphone output is auto-detected. There is no output device selector yet.
- The OpenAI key is stored locally in app config. Future hardening should move this to Keychain or a backend proxy.

## Code Entry Points

- Outgoing service: `src-tauri/src/application/services/live_translation_service.rs`
- OpenAI realtime translation client: `src-tauri/src/infrastructure/openai/realtime_translation.rs`
- Audio routing port: `src-tauri/src/domain/ports/translation_audio_output.rs`
- Platform factory: `src-tauri/src/infrastructure/audio/platform_factory.rs`
- macOS/Windows virtual microphone output: `src-tauri/src/infrastructure/audio/cpal_output.rs`
- Linux virtual microphone output: `src-tauri/src/infrastructure/audio/linux_pulse.rs`
- Incoming subtitles service: `src-tauri/src/application/services/incoming_translation_service.rs`
- macOS system audio capture: `src-tauri/src/infrastructure/audio/macos_system_audio_capture.rs`
- Windows system loopback capture: `src-tauri/src/infrastructure/audio/windows_wasapi_loopback_capture.rs`
- OpenAI text translation client: `src-tauri/src/infrastructure/openai/text_translation.rs`
- Settings UI: `src/features/settings/presentation/components/sections/RecordingModeSection.vue`
