# Source, don't run: `. scripts/android-env.sh`
#
# What building, installing and testing the Android app needs on PATH and in
# the environment: the SDK, the newest NDK installed beside it, a JDK (17+) for
# Gradle, rustup's cargo (the Android targets live there), and Node 22+ for
# the interface. `cargo tauri android ...` sets the C toolchain itself; the
# CC/AR/linker variables are for a bare `cargo check --target ...-android`,
# which compiles C (sqlite, ring, aws-lc) for the phone and needs the NDK's
# clang to do it. See docs/mobile.md.

export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
if [ -z "${NDK_HOME:-}" ]; then
  NDK_HOME="$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -1)"
  export NDK_HOME
fi
# A JDK, not a JRE: Gradle compiles Kotlin and Java. The first with a javac
# among an unpacked one under ~/.local/share/jdk (no root needed to put one
# there) and the system's.
if [ -z "${JAVA_HOME:-}" ] || [ ! -x "$JAVA_HOME/bin/javac" ]; then
  for jdk in "$HOME"/.local/share/jdk/jdk-* /usr/lib/jvm/*; do
    if [ -x "$jdk/bin/javac" ]; then
      export JAVA_HOME="$jdk"
      break
    fi
  done
fi

PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$ANDROID_HOME/cmdline-tools/latest/bin:$HOME/.cargo/bin:$PATH"
for node in "$HOME"/.nvm/versions/node/v2[2-9]*/bin; do
  [ -d "$node" ] && PATH="$node:$PATH"
done
export PATH

# The lowest Android the app runs on: Tauri's own minimum, Android 7.0.
android_api=24
android_bin="$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
for triple in aarch64-linux-android x86_64-linux-android; do
  var=$(echo "$triple" | tr 'a-z-' 'A-Z_')
  low=$(echo "$triple" | tr '-' '_')
  export "CC_$low=$android_bin/$triple$android_api-clang"
  export "CXX_$low=$android_bin/$triple$android_api-clang++"
  export "AR_$low=$android_bin/llvm-ar"
  export "CARGO_TARGET_${var}_LINKER=$android_bin/$triple$android_api-clang"
done
unset android_api android_bin triple var low node jdk
