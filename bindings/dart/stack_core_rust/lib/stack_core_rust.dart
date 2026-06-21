/// Generated flutter_rust_bridge binding for the `stack_core` Rust crate.
///
/// Initialize the runtime once with [RustLib.init] before calling any binding
/// function. On host (macOS) for tests this loads the dylib by path via
/// [ExternalLibrary]; on device the default loader resolves the bundled library.
library;

// Host-path dylib loader, needed by tests to open the library explicitly.
// `ExternalLibrary` lives in the for-generated API surface, not the public one.
export 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart'
    show ExternalLibrary;

// Generated Rust binding surface (treated as read-only API).
export 'src/rust/frb_generated.dart' show RustLib;
export 'src/rust/frb_api.dart';
export 'src/rust/domain.dart';
export 'src/rust/error.dart';
export 'src/rust/service/kind.dart';
export 'src/rust/service/provider.dart';
