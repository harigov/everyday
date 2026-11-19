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
    sherpa-onnx-v2.0.1-linux-x64-static-lib.tar.bz2) echo 55cfadf65ba50d0afc1a802b7a337942819e51c94a5b5a384def1978f7c70871 ;;
    sherpa-onnx-v2.0.1-osx-arm64-static-lib.tar.bz2) echo d7a9b14f337958e7373c8c650f046df33d210bbaff2f33cb8322dda663ee5e1c ;;
    sherpa-onnx-v2.0.1-osx-x64-static-lib.tar.bz2) echo 6a3cb3c14104d3a02e7b2a6cd5621f590b14892cce7d0bc0935f7655ef518938 ;;
    sherpa-onnx-v2.0.1-win-x64-static-MT-Release-lib.tar.bz2) echo 23e7b986b197eda0bbc47ff3d7d5c87afd6ef2b9077214ba26b0f1ec52ef0e66 ;;
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
