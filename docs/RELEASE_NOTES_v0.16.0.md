## What changed

### Incoming spoken translation on macOS
- Capture meeting or system audio and play translated speech through the selected output device.
- Choose spoken incoming delivery, adjust its volume, and mute the active incoming session without restarting it.
- See a feedback warning when incoming and outgoing spoken translation run at the same time.

### More resilient audio and translation sessions
- Recover system capture and local playback after output-device route changes.
- Keep realtime, STT, and audio queues bounded during long sessions and network failures.
- Clean up failed, suspended, stopped, and unexpectedly closed sessions without leaking runtime owners.
- Preserve graceful output drain, transcript spacing, and reliable warm dictation connections.

### Safer auto-paste on macOS
- Restore the previous clipboard after verified native TextEdit paste flows.
- Keep dictated text available for browser, terminal, editor, and unknown targets that may read the clipboard asynchronously.

### Release confidence
- Added full-duplex smoke coverage, measured long-session soaks, restart stress, semantic audio checks, and hardware recovery evidence.

## Installation

**macOS:**
- Download the `.dmg` file for Intel or Apple Silicon.
- Drag VoicetextAI to Applications.

**Windows:**
- Download the `.msi` installer and run it.

**Linux:**
- Use `.deb` for Debian/Ubuntu.
- Use `.AppImage` for other distributions.
