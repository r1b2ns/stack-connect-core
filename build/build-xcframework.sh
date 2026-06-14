#!/usr/bin/env bash
# Builds StackCoreRust.xcframework for iOS (device + simulator) plus the Swift bindings.
set -euo pipefail

cd "$(dirname "$0")/.."

LIB="libstack_core.a"
OUT="bindings/swift/StackCoreRust.xcframework"
HEADERS="build/generated/headers"

IOS_DEVICE="aarch64-apple-ios"
IOS_SIM_ARM="aarch64-apple-ios-sim"
IOS_SIM_X86="x86_64-apple-ios"

# Match the SwiftPM package's minimum (iOS 17) so linked objects don't warn.
export IPHONEOS_DEPLOYMENT_TARGET="17.0"

rustup target add "$IOS_DEVICE" "$IOS_SIM_ARM" "$IOS_SIM_X86" >/dev/null

echo "==> Building static libs"
cargo build --release --target "$IOS_DEVICE"
cargo build --release --target "$IOS_SIM_ARM"
cargo build --release --target "$IOS_SIM_X86"

echo "==> Generating Swift bindings + headers"
./build/gen-swift.sh
rm -rf "$HEADERS"
mkdir -p "$HEADERS"
cp build/generated/swift/*.h "$HEADERS"/
cp build/generated/swift/module.modulemap "$HEADERS"/

echo "==> Merging simulator arches (arm64-sim + x86_64-sim)"
mkdir -p target/universal-sim/release
lipo -create \
  "target/${IOS_SIM_ARM}/release/${LIB}" \
  "target/${IOS_SIM_X86}/release/${LIB}" \
  -output "target/universal-sim/release/${LIB}"

echo "==> Assembling xcframework"
rm -rf "$OUT"
xcodebuild -create-xcframework \
  -library "target/${IOS_DEVICE}/release/${LIB}" -headers "$HEADERS" \
  -library "target/universal-sim/release/${LIB}" -headers "$HEADERS" \
  -output "$OUT"

echo "Built $OUT"
