#!/usr/bin/env bash
# Builds libstack_core.so (flutter_rust_bridge binding) for Android and copies it
# into the stack_mobile Flutter app's jniLibs so `flutter build apk` bundles it.
#
# Mirrors build/build-xcframework.sh in style. iOS uses the UniFFI staticlib via
# the xcframework; Android uses the FRB cdylib loaded from the APK's jniLibs.
set -euo pipefail

cd "$(dirname "$0")/.."

# --- Configuration -----------------------------------------------------------

# ABIs Flutter ships by default. cargo-ndk maps these to Rust targets:
#   arm64-v8a   -> aarch64-linux-android
#   armeabi-v7a -> armv7-linux-androideabi
#   x86_64      -> x86_64-linux-android
ABIS=(arm64-v8a armeabi-v7a x86_64)

# Output: the Flutter app's jniLibs. The two repos are siblings today
# (stack-connect-core and stack-connect). When stack-connect-core becomes a
# submodule of stack-connect this relative path stays valid (../../...).
JNI_LIBS="../stack-connect/flutter/apps/stack_mobile/android/app/src/main/jniLibs"

# Release build: ~10x smaller .so than debug, and the host FRB build already
# works in release. Drop --release below if you ever need faster iteration.
PROFILE="release"

# minSdk for the .so. 21 matches Flutter's default minSdk and is safe for FRB.
API_LEVEL="21"

# --- Toolchain detection -----------------------------------------------------

# ANDROID_HOME / ANDROID_NDK_HOME: honor the environment, else auto-detect the
# standard macOS Android Studio locations.
: "${ANDROID_HOME:=${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
export ANDROID_HOME

if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
  # Prefer the NDK this project is pinned to (build.gradle.kts ndkVersion),
  # else fall back to the newest NDK installed under the SDK.
  PINNED_NDK="$ANDROID_HOME/ndk/28.2.13676358"
  if [[ -d "$PINNED_NDK" ]]; then
    ANDROID_NDK_HOME="$PINNED_NDK"
  elif [[ -d "$ANDROID_HOME/ndk" ]]; then
    ANDROID_NDK_HOME="$(/bin/ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -1)"
  fi
fi
export ANDROID_NDK_HOME

if [[ ! -d "${ANDROID_NDK_HOME:-/nonexistent}" ]]; then
  echo "error: Android NDK not found. Set ANDROID_NDK_HOME or install an NDK under \$ANDROID_HOME/ndk." >&2
  exit 1
fi

echo "==> ANDROID_HOME=$ANDROID_HOME"
echo "==> ANDROID_NDK_HOME=$ANDROID_NDK_HOME"

# Make cargo-ndk available even if the caller didn't add ~/.cargo/bin to PATH.
export PATH="$HOME/.cargo/bin:$PATH"

# --- Build -------------------------------------------------------------------

# cargo-ndk's -o flag writes per-ABI subdirs (arm64-v8a/, armeabi-v7a/, ...)
# matching jniLibs layout exactly, so no manual per-ABI copying is needed.
mkdir -p "$JNI_LIBS"

NDK_TARGETS=()
for abi in "${ABIS[@]}"; do
  NDK_TARGETS+=(-t "$abi")
done

echo "==> Building libstack_core.so ($PROFILE) for: ${ABIS[*]}"
cargo ndk \
  --platform "$API_LEVEL" \
  "${NDK_TARGETS[@]}" \
  -o "$JNI_LIBS" \
  build --release -p stack_core --features frb

echo "==> Produced:"
for abi in "${ABIS[@]}"; do
  so="$JNI_LIBS/$abi/libstack_core.so"
  if [[ -f "$so" ]]; then
    printf '    %s (%s)\n' "$so" "$(du -h "$so" | cut -f1)"
  else
    echo "error: expected $so was not produced" >&2
    exit 1
  fi
done

echo "Done. Run 'flutter build apk' in stack_mobile to bundle these."
