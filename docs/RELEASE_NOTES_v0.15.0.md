## What changed

### Live translation reliability
- Stabilized live translation sessions across audio capture, frontend state, and OpenAI text/translation settings.
- Hardened realtime translation failure handling so broken sessions close cleanly instead of lingering.
- Normalized translation target languages and trimmed OpenAI model/API key settings before use.

### Recording session handling
- Restored recording state after failed starts.
- Guarded keep-alive resume while a recording session is already active.
- Closed failed terminal transcription sessions more precisely.

### Quality gates
- Added live audio smoke coverage and late session event regression tests.
- Hardened async listener lifecycle, settings persistence, external auth URL opening, and live audio runner parsing.

## Installation

**macOS:**
- Download the `.dmg` file for Intel or Apple Silicon.
- Drag VoicetextAI to Applications.

**Windows:**
- Download the `.msi` installer and run it.

**Linux:**
- Use `.deb` for Debian/Ubuntu.
- Use `.AppImage` for other distros.
