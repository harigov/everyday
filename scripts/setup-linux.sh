#!/usr/bin/env bash
# Install the system libraries the desktop shell links against on Linux.
#
# Only the GUI needs these. The core, the storage backends and the `karmayoga`
# CLI are portable Rust and build without them -- which is why `cargo test`
# works on a bare machine and `cargo tauri dev` does not.
#
# Four groups, for four reasons: the webview headers Tauri itself needs;
# PipeWire's development headers, for `capture.rs`'s loopback track (cpal's
# `pipewire` feature builds unconditionally on this target -- see
# `crates/karmayoga-app/Cargo.toml`'s own comment); libclang, because
# `libspa-sys`/`pipewire-sys` generate their bindings with `bindgen` at build
# time, and `bindgen` needs a real `libclang` to parse PipeWire's C headers
# against, not just the `clang` binary; and the Vulkan headers, SPIRV-Headers
# and the `glslc` shader compiler, for the llama.cpp the app builds to run a
# decision model on a graphics card (`crates/karmayoga-service/Cargo.toml`).
set -euo pipefail

echo "Karma Yoga needs the platform webview development headers, PipeWire's"
echo "headers, libclang (for bindgen) and the Vulkan headers and shader"
echo "compiler to build its desktop shell. This installs them with your system"
echo "package manager."
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
        libpipewire-0.3-dev \
        libasound2-dev \
        libspa-0.2-dev \
        libclang-dev \
        build-essential curl wget file pkg-config
    # Ubuntu 22.04 and Debian 11 package no `glslc`.
    if apt-cache show glslc >/dev/null 2>&1; then
        sudo apt-get install -y libvulkan-dev glslc spirv-headers
    else
        echo
        echo "This release has no glslc package. Fetch LunarG's Vulkan SDK instead:"
        echo '  export VULKAN_SDK=$(scripts/vulkan-sdk.sh ~/.cache/vulkan-sdk)'
    fi
elif command -v dnf >/dev/null 2>&1; then
    sudo dnf install -y \
        webkit2gtk4.1-devel javascriptcoregtk4.1-devel libsoup3-devel \
        gtk3-devel librsvg2-devel libappindicator-gtk3-devel \
        pipewire-devel alsa-lib-devel \
        clang-devel \
        vulkan-loader-devel vulkan-headers glslc spirv-headers-devel \
        openssl-devel curl wget file
elif command -v pacman >/dev/null 2>&1; then
    sudo pacman -S --needed \
        webkit2gtk-4.1 gtk3 libsoup3 librsvg libayatana-appindicator \
        libpipewire alsa-lib \
        clang \
        vulkan-icd-loader vulkan-headers shaderc spirv-headers \
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
