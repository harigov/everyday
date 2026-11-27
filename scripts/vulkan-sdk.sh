#!/usr/bin/env bash
# Fetch LunarG's Vulkan SDK for Linux into a directory, check it against a
# SHA-256 pinned here, and print the path to point `VULKAN_SDK` at.
#
#   export VULKAN_SDK=$(scripts/vulkan-sdk.sh <dir>)
#
# The desktop app builds llama.cpp with its Vulkan backend on Linux (see
# `crates/jatayu-service/Cargo.toml`), which needs the Vulkan headers,
# SPIRV-Headers and the `glslc` shader compiler at build time. A
# distribution that packages them needs none of this: `scripts/setup-linux.sh`
# installs them. Ubuntu 22.04, which releases are built on, has no `glslc`,
# so the release build takes them from here instead -- as does anyone without
# root to install packages with.
#
# Only what the build reads is unpacked: the headers, `glslc`, the loader to
# link against and SPIRV-Headers' CMake package. The app itself links the
# system's `libvulkan.so.1` at run time, not this copy.
#
# Digest from LunarG's own listing:
#   https://vulkan.lunarg.com/sdk/sha/<version>/linux/vulkansdk-linux-x86_64-<version>.tar.xz.json
set -euo pipefail

dir=${1:?usage: vulkan-sdk.sh <dir>}

version=1.4.341.1
sha256=3bf0f762afb6c79bc6a9d9fb5998745ccff928800a29619b501ed9de7fd9789b
archive=vulkansdk-linux-x86_64-$version.tar.xz

[ "$(uname -s)-$(uname -m)" = Linux-x86_64 ] || {
  echo "LunarG's Linux SDK is for x86-64; install your distribution's Vulkan headers and glslc instead" >&2
  exit 1
}

sdk="$dir/$version/x86_64"
if [ ! -x "$sdk/bin/glslc" ]; then
  mkdir -p "$dir"
  path="$dir/$archive"
  if [ ! -f "$path" ]; then
    curl -fsSL --retry 3 -o "$path.part" \
      "https://sdk.lunarg.com/sdk/download/$version/linux/$archive"
    mv "$path.part" "$path"
  fi
  got=$(sha256sum "$path" | cut -d' ' -f1)
  if [ "$got" != "$sha256" ]; then
    rm -f "$path"
    echo "$archive: SHA-256 $got, expected $sha256" >&2
    exit 1
  fi
  tar -xJf "$path" -C "$dir" \
    "$version/x86_64/include" \
    "$version/x86_64/bin/glslc" \
    "$version/x86_64/lib/libvulkan.so" \
    "$version/x86_64/lib/libvulkan.so.1" \
    "$version/x86_64/lib/libvulkan.so.1.4.341" \
    "$version/x86_64/share/cmake/SPIRV-Headers"
  rm -f "$path"
  echo "$archive: ok" >&2
fi
echo "$sdk"
