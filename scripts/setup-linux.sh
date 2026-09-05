#!/usr/bin/env bash
# Install the system libraries the desktop shell links against on Linux.
#
# Only the GUI needs these. The core, the storage backends and the `everyday`
# CLI are portable Rust and build without them -- which is why `cargo test`
# works on a bare machine and `cargo tauri dev` does not.
set -euo pipefail

echo "Every Day needs the platform webview development headers to build its"
echo "desktop shell. This installs them with your system package manager."
echo

if command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update
    sudo apt-get install -y \
        libwebkit2gtk-4.1-dev \
        libjavascriptcoregtk-4.1-dev \
        libsoup-3.0-dev \
        libgtk-3-dev \
        librsvg2-dev \
        libayatana-appindicator3-dev \
        build-essential curl wget file pkg-config
elif command -v dnf >/dev/null 2>&1; then
    sudo dnf install -y \
        webkit2gtk4.1-devel javascriptcoregtk4.1-devel libsoup3-devel \
        gtk3-devel librsvg2-devel libappindicator-gtk3-devel \
        openssl-devel curl wget file
elif command -v pacman >/dev/null 2>&1; then
    sudo pacman -S --needed \
        webkit2gtk-4.1 gtk3 libsoup3 librsvg libayatana-appindicator \
        base-devel curl wget file openssl
else
    echo "Unrecognised package manager." >&2
    echo "Install the Tauri v2 Linux prerequisites manually:" >&2
    echo "  https://tauri.app/start/prerequisites/" >&2
    exit 1
fi

echo
echo "Done. Verify with:"
echo "  pkg-config --exists webkit2gtk-4.1 && echo ok"
