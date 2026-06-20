# stack_core task runner. Run `just` to list recipes.

# Show available recipes (default).
default:
    @just --list

# Build the workspace (debug). Pass `--release` etc. via ARGS, e.g. `just build --release`.
build *ARGS:
    cargo build {{ARGS}}

# Build the StackCoreRust.xcframework + Swift bindings for iOS (device + simulator).
build-xcframework:
    ./build/build-xcframework.sh

# Build libstack_core.so (FRB) for Android and copy into the stack_mobile jniLibs.
build-android:
    ./build/build-android.sh

# Run all tests (lib + facade smoke).
test *ARGS:
    cargo test -p stack_core {{ARGS}}

# Remove build artifacts: cargo target/ plus generated Swift bindings and the xcframework.
clean:
    cargo clean
    rm -rf build/generated bindings/swift/StackCoreRust.xcframework
