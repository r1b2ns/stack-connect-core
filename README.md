# stack-connect-core

Shared Rust core for [stack-connect](../stack-connect), consumed natively by iOS
via UniFFI (`StackCore.xcframework`). See [RUST_CORE_PLAN.md](RUST_CORE_PLAN.md)
for the full plan and roadmap.

## Status

**Plugin-based multi-service architecture.** The core is a hub: each external
service (App Store Connect, Firebase, Google today; AWS, GitHub, … in the future)
plugs in as a `Provider` implementing a common contract, registered in a
`registry`. Adding a service touches neither the core nor the UniFFI facade — see
[RUST_CORE_PLAN.md](RUST_CORE_PLAN.md) (§3 contract, §4 how to add a service).

**Phase 0 — skeleton + binding proof ✅.** Cargo workspace, UniFFI facade,
`CredentialStore` callback, `StackError`, xcframework (iOS device/sim) + smoke/XCTest.

**Phase 1 — service contract + 1st plugin (App Store Connect) ✅.** `service::{provider,
kind, registry}` (`Provider`/`ServiceKind`/`Capability`/`CredentialField`), `auth::es256`
(`.p8`/P-256), `providers/appstore` (`validate` + `fetch_apps` via `GET /v1/apps`,
`links.next` pagination), facade `connect()`/`credential_schema()`/`available_services()`.
Google sample removed. **17 Rust tests** + host smoke + XCTest on the iOS simulator — green.

Next: **Phase 2** — plug App Store Connect into the iOS app (strangler) + full ASC
capabilities (~31 resources) + `SyncService` over `BlobStore`.

## Layout

```
crates/stack_core/      # the core crate
  src/
    api/play.rs         # Play Developer Reporting client (apps:search)
    auth/               # service account · JWT RS256 · OAuth2 + cache
    domain.rs           # AppInfo (uniffi::Record)
    error.rs            # StackError (uniffi::Error)
    ports.rs            # CredentialStore (foreign trait)
    facade.rs           # PlayProvider (uniffi::Object)
    bin/uniffi-bindgen.rs
  tests/                # smoke (public API) + RSA key fixtures
bindings/swift/         # SwiftPM package consumable by the app (binaryTarget)
  Package.swift         # StackCore.xcframework + generated StackCore.swift
  smoke/main.swift      # host smoke (cross-FFI)
  Tests/                # XCTest (iOS simulator)
build/
  gen-swift.sh          # generates the Swift bindings (UniFFI library mode)
  build-xcframework.sh  # generates StackCore.xcframework (iOS device + sim)
  swift-smoke.sh        # builds + runs the cross-FFI smoke on the host
```

## Development

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test

# Swift bindings + xcframework (macOS):
./build/build-xcframework.sh

# Cross-FFI smoke on the host (Swift → Rust → callback → error):
./build/swift-smoke.sh
```

UniFFI is pure proc-macro (`uniffi::setup_scaffolding!()`); `uniffi-bindgen` is
embedded as a crate bin, pinned to the same version as the runtime.
