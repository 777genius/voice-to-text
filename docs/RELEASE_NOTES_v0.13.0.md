## What changed

### Recording hotkeys
- Added an optional double Space recording hotkey.
- The hotkey is off by default and can be enabled in Settings.
- When triggered from a text field, the two inserted spaces are removed automatically.

### Recording performance
- Microphone capture now uses a fast integer downsample path for common 48 kHz to 16 kHz input.
- The audio visualizer sends and renders fewer UI-only spectrum updates while keeping STT audio unchanged.
- Microphone test is blocked while recording is already active.

### Landing page
- The open-source section now shows recent GitHub releases.
- Release downloads can fall back to a static snapshot and refresh from GitHub afterward.
- The free pricing card is more compact and clearer.

### Developer runtime
- Debug builds use a separate bundle id and deep-link scheme.
- The updater is disabled in debug builds.
- A prod-backend dev script is available for local frontend testing against the production API.

## Fixes

- Recording error details can be opened from the mini recording window.
- Debug builds no longer look like the production app to updater/focus logic.

## Installation

**macOS:**
- Download the `.dmg` file for Intel or Apple Silicon.
- Drag VoicetextAI to Applications.

**Windows:**
- Download the `.msi` installer and run it.

**Linux:**
- Use `.deb` for Debian/Ubuntu.
- Use `.AppImage` for other distros.
