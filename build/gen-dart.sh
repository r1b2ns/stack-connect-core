#!/usr/bin/env bash
# Generates the Dart bindings (flutter_rust_bridge) for the stack_core crate.
#
# Mirrors build/gen-swift.sh: the generated sources land in the standalone
# binding package under bindings/dart/stack_core_rust and are gitignored. The
# codegen also (re)writes the core crate's frb_generated.rs glue with the
# matching FFI symbol prefix, so rebuild the native library afterwards
# (`cargo build --features frb`).
set -euo pipefail

cd "$(dirname "$0")/.."

PKG_DIR="bindings/dart/stack_core_rust"

cd "$PKG_DIR"

# Resolve Dart/Flutter deps so codegen + build_runner can run.
flutter pub get

# FRB codegen reads flutter_rust_bridge.yaml in this dir. Emits lib/src/rust/*
# (Dart) and rewrites <core>/crates/stack_core/src/frb_generated.rs (Rust glue).
flutter_rust_bridge_codegen generate

# Emit the freezed classes (e.g. error.freezed.dart) the codegen output relies on.
dart run build_runner build --delete-conflicting-outputs

echo "Dart bindings written to ${PKG_DIR}/lib/src/rust"
ls -1 lib/src/rust
