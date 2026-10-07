#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/macos-signing-preflight.sh"
test "${APPLE_TEAM_ID:-}" = 86399583GS
for name in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_API_KEY_BASE64 APPLE_API_KEY_ID APPLE_API_ISSUER; do
    test -n "${!name:-}" || { echo "Missing $name" >&2; exit 1; }
done
export APPLE_API_KEY_PATH="$RUNNER_TEMP/apple-notary-key.p8"
python3 - <<'PYTHON'
import base64, os, pathlib
for variable, name in [('APPLE_API_KEY_BASE64', 'apple-notary-key.p8'), ('APPLE_CERTIFICATE', 'certificate.p12')]:
    data = base64.b64decode(os.environ[variable], validate=True)
    if not data:
        raise SystemExit(f'Empty {variable}')
    path = pathlib.Path(os.environ['RUNNER_TEMP']) / name
    with path.open('wb') as stream:
        os.fchmod(stream.fileno(), 0o600)
        stream.write(data)
PYTHON
macos_signing_preflight
KEYCHAIN_PATH="$RUNNER_TEMP/app-signing.keychain-db"
KEYCHAIN_PASSWORD="$(openssl rand -base64 32)"
security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security set-keychain-settings -lut 21600 "$KEYCHAIN_PATH"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security import "$RUNNER_TEMP/certificate.p12" -P "$APPLE_CERTIFICATE_PASSWORD" \
    -A -t cert -f pkcs12 -k "$KEYCHAIN_PATH"
security set-key-partition-list -S apple-tool:,apple: -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
existing_keychains=()
while IFS= read -r keychain; do
    existing_keychains+=("${keychain//\"/}")
done < <(security list-keychains -d user)
security list-keychains -d user -s "$KEYCHAIN_PATH" "${existing_keychains[@]}"
security find-identity -v -p codesigning "$KEYCHAIN_PATH" | \
    grep -F 'DBF74EF5BF85404EE5355248F22C883027857C94 "Developer ID Application: ILLIA ZELENKO (86399583GS)"'
printf 'APPLE_API_KEY=%s\nAPPLE_API_KEY_PATH=%s\nAPPLE_SIGNING_IDENTITY=%s\n' \
    "$APPLE_API_KEY" "$APPLE_API_KEY_PATH" "$APPLE_SIGNING_IDENTITY" >> "$GITHUB_ENV"
