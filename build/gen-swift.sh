#!/usr/bin/env bash
# Generates the Swift bindings from the built library (UniFFI library mode).
set -euo pipefail

cd "$(dirname "$0")/.."

OUT="build/generated/swift"
LIB_NAME="libstack_core.dylib"

mkdir -p "$OUT"

# Build the host cdylib that bindgen reads metadata from, plus the bindgen bin.
cargo build --release --features cli

cargo run --release --features cli --bin uniffi-bindgen -- generate \
  --library "target/release/${LIB_NAME}" \
  --language swift \
  --out-dir "$OUT"

# clang/xcframework expect the module map to be named exactly `module.modulemap`.
find "$OUT" -name '*FFI.modulemap' -exec sh -c 'mv "$1" "$(dirname "$1")/module.modulemap"' _ {} \;

# Mirror the generated wrapper into the consumable SwiftPM package.
PKG_SRC="bindings/swift/Sources/StackCoreRust"
mkdir -p "$PKG_SRC"
cp "$OUT/StackCoreRust.swift" "$PKG_SRC/StackCoreRust.swift"

echo "Swift bindings written to $OUT (wrapper mirrored to $PKG_SRC)"
ls -1 "$OUT"
