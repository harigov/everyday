#!/usr/bin/env bash
# Fetch the tools Tauri's AppImage bundler runs, check each against a SHA-256
# pinned here, and put them in the directory the bundler looks in first.
#
#   scripts/appimage-tools.sh <cache-dir> [arch]
#
# `cargo tauri build --bundles appimage` downloads five things before it
# assembles an AppImage -- AppRun, linuxdeploy, linuxdeploy's GTK and
# GStreamer plugins, and its AppImage plugin -- with no checksum, three of
# them from URLs whose content changes under the same name (a `master` branch
# and a `continuous` release). The first two are executed on the runner and
# the whole set ends up inside what people install. The bundler only
# downloads a file that is not already in its tools directory
# (`if !path.exists()` in crates/tauri-bundler/src/bundle/linux/appimage/
# linuxdeploy.rs), so filling that directory first, from this table, means a
# changed upstream file fails here, loudly, instead of shipping.
#
# <cache-dir> is where the bundler looks: `$XDG_CACHE_HOME/tauri`, or
# `~/.cache/tauri` (it asks the `dirs` crate for the cache directory, and
# `bundle.useLocalToolsDir` is off). <arch> defaults to this machine's; the
# bundler names the tools after the first part of the build target, which on
# Linux is `uname -m`. Only x86_64 is built by release.yml, so it is the only
# one pinned; another architecture fails below rather than being left to the
# bundler's unchecked download.
#
# The table is for @tauri-apps/cli 2.11.4 (tauri-bundler 2.9.4), the version
# pinned in ui/package.json. Moving that means reading linuxdeploy.rs's
# `prepare_tools` again: a file or URL it gained is downloaded unchecked, and
# one it lost would be fetched here for nothing.
#
# How the digests were made, so they can be made again: each URL was fetched
# on 2026-11-20 and hashed (GitHub's own `digest` for the release assets
# agrees where it publishes one). Three are mutable upstream, so the pin is
# the content as of that day, not a promise about the name:
#   - linuxdeploy-plugin-gtk.sh and linuxdeploy-plugin-gstreamer.sh are read
#     at the commit `master` pointed at then, not at `master`, so the URL
#     stays good; the bundler reads `master`, and a different file there is
#     what this script keeps out.
#   - linuxdeploy-plugin-appimage is the `continuous` release, rebuilt
#     upstream (its asset was last replaced 2026-10-01). When it is, this
#     fails with a SHA-256 mismatch: look at what changed, then update the
#     digest and size here. The bundler would fall back to linuxdeploy's
#     built-in copy without it, so dropping it is not an option -- an
#     unpinned fallback is what this replaces.
#
# The bundler also rewrites three bytes of the linuxdeploy AppImage in place
# (a `dd` over its magic, so desktop integration tools ignore it). A tools
# directory a build has already used therefore no longer matches its digest;
# a file that does not match is fetched again rather than trusted, so this
# is safe to re-run on a machine that has built before.
set -euo pipefail

dir=${1:?usage: appimage-tools.sh <cache-dir> [arch]}
arch=${2:-$(uname -m)}

# <name in the cache> <sha256> <size in bytes> <url>
tools() {
  case "$arch" in
    x86_64)
      cat <<'EOF'
AppRun-x86_64 f30140a43a0a59e46db21bdefdf749b9e9f2c6946e92afabbacf98b8ae73fb4f 31552 https://github.com/tauri-apps/binary-releases/releases/download/apprun-old/AppRun-x86_64
linuxdeploy-x86_64.AppImage e762bea85c8eb0d4b3508d46e5c1f037f717d0f9303ae3b4aafc8b04991fa1ef 13264064 https://github.com/tauri-apps/binary-releases/releases/download/linuxdeploy/linuxdeploy-x86_64.AppImage
linuxdeploy-plugin-gtk.sh 7804c9eef13e59bf2783aad9882ef9db8f3f3f9e8d631874b1d348d550a3693f 14622 https://raw.githubusercontent.com/tauri-apps/linuxdeploy-plugin-gtk/dda522bce37387f1b853d9095713bfaa924c8423/linuxdeploy-plugin-gtk.sh
linuxdeploy-plugin-gstreamer.sh c107b49d84edbffc6ab226ed1007e0626a4f7aa2c3a36b7782bef62351d49e94 4857 https://raw.githubusercontent.com/tauri-apps/linuxdeploy-plugin-gstreamer/2a2e67491c32995a3f279ad0ecbe77abd512b42a/linuxdeploy-plugin-gstreamer.sh
linuxdeploy-plugin-appimage.AppImage 49d6a17160675a6bd1781699aae6bdf7692d98552e02a3671d2183d10547842e 16488952 https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-x86_64.AppImage
EOF
      ;;
    *)
      echo "no pinned AppImage tools for $arch; add them to $0" >&2
      return 1
      ;;
  esac
}

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

table=$(tools)
mkdir -p "$dir"
while read -r name want size url; do
  path="$dir/$name"
  if [ -f "$path" ] && [ "$(sha256 "$path")" = "$want" ]; then
    echo "$name: ok (already here)"
    continue
  fi
  # Whatever is there is not the pinned file: a bundler's patched copy, or
  # something else. Replace it; never keep it.
  rm -f "$path" "$path.part"
  # `--max-filesize` stops a changed asset filling the runner's disk before
  # the digest gets a say.
  curl -fsSL --retry 3 --max-filesize "$((size * 2))" -o "$path.part" "$url"
  got=$(sha256 "$path.part")
  if [ "$got" != "$want" ]; then
    rm -f "$path.part"
    echo "$name: SHA-256 $got, expected $want ($url)" >&2
    exit 1
  fi
  # The mode the bundler gives what it downloads itself.
  chmod 770 "$path.part"
  mv "$path.part" "$path"
  echo "$name: ok"
done <<<"$table"
