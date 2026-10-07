#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/ci/macos-signing-preflight.sh"
macos_signing_preflight
exec pnpm tauri build "$@"
