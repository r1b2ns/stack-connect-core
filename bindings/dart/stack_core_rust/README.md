# stack_core_rust

The generated [flutter_rust_bridge](https://cjycode.com/flutter_rust_bridge/)
(FRB) Dart binding for the `stack_core` Rust crate.

This is the Dart counterpart to the Swift binding under `bindings/swift`. Just
like the Swift `StackCoreRust.swift` wrapper, the generated Dart sources here
(`lib/src/rust/`) are **gitignored** and regenerated locally — only this
package's manifest, barrel (`lib/stack_core_rust.dart`), tooling and lint config
are tracked.

## Regenerating

From the core repo root:

```sh
just gen-dart
# or directly:
./build/gen-dart.sh
```

This runs `flutter_rust_bridge_codegen generate` (reading
`flutter_rust_bridge.yaml`) followed by `build_runner` to emit the freezed
classes, writing into `lib/src/rust/`. The codegen also rewrites the core
crate's `crates/stack_core/src/frb_generated.rs` glue with the matching FFI
symbol prefix, so the native library must be rebuilt afterwards:

```sh
cargo build --release --features frb   # or --features frb (debug) for tests
```

## Consuming

The Flutter package `stack_core_dart` (in the sibling `stack-connect` repo)
depends on this package via a path dependency and re-exports its surface, so app
code keeps importing `package:stack_core_dart/stack_core_dart.dart` unchanged.
