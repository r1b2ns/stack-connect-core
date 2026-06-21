#!/usr/bin/env bash
# Generates the Dart bindings (flutter_rust_bridge) for the stack_core crate.
#
# Mirrors build/gen-swift.sh: the generated sources land in the standalone
# binding package under bindings/dart/stack_core_rust and are gitignored. The
# codegen also (re)writes the core crate's frb_generated.rs glue with the
# matching FFI symbol prefix, so rebuild the native library afterwards
# (`cargo build --features frb`).
set -euo pipefail

# Capture the repo root in its canonical (symlink-resolved) form BEFORE we cd
# into the package dir, so we can derive the absolute rust_root/rust_output paths
# the codegen needs. These two paths used to be hardcoded (and committed) in
# flutter_rust_bridge.yaml, which forced a machine-specific edit on every host
# switch (macOS <-> Windows). They are 100% derivable from the repo layout, so
# we compute them here and inject them via CLI flags (which take precedence over
# the yaml).
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

PKG_DIR="bindings/dart/stack_core_rust"

# Fixed repo layout: the FRB facade crate is <repo>/crates/stack_core and its
# generated glue lands at <crate>/src/frb_generated.rs.
RUST_ROOT_DIR="${REPO_ROOT}/crates/stack_core"

# FRB computes the generated module path by doing a *literal* prefix-match of
# rust_output against rust_root. Both must therefore be in the SAME canonical
# form, or the match breaks. We canonicalize the crate DIRECTORY (which always
# exists) and append the rust_output filename manually -- we must NOT
# canonicalize frb_generated.rs directly, because it may not exist yet on a
# clean checkout (realpath would fail).
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*)
    # Windows under Git Bash / MSYS. FRB's internal std::fs::canonicalize emits
    # the extended-length `\\?\` prefix for rust_root (especially under OneDrive),
    # so rust_output must carry the exact same prefix/form to satisfy the literal
    # prefix-match. Convert to a Windows path with cygpath, then prepend `\\?\`
    # and append the output sub-path using backslashes -- keeping rust_output as
    # rust_root + `\src\frb_generated.rs` guarantees the prefixes line up.
    RUST_ROOT_WIN="$(cygpath -w "$RUST_ROOT_DIR")"
    RUST_ROOT="\\\\?\\${RUST_ROOT_WIN}"
    RUST_OUTPUT="${RUST_ROOT}\\src\\frb_generated.rs"
    ;;
  *)
    # macOS / Linux. Clean POSIX form, no `\\?\` prefix. realpath resolves the
    # crate dir; we append the output sub-path so we never touch the (possibly
    # missing) generated file.
    RUST_ROOT="$(realpath "$RUST_ROOT_DIR")"
    RUST_OUTPUT="${RUST_ROOT}/src/frb_generated.rs"
    ;;
esac

cd "$PKG_DIR"

# Resolve Dart/Flutter deps so codegen + build_runner can run.
flutter pub get

# FRB codegen reads flutter_rust_bridge.yaml in this dir for the remaining config
# (rust_input, rust_features, dart_output, add_mod_to_lib). We inject rust_root
# and rust_output via CLI flags so they stay machine-derived instead of committed
# as absolute paths. Emits lib/src/rust/* (Dart) and rewrites
# <core>/crates/stack_core/src/frb_generated.rs (Rust glue).
flutter_rust_bridge_codegen generate \
  --rust-root "$RUST_ROOT" \
  --rust-output "$RUST_OUTPUT"

# Workspace target-dir fix: FRB hardcodes the default-loader `ioDirectory` as
# `<rust_crate_dir>/target/release` (see compute_default_external_library_relative_directory).
# This crate lives in a cargo WORKSPACE, so its build output is the workspace
# root `target/release`, not `crates/stack_core/target/release`. Rewrite the
# generated path so the host/dev loader resolves the real dylib. (Desktop app
# bundles the lib next to its executable via CMake — see build/build-desktop.sh.)
sed -i.bak 's#crates/stack_core/target/release#target/release#' lib/src/rust/frb_generated.dart
rm -f lib/src/rust/frb_generated.dart.bak

# Emit the freezed classes (e.g. error.freezed.dart) the codegen output relies on.
dart run build_runner build --delete-conflicting-outputs

echo "Dart bindings written to ${PKG_DIR}/lib/src/rust"
ls -1 lib/src/rust
