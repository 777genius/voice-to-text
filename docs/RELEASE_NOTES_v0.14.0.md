## What changed

### Recording reliability
- Deepgram streaming now handles keep-alive resume, finalize, drain, and close timing more safely.
- Late finalize text is preserved after drain acknowledgement, reducing missing last-word cases.
- Transcript events are delivered in order and stale session ids are guarded more carefully.

### Quiet session detection
- Recording sessions that receive microphone audio but produce no useful STT transcript are now flagged as suspiciously quiet.
- This makes microphone or STT pipeline failures easier to identify instead of silently ending with empty text.

### Auto-paste
- macOS auto-paste now prefers clipboard paste and adds focus/window guards before inserting text.
- The hotkey stop grace window keeps text visible while final STT events still arrive.

## Installation

**macOS:**
- Download the `.dmg` file for Intel or Apple Silicon.
- Drag VoicetextAI to Applications.

**Windows:**
- Download the `.msi` installer and run it.

**Linux:**
- Use `.deb` for Debian/Ubuntu.
- Use `.AppImage` for other distros.
