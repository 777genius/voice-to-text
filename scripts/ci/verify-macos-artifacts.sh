#!/bin/bash
set -euo pipefail
shopt -s nullglob
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUNDLE_DIR="${1:?Usage: verify-macos-artifacts.sh BUNDLE_DIR RECEIPT}"
RECEIPT="${2:?Usage: verify-macos-artifacts.sh BUNDLE_DIR RECEIPT}"
TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/voicetext-artifacts.XXXXXX")"
MOUNT_POINT=""
cleanup() {
    local status=$?
    if [ -n "$MOUNT_POINT" ] && ! hdiutil detach "$MOUNT_POINT"; then
        echo "Could not detach owned mount; retaining $TEMP_ROOT" >&2
        exit 1
    fi
    rm -rf "$TEMP_ROOT"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
verify_contents() {
    local bundles=("$1"/*.app)
    test "${#bundles[@]}" -eq 1
    test "${bundles[0]}" = "$1/VoicetextAI.app"
    test -d "${bundles[0]}" && test ! -L "${bundles[0]}"
    bash "$SCRIPT_DIR/verify-macos-signing.sh" "${bundles[0]}" --notarized
}
tars=("$BUNDLE_DIR"/macos/*.app.tar.gz)
dmgs=("$BUNDLE_DIR"/dmg/*.dmg)
test "${#tars[@]}" -gt 0 && test "${#dmgs[@]}" -gt 0
for archive in "${tars[@]}"; do
    extract_root="$(mktemp -d "$TEMP_ROOT/tar.XXXXXX")"
    tar -tzf "$archive" > "$TEMP_ROOT/members"
    if grep -Eq '(^/|(^|/)\.\.(/|$))' "$TEMP_ROOT/members"; then
        echo "Unsafe archive member path" >&2; exit 1
    fi
    tar -xzf "$archive" -C "$extract_root"
    verify_contents "$extract_root"
done
for archive in "${dmgs[@]}"; do
    MOUNT_POINT="$(mktemp -d "$TEMP_ROOT/mount.XXXXXX")"
    hdiutil attach "$archive" -readonly -nobrowse -mountpoint "$MOUNT_POINT"
    verify_contents "$MOUNT_POINT"
    hdiutil detach "$MOUNT_POINT"
    MOUNT_POINT=""
done
python3 - "$RECEIPT" "${tars[@]}" "${dmgs[@]}" <<'PYTHON'
import hashlib, json, os, pathlib, subprocess, sys
source = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
assert source == os.environ['GITHUB_SHA'], 'checkout source mismatch'
assert source == os.environ['GITHUB_WORKFLOW_SHA'], 'workflow source mismatch'
def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()
report = dict(schemaVersion=1, sourceSHA=source, workflowSHA=os.environ['GITHUB_WORKFLOW_SHA'],
              workflowRef=os.environ['GITHUB_WORKFLOW_REF'], runID=os.environ['GITHUB_RUN_ID'],
              runAttempt=os.environ['GITHUB_RUN_ATTEMPT'], signingTeam='86399583GS',
              notarizationVerified=True, transportedAppsVerified=True,
              archives=[dict(path=name, sha256=digest(pathlib.Path(name))) for name in sys.argv[2:]])
with pathlib.Path(sys.argv[1]).open('x') as stream:
    json.dump(report, stream, sort_keys=True, separators=(',', ':'))
    stream.write('\n')
PYTHON
