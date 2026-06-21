# stack-connect-core

Shared Rust core for [stack-connect](../stack-connect), consumed by:

- **iOS** natively via **UniFFI** (`StackCoreRust.xcframework`).
- **Flutter** (Android + desktop) via **flutter_rust_bridge** (behind the `frb` cargo
  feature).

See [RUST_CORE_PLAN.md](RUST_CORE_PLAN.md) for the full plan and roadmap.

## Status

**Plugin-based multi-service architecture.** The core is a hub: each external service
plugs in as a `Provider` implementing a common contract, registered in a `registry`.
Adding a service touches neither the core nor the FFI facade — see
[RUST_CORE_PLAN.md](RUST_CORE_PLAN.md) (§3 contract, §4 how to add a service).

**App Store Connect — fully migrated.** The `appstore` provider exposes 15 capabilities,
each as an `async` sub-object off `Provider`:

| | |
| --- | --- |
| `Apps` | list apps (`GET /v1/apps`, `links.next` pagination) |
| `Reviews` | customer reviews (paged) + reply/delete + review submissions |
| `AppStoreVersions` | versions read + create/update/delete |
| `Builds` | enriched read + page/group/detail/current + lifecycle/relationship writes |
| `BetaGroups` | groups + testers read/write, tester count, resend invite |
| `BetaBuildLocalizations` | per-build "What to Test" read/write |
| `BetaAppLocalizations` | app-level feedback email + description read/write |
| `BetaAppReviewDetail` | TestFlight "Test Information" (contact + demo account) |
| `AppMetadata` | app info / localizations |
| `AccessibilityDeclarations` | accessibility declarations |
| `Users` | team users + invitations |
| `Devices` · `BundleIds` · `Certificates` · `Profiles` | provisioning |

A generic `SyncService` drives persistence over the host's `BlobStore`. The typed
`StackError::PendingAgreements` surfaces App Store Connect 403 *pending agreements*
across every capability call. The iOS app runs on this core **only** — the legacy
`appstoreconnect-swift-sdk` has been removed.

**Out of scope (for now):** Firebase and Google Play stay implemented **natively in the
iOS app**; they are not (yet) ported to the core. When revisited, each is just a
`providers/<x>/` module + registration (plan §4).

## Layout

```
crates/stack_core/                    # the core crate
  src/
    auth/es256.rs                     # ES256 (P-256 / .p8) JWT for App Store Connect
    domain.rs                         # uniffi::Record value types (AppInfo, ...)
    error.rs                          # StackError (uniffi::Error)
    ports.rs                          # foreign traits: CredentialStore, BlobStore, DebugLogger
    facade.rs                         # available_services / credential_schema / connect / make_sync_service
    service/
      provider.rs                     # Provider + Capability (the uniform contract)
      kind.rs                         # ServiceKind / CredentialField
      registry.rs                     # ServiceKind -> concrete plugin
      sync.rs                         # SyncService over BlobStore
      capabilities/                   # one module per App Store Connect capability
    providers/appstore/               # the App Store Connect plugin (client + provider)
    frb_api.rs / frb_generated.rs     # flutter_rust_bridge facade (feature = "frb")
    bin/uniffi-bindgen.rs             # embedded bindgen, pinned to the runtime version
bindings/
  swift/                              # SwiftPM package consumable by the iOS app (binaryTarget)
    StackCoreRust.xcframework         # generated: iOS device + simulator
    Sources/ · Tests/ · smoke/        # generated bindings · XCTest (sim) · host smoke
  dart/stack_core_rust/               # Dart package consumable by the Flutter apps (path dep)
    lib/stack_core_rust.dart          # barrel re-exporting the generated FRB surface
    flutter_rust_bridge.yaml          # FRB codegen config
    lib/src/rust/                     # generated FRB bindings (gitignored)
build/
  build-xcframework.sh               # generates StackCoreRust.xcframework + Swift bindings
  gen-swift.sh                       # generates the Swift bindings (UniFFI library mode)
  gen-dart.sh                        # generates the Dart FRB bindings into bindings/dart
  build-android.sh                   # builds libstack_core.so (FRB) into the Flutter jniLibs
  swift-smoke.sh                     # builds + runs the cross-FFI smoke on the host
```

Both generated binding artifacts (the Swift wrapper/xcframework and the Dart
`lib/src/rust/`) are **gitignored** — regenerated locally from the core crate.

## Development

Rust toolchain **1.96.0** (pinned in `rust-toolchain.toml`). Recipes via [`just`](https://github.com/casey/just):

```bash
just                       # list recipes
just test                  # cargo test -p stack_core (240 lib + 3 facade tests)
just build-xcframework     # Swift bindings + xcframework (macOS)
just gen-dart              # Dart FRB bindings into bindings/dart (regenerates frb_generated.rs)
just build-android         # libstack_core.so (FRB) into the Flutter jniLibs
just clean                 # remove target/ + generated bindings + xcframework

cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

The `frb` feature is **off by default**, so the iOS staticlib and the default
`cargo test` are unaffected by the Flutter binding. UniFFI is pure proc-macro
(`uniffi::setup_scaffolding!()`); `uniffi-bindgen` is embedded as a crate bin, pinned to
the same version as the runtime.
