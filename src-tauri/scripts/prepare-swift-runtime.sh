#!/usr/bin/env bash
set -euo pipefail

if [[ "${TAURI_ENV_PLATFORM:-}" != "darwin" && "$(uname -s)" != "Darwin" ]]; then
  exit 0
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tauri_dir="$(cd "$script_dir/.." && pwd)"
dest_dir="$tauri_dir/target/swift-runtime"
dest="$dest_dir/libswift_Concurrency.dylib"

developer_dir="$(xcode-select -p 2>/dev/null || true)"
candidates=(
  "$developer_dir/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx/libswift_Concurrency.dylib"
  "$developer_dir/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx/libswift_Concurrency.dylib"
  "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx/libswift_Concurrency.dylib"
  "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx/libswift_Concurrency.dylib"
)

src=""
for candidate in "${candidates[@]}"; do
  if [[ -f "$candidate" ]]; then
    src="$candidate"
    break
  fi
done

if [[ -z "$src" ]]; then
  echo "libswift_Concurrency.dylib not found in Xcode Swift runtime paths" >&2
  exit 1
fi

mkdir -p "$dest_dir"
cp "$src" "$dest"
chmod 755 "$dest"

echo "Copied Swift Concurrency runtime: $src -> $dest"
