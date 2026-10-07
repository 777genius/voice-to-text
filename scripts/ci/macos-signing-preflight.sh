#!/bin/bash
# Подключайте helper через source до Tauri, чтобы нормализованные API variables наследовались.
macos_signing_preflight() {
    if [ "${APPLE_TEAM_ID:-}" != 86399583GS ]; then
        echo "APPLE_TEAM_ID must be 86399583GS" >&2; return 1
    fi
    # Другие локальные release helpers используют APPLE_API_KEY как путь .p8.
    if [ -n "${APPLE_API_KEY_ID:-}" ]; then
        if [[ "${APPLE_API_KEY:-}" = /* ]]; then
            export APPLE_API_KEY_PATH="${APPLE_API_KEY_PATH:-$APPLE_API_KEY}"
        elif [ -n "${APPLE_API_KEY:-}" ] && [ "$APPLE_API_KEY" != "$APPLE_API_KEY_ID" ]; then
            echo "Conflicting notarization API key IDs" >&2; return 1
        fi
        export APPLE_API_KEY="$APPLE_API_KEY_ID"
    fi
    if ! [[ "${APPLE_API_KEY:-}" =~ ^[A-Z0-9]{10}$ ]] ||
       ! [[ "${APPLE_API_ISSUER:-}" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]] ||
       ! [[ "${APPLE_API_KEY_PATH:-}" = /*.p8 ]] ||
       [ ! -f "${APPLE_API_KEY_PATH:-}" ] || [ ! -r "${APPLE_API_KEY_PATH:-}" ]; then
        echo "Require API key ID, issuer UUID and readable absolute APPLE_API_KEY_PATH (.p8)" >&2; return 1
    fi
    local identity='Developer ID Application: ILLIA ZELENKO (86399583GS)'
    if [ -n "${APPLE_SIGNING_IDENTITY:-}" ] && [ "$APPLE_SIGNING_IDENTITY" != "$identity" ] &&
       [ "$APPLE_SIGNING_IDENTITY" != DBF74EF5BF85404EE5355248F22C883027857C94 ]; then
        echo "Unexpected APPLE_SIGNING_IDENTITY" >&2; return 1
    fi
    export APPLE_SIGNING_IDENTITY="$identity"
    unset APPLE_ID APPLE_PASSWORD
}
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    set -euo pipefail
    macos_signing_preflight
fi
