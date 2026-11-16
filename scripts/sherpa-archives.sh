#!/usr/bin/env bash
# Fetch sherpa-onnx's prebuilt native libraries into a directory and check
# each against a SHA-256 pinned here, for `SHERPA_ONNX_ARCHIVE_DIR`.
#
#   scripts/sherpa-archives.sh <dir> <archive>...
#
# `sherpa-onnx-sys`'s build script downloads its archive from GitHub with no
# checksum, and the release build links that archive into the installers
# people run. Pointing the build at a directory this script has filled means
# a changed release asset fails here, loudly, instead of shipping.
#
# The version comes from Cargo.lock, so a bump that forgets this table fails
# rather than quietly checking nothing. Digests are GitHub's own for each
# release asset (`gh api repos/k2-fsa/sherpa-onnx/releases/tags/v<version>`).
set -euo pipefail

dir=${1:?usage: sherpa-archives.sh <dir> <archive>...}
shift
[ "$#" -gt 0 ] || { echo "name at least one archive" >&2; exit 2; }

here=$(cd "$(dirname "$0")/.." && pwd)
version=$(awk '/^name = "sherpa-onnx"$/{getline; print; exit}' "$here/Cargo.lock" | sed -E 's/version = "(.*)"/\1/')
[ -n "$version" ] || { echo "could not find sherpa-onnx in Cargo.lock" >&2; exit 1; }

pinned() {
  case "$1" in
    sherpa-onnx-v1.13.8-linux-x64-static-lib.tar.bz2) echo e1fdc5b67530e15741ef897fa5ffff297056f3bf0c6d829a27af9225a4c4b5a6 ;;
    sherpa-onnx-v1.13.8-osx-arm64-static-lib.tar.bz2) echo 9091bf160dc7fdacedbc906b212badf53c2993f4e5277a0e03998e96c31d60da ;;
    sherpa-onnx-v1.13.8-osx-x64-static-lib.tar.bz2) echo a3f88da3e54c850a12d61431e73f8affcd1f13738b75b768847dd79541835b4b ;;
    sherpa-onnx-v1.13.8-win-x64-static-MT-Release-lib.tar.bz2) echo 56ffcf3c454c1f14f7bc9887286cc8143e7e542dc632804e1c447d5f8d534eaf ;;
    *) return 1 ;;
  esac
}

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

mkdir -p "$dir"
for archive in "$@"; do
  archive=${archive//\{version\}/$version}
  want=$(pinned "$archive") || {
    echo "no pinned SHA-256 for $archive (sherpa-onnx $version); add it to $0" >&2
    exit 1
  }
  path="$dir/$archive"
  if [ ! -f "$path" ]; then
    curl -fsSL --retry 3 -o "$path.part" \
      "https://github.com/k2-fsa/sherpa-onnx/releases/download/v$version/$archive"
    mv "$path.part" "$path"
  fi
  got=$(sha256 "$path")
  if [ "$got" != "$want" ]; then
    rm -f "$path"
    echo "$archive: SHA-256 $got, expected $want" >&2
    exit 1
  fi
  echo "$archive: ok"
done
