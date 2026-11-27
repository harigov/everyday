#!/usr/bin/env bash
# What building the desktop shell needs on macOS beyond Rust: Xcode (its
# command line tools build the C and C++ the app links, and the iOS target
# needs the whole of Xcode), and CMake, which builds llama.cpp -- with Metal
# on Apple Silicon, see crates/jatayu-service/Cargo.toml -- and libopus.
#
# The webview is the system's own WKWebView, so there are no webview headers
# to install, and CoreAudio needs nothing either. Nothing here uses sudo:
# accepting Xcode's licence and its first-launch step are the only things
# that do, and this says so rather than asking for a password.
set -euo pipefail

missing=0

if ! xcode-select -p >/dev/null 2>&1; then
    echo "Xcode's command line tools are not installed. Run: xcode-select --install" >&2
    missing=1
elif ! xcrun --find clang >/dev/null 2>&1; then
    echo "Xcode is installed but not ready. Run once, in a terminal:" >&2
    echo "  sudo xcodebuild -license accept && sudo xcodebuild -runFirstLaunch" >&2
    missing=1
fi

if ! command -v cmake >/dev/null 2>&1; then
    if command -v brew >/dev/null 2>&1; then
        echo "Installing CMake with Homebrew (llama.cpp and libopus build with it)"
        brew install cmake
    else
        echo "CMake is not installed: get it from https://cmake.org/download/ or Homebrew" >&2
        missing=1
    fi
fi

# The iPhone build needs the whole of Xcode, not just its command line tools,
# and the Rust targets for a phone and for the simulator. Only said, not
# done: a desktop build needs neither.
if [ "$(xcode-select -p 2>/dev/null)" = /Library/Developer/CommandLineTools ] &&
    [ -d /Applications/Xcode.app ]; then
    echo "note: for the iOS build, point the tools at Xcode itself:" >&2
    echo "  sudo xcode-select -s /Applications/Xcode.app" >&2
fi

exit "$missing"
