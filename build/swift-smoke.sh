#!/usr/bin/env bash
# Host cross-FFI smoke: links the macOS staticlib and drives the Swift binding
# through Rust (error bridging + foreign-trait callback). See bindings/swift/smoke/main.swift.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Building macOS staticlib"
cargo build --release --target aarch64-apple-darwin

echo "==> Generating Swift bindings"
./build/gen-swift.sh >/dev/null

WORK="target/swift-smoke"
rm -rf "$WORK"
mkdir -p "$WORK/Headers"
cp build/generated/swift/StackCoreRustFFI.h build/generated/swift/module.modulemap "$WORK/Headers/"
cp build/generated/swift/StackCoreRust.swift "$WORK/"

echo "==> Compiling + running smoke"
swiftc -I "$WORK/Headers" \
  "$WORK/StackCoreRust.swift" bindings/swift/smoke/main.swift \
  -L target/aarch64-apple-darwin/release -lstack_core \
  -framework CoreFoundation -framework Security -framework SystemConfiguration \
  -o "$WORK/smoke"

"$WORK/smoke"
