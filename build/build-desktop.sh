#!/usr/bin/env bash
# Builds the flutter_rust_bridge (FRB) desktop cdylib for the stack_desktop app
# (Windows `.dll`, optionally Linux `.so`).
#
# Mirrors build/build-android.sh in style. Android loads the FRB cdylib from the
# APK's jniLibs; desktop loads it from next to the executable (bundled per
# platform via the app's CMake / Flutter native-assets step).
#
# IMPORTANT — runs in CI, not on macOS. `stack_desktop` targets Windows (and
# Linux); there is no `macos/` desktop runner. Cross-compiling to
# `x86_64-pc-windows-msvc` needs the MSVC toolchain, so in practice this script
# runs on the matching platform's CI runner (a Windows runner for the `.dll`, a
# Linux runner for the `.so`). On a host that CAN target the triple it also works
# locally for `flutter run -d <platform>`.
set -euo pipefail

cd "$(dirname "$0")/.."

# --- Configuration -----------------------------------------------------------

# First arg selects the desktop target. Accepts a friendly name or a raw triple.
#   windows (default) -> x86_64-pc-windows-msvc   -> stack_core.dll
#   linux             -> x86_64-unknown-linux-gnu -> libstack_core.so
TARGET_ARG="${1:-windows}"

case "$TARGET_ARG" in
  windows) TRIPLE="x86_64-pc-windows-msvc"  ; LIB_FILE="stack_core.dll"    ;;
  linux)   TRIPLE="x86_64-unknown-linux-gnu"; LIB_FILE="libstack_core.so"  ;;
  *)       TRIPLE="$TARGET_ARG"  # raw triple; infer the artifact name below
           case "$TRIPLE" in
             *windows*) LIB_FILE="stack_core.dll"   ;;
             *apple*)   LIB_FILE="libstack_core.dylib" ;;
             *)         LIB_FILE="libstack_core.so"  ;;
           esac ;;
esac

PROFILE="release"

# Optional copy destination for the built library (e.g. the app's bundle dir or
# a CI staging dir). When unset the script only builds + reports the path; the
# desktop app is responsible for bundling the lib next to its executable (CMake).
DEST="${DEST:-}"

# --- Build -------------------------------------------------------------------

export PATH="$HOME/.cargo/bin:$PATH"

# Best-effort: make sure the std lib for the target is installed.
if command -v rustup >/dev/null 2>&1; then
  rustup target add "$TRIPLE" >/dev/null 2>&1 || true
fi

echo "==> Building $LIB_FILE ($PROFILE) for: $TRIPLE"
cargo build --release -p stack_core --features frb --target "$TRIPLE"

ARTIFACT="target/$TRIPLE/$PROFILE/$LIB_FILE"
if [[ ! -f "$ARTIFACT" ]]; then
  echo "error: expected $ARTIFACT was not produced" >&2
  exit 1
fi

echo "==> Produced: $ARTIFACT ($(du -h "$ARTIFACT" | cut -f1))"

# --- Optional copy -----------------------------------------------------------

if [[ -n "$DEST" ]]; then
  mkdir -p "$DEST"
  cp "$ARTIFACT" "$DEST/"
  echo "==> Copied to: $DEST/$LIB_FILE"
else
  echo "Done. Set DEST=<dir> to copy the library, or bundle $LIB_FILE next to the"
  echo "stack_desktop executable (CMake) so FRB resolves it by stem 'stack_core'."
fi
