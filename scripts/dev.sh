#!/usr/bin/env bash
# Run the desktop app in development, under a memory cap.
#
# Requires the platform webview headers (see setup-linux.sh on Linux) and the
# Tauri CLI: `cargo install tauri-cli --version '^2'`.
set -euo pipefail
cd "$(dirname "$0")/.."

MEM="${EVERYDAY_DEV_MEM:-8G}"

# `cargo install` drops binaries in ~/.cargo/bin. rustup puts that on PATH;
# a distro-packaged cargo (/usr/bin/cargo) does not, so add it ourselves.
if [ -d "$HOME/.cargo/bin" ]; then
    PATH="$PATH:$HOME/.cargo/bin"
fi

if ! command -v cargo-tauri >/dev/null 2>&1; then
    echo "The Tauri CLI is not installed. Run:" >&2
    echo "  cargo install tauri-cli --version '^2' --locked" >&2
    exit 1
fi

cd crates/everyday-app
if command -v systemd-run >/dev/null 2>&1 &&
   systemd-run --user --scope -q -p MemoryMax=64M -- /bin/true >/dev/null 2>&1; then
    exec systemd-run --user --scope --quiet --collect \
        -p MemoryMax="$MEM" -p MemorySwapMax=0 -- cargo tauri dev
fi
exec cargo tauri dev
