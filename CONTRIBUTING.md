# Contributing to stack-connect-core

Thanks for helping. This crate is the shared core of [StackConnect](https://github.com/r1b2ns/stack-connect): networking, authentication and the service providers, exposed to Swift (UniFFI) and Dart (flutter_rust_bridge). By taking part you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Report security issues privately as described in [SECURITY.md](SECURITY.md).

## Setup

- Install [rustup](https://rustup.rs). The toolchain (Rust 1.96.0, with rustfmt and clippy) is pinned in `rust-toolchain.toml` and installs itself on the first `cargo` command.
- Optional: [`just`](https://github.com/casey/just) for the recipes in the `justfile` (`just` lists them).
- Building the iOS xcframework (`./build/build-xcframework.sh`) needs macOS with Xcode.

```bash
git clone https://github.com/r1b2ns/stack-connect-core.git
cd stack-connect-core
cargo test --all-features
```

## Before opening a pull request

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

CI runs the same three commands, then builds the xcframework on macOS.

- **Tests are required.** The bulk of the auth, API, persistence and sync coverage lives in this crate; new behavior and bug fixes come with tests. HTTP is tested against local mock servers, never real Apple or Google endpoints.
- **No real secrets.** Test keys are generated fixtures (`crates/stack_core/tests/fixtures`); never commit real keys, tokens, cookies or account data.
- **Keep the exported surface deliberate.** Anything reachable from Swift or Dart is an API. Call out changes to `uniffi::` types, the facade (`facade.rs`) or the FRB facade (`frb_api.rs`) in the pull request.
- **Logging never leaks credentials.** Use the redacting `CoreLogger` targets; see [docs/LOGGING.md](docs/LOGGING.md).
- **Commit messages** follow [Conventional Commits](https://www.conventionalcommits.org/), as the history does: `feat(reviews): …`, `fix(frb): …`.

## Adding a service

Every external service is a `Provider` plugin registered in `service/registry.rs`; adding one touches neither the core nor the FFI facade. The contract and the step-by-step guide are in [RUST_CORE_PLAN.md](RUST_CORE_PLAN.md) (§3 and §4). Please open an issue to discuss a new provider before starting.

## Changing the Flutter (FRB) binding

The flutter_rust_bridge glue is behind the `frb` feature. If you edit `crates/stack_core/src/frb_api.rs` or any type it exposes, regenerate the bindings (`build/gen-dart.sh`) and commit `frb_generated.rs` in the same change; the Rust and Dart sides must stay in sync.

## Changing what the iOS app sees

The iOS app consumes a released xcframework plus the generated Swift wrapper. After a change to the UniFFI surface, the app is updated in a separate pull request in [stack-connect](https://github.com/r1b2ns/stack-connect) once a new core release is published (the procedure is in that repo's `Packages/StackCoreRust/Package.swift`).

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
