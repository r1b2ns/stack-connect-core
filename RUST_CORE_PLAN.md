# Plan: `stack_core` (Rust) + Swift/iOS binding (UniFFI)

> Shared Rust core that functions as a **multi-service hub**: each
> external service (App Store Connect, Firebase, Google Play today; AWS, GitHub and
> others in the future) enters as a **plugin** (`Provider`) that implements a
> common contract. Consumed natively by iOS via **UniFFI**
> (`StackCore.xcframework`).
> Historical companion: `../stack-connect/SHARED_CORE_PLAN.md` and `FLUTTER_PLAN.md`.
>
> **Central principle:** adding a new service = adding a `providers/<x>/` module
> (+ an authenticator, if needed) and registering it. **Nothing** in the core, the
> UniFFI facade, or the other providers changes.

## 1. Decided Assumptions

| Decision | Choice | Design Consequence |
|---|---|---|
| Bindings | **Swift/iOS via UniFFI** (done) + **Dart via flutter_rust_bridge** (active, for the Flutter Android+desktop apps) | Core stays **binding-agnostic** (thin facade per generator); FRB is a *separate* toolchain from UniFFI, added as a second facade behind a `frb` feature — see §11 |
| Architecture | **Multi-service via plugins** (`Provider` + `registry`) | FFI surface **stable** regardless of how many services exist; new service does not touch core |
| 1st service | **App Store Connect** | First complete plugin; validates the contract with a real service |
| Next | **Firebase, Google Play** (porting the Swift packages) → then **AWS, GitHub, …** | Enter as plugins reusing existing or new authenticators |
| Apple | Client *subset* (~31 endpoint families), not the giant SDK | Avoids porting the 2,411 generated files from `appstoreconnect-swift-sdk` |
| Persistence | **Native per platform via callback (`BlobStore`)** | Core *stateless*; iOS continues with SwiftData; lower-risk migration |

## 2. Target Architecture (Service Plugins)

```
            ┌──────────────── iOS app (native) ─────────────┐
            │ SwiftUI views · ViewModels (@Observable)       │
            └───────────────────────┬──────────────────────┘
                                    │ UniFFI (async/await, callbacks)
                ┌───────────────────┴───────────────────┐
                │  facade: available_services()          │
                │          credential_schema(kind)       │
                │          connect(kind, account, store) │  → Arc<dyn Provider>
                └───────────────────┬───────────────────┘
                                    │
                          ┌─────────┴──────────┐
                          │  service::registry  │  (kind → provider, schema)
                          └─────────┬──────────┘
        ┌──────────────┬───────────┼───────────┬───────────────┐
        ▼              ▼           ▼           ▼               ▼
  providers/appstore  firebase  googleplay   (aws)         (github)   ← plugins
        │              │           │           │               │
        └──────────────┴─────┬─────┴───────────┴───────────────┘
                             │ use
              ┌──────────────┴──────────────┐
              │ auth (ES256·OAuth-JWT·OAuth2·SigV4)  ·  http (reqwest)
              └──────────────┬──────────────┘
            HTTPS ▼                         ▲ callbacks (UniFFI)
   ┌──────────────────────┐     ┌───────────┴───────────────┐
   │ External APIs        │     │ CredentialStore→Keychain  │
   │ (Apple/Google/AWS/…) │     │ BlobStore→SwiftData       │
   └──────────────────────┘     └───────────────────────────┘
```

The UniFFI facade exposes **a uniform contract** (`Provider` + factory). Only the facade
knows the binding generator; `domain`/`service`/`providers`/`auth` never
import `uniffi`.

## 3. Service Contract (the Heart of Extensibility)

Types exported by UniFFI, stable regardless of how many services exist:

```rust
/// Which external service a connected account talks to. Grows with new plugins.
#[derive(uniffi::Enum)]
pub enum ServiceKind { AppStoreConnect, Firebase, GooglePlay /* future: Aws, GitHub, … */ }

/// Capability that a provider CAN expose. The UI calls `capabilities()` to know what's available.
#[derive(uniffi::Enum)]
pub enum Capability { Apps, Builds, Reviews, RemoteConfig, Messaging /* … */ }

/// A credential field that the service requires — drives the "connect account" form in the UI.
#[derive(uniffi::Record)]
pub struct CredentialField { pub key: String, pub label: String, pub secret: bool, pub multiline: bool }

/// Uniform contract that EVERY plugin implements (exported as a UniFFI interface).
#[uniffi::export(async_runtime = "tokio")]
pub trait Provider: Send + Sync {
    fn kind(&self) -> ServiceKind;
    fn capabilities(&self) -> Vec<Capability>;
    async fn validate(&self) -> Result<(), StackError>;
    /// Common capability; returns `StackError::Unsupported` if the service does not have it.
    async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError>;
}
```

Factory + discovery (also exported):

```rust
/// All services that the core can connect to today (drives a selector in the UI).
#[uniffi::export]
pub fn available_services() -> Vec<ServiceKind>;

/// The credential form that the UI should render for a service.
#[uniffi::export]
pub fn credential_schema(kind: ServiceKind) -> Vec<CredentialField>;

/// Reads secrets from the host (Keychain) and builds the provider for the `kind`.
#[uniffi::export(async_runtime = "tokio")]
pub fn connect(kind: ServiceKind, account_id: String, store: Arc<dyn CredentialStore>)
    -> Result<Arc<dyn Provider>, StackError>;
```

**Design decision — capabilities as uniform methods** (instead of different concrete types
per service crossing the FFI): the Swift surface is **a single one**
(`Provider`); what changes between services is *which* capabilities come in
`capabilities()`. Adding a **new service** = **zero** changes to the Swift API;
adding a **new capability** = one more method in `Provider`. (Alternative for
very rich APIs: sub-objects per capability, e.g. `fn reviews(&self) -> Option<Arc<dyn Reviews>>` —
adopt only if the capability becomes too large for a single method.)

## 4. How to Add a New Service (e.g. AWS, GitHub)

Without touching `domain`, `facade`, `service::provider` or the other plugins:

1. **`ServiceKind::Aws`** (one enum variant).
2. **Authenticator** in `auth/` if the scheme is new (e.g. `auth/sigv4.rs` for AWS,
   reuse of `auth/oauth2.rs` for GitHub). Behind the `Authenticator` trait.
3. **`providers/aws/`** implementing `Provider` (+ the `Capability` it offers).
4. **Register** in `service/registry.rs`: `build(kind)`, `credential_schema(kind)`
   and include in `available_services()`.

The facade already exposes the new service automatically (the UI sees it in `available_services()`
and requests the right secrets via `credential_schema()`).

## 5. Layers: What Migrates to Rust vs. What Stays Native

| Origin (Swift) | LOC | Destination |
|---|---|---|
| `appstoreconnect-swift-sdk` (actual app usage) | ~2,854 | → `providers/appstore` (**1st plugin**) + `auth::es256` |
| `Packages/APIProviderFirebase` | ~1,933 | → `providers/firebase` (plugin) + `auth::oauth_jwt` |
| `Packages/APIProviderPlay` | ~1,082 | → `providers/googleplay` (plugin) + `auth::oauth_jwt` |
| `Packages/StackProtocols` (`AccountConnectionProtocol`, `AppInfo`) | 47 | **dissolved** in core: `AppInfo`→`domain`; `AccountConnectionProtocol`→ `Provider` trait |
| `StackCore::PersistentStorable` | 31 | → `BlobStore` trait (UniFFI callback) |
| `StackCore::SwiftDataStorable` | 130 | **stays native** (implements the callback) |
| `StackCore::WidgetIconCache` / `Log` / `AppGroup` | ~104 | **stays native** (UIKit/WidgetKit/os.Logger) |
| ViewModels / Views / Coordinators / Keychain | — | **stays native** |

## 6. Verdict on `StackProtocols`

**Do not rewrite as a separate crate.** `AppInfo` becomes a `serde` `struct` in `domain/`
(exported by UniFFI). `AccountConnectionProtocol` (`validateCredentials`/`fetchApps`/
`disconnect`) is **generalized** in the `Provider` trait (multi-service). The ViewModels
start using the generated `Provider`.

## 7. Cargo Workspace Structure

```
stack-connect-core/
├── Cargo.toml                 # [workspace]
├── rust-toolchain.toml
├── crates/
│   └── stack_core/
│       ├── Cargo.toml         # auth schemes behind features (es256, oauth_jwt, oauth2, sigv4)
│       ├── uniffi.toml
│       ├── src/
│       │   ├── lib.rs         # uniffi::setup_scaffolding!; re-exports
│       │   ├── domain/        # AppInfo, ... (shared value types)
│       │   ├── ports/         # CredentialStore, BlobStore, Clock (host callbacks)
│       │   ├── error/         # StackError (+ Unsupported)
│       │   ├── http/          # typed reqwest client + pagination strategies
│       │   ├── auth/          # pluggable authenticators (trait Authenticator):
│       │   │                  #   es256 (Apple) · oauth_jwt (Google SA) · oauth2 (GitHub) · sigv4 (AWS)
│       │   ├── service/
│       │   │   ├── provider.rs   # trait Provider · Capability
│       │   │   ├── kind.rs       # ServiceKind · CredentialField
│       │   │   └── registry.rs   # build(kind) · credential_schema · available_services
│       │   ├── providers/        # one module per concrete service:
│       │   │   ├── appstore/     #   1st plugin (App Store Connect)
│       │   │   ├── firebase/     #   (later phase)
│       │   │   └── googleplay/   #   (later phase)   future: aws/ github/
│       │   ├── sync/          # Generic SyncService (per provider/capability) over BlobStore
│       │   └── facade/        # #[uniffi::export]: connect() · credential_schema() · available_services()
│       └── tests/             # wiremock + JSON fixtures
├── bindings/swift/            # generated scaffolding + binary Package.swift
├── build/                     # build-xcframework.sh · gen-swift.sh · swift-smoke.sh
└── .github/workflows/ci.yml
```

## 8. Callback Interfaces (Core ↔ Native)

UniFFI 0.31 recommends **foreign traits** (`with_foreign`, `Arc<dyn Trait>`) instead of
the old `callback_interface`:

```rust
#[uniffi::export(with_foreign)]
pub trait CredentialStore: Send + Sync {   // generic: works for any service
    fn secret(&self, account_id: String, key: String) -> Option<String>;
    fn set_secret(&self, account_id: String, key: String, value: String);
    fn delete(&self, account_id: String);
}

#[uniffi::export(with_foreign)]
pub trait BlobStore: Send + Sync {          // mirrors PersistentStorable
    fn save(&self, type_name: String, id: String, json: String);
    fn fetch(&self, type_name: String, id: String) -> Option<String>;
    fn fetch_all(&self, type_name: String) -> Vec<String>;
    fn delete(&self, type_name: String, id: String);
}
```

The **credential keys** are defined by each provider via `credential_schema`
(Apple: `issuerId`/`keyId`/`p8`; Google: `serviceAccountJson`; AWS: `accessKeyId`/
`secretAccessKey`/`region`; GitHub: `token`). The `CredentialStore` itself remains
generic (key→value per account). iOS: `KeychainStorable`→`CredentialStore`,
`SwiftDataStorable`→`BlobStore`.

## 9. App Store Connect — the *Subset* (1st Plugin)

`providers/appstore` covers the ~31 families used today (from `AppleAccountConnection.swift`):
`apps`, `appInfos`/`appInfoLocalizations`, `appStoreVersions` (+localizations/
phasedReleases/releaseRequests), `builds`, `betaGroups`/`betaTesters`/
`betaBuildLocalizations`, `customerReviews`, `reviewSubmissions`, `users`/
`userInvitations`, `bundleIds`/`certificates`/`devices`/`profiles`,
`analyticsReportRequests`/`analyticsReports`. Auth: `auth::es256` (`.p8`/P-256,
`aud=appstoreconnect-v1`, `exp ≤ 20min`). JSON:API pagination via `links.next`.

## 10. Dependencies (Crates)

```toml
reqwest      = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"
jsonwebtoken = "9"     # ES256 (.p8 Apple) and RS256 (Google SA), depending on the auth scheme
tokio        = { version = "1", features = ["rt", "rt-multi-thread", "sync", "macros", "time"] }
thiserror    = "2"
uniffi       = { version = "0.31", features = ["tokio"] }  # async → Swift; pure proc-macro
# future, behind features: aws-sigv4 (AWS), oauth2 (GitHub), ...

[features]                      # each auth/service scheme only enters the binary if compiled
default      = ["appstore"]
appstore     = []               # ES256
google       = []               # oauth_jwt (Firebase + Google Play)
# aws        = ["dep:aws-sigv4"]
# github     = []               # oauth2
```

The providers/authenticators are behind **cargo features** — the `.xcframework`
only loads what is enabled.

## 11. Phase-Based Roadmap

- **Phase 0 — Skeleton + proof of binding. ✅ COMPLETE.** Cargo workspace; UniFFI facade; `CredentialStore` callback (foreign trait); typed error `StackError`; `build-xcframework.sh`/`gen-swift.sh`/`swift-smoke.sh`; host Swift smoke + **XCTest on iOS simulator** crossing the boundary. fmt/clippy passing. Rust toolchain 1.96. *(Used a disposable example provider; to be replaced by the service contract.)*
- **Phase 1 — Service contract + 1st plugin (App Store Connect). ✅ COMPLETE.** `service::{provider, kind, registry}` (`Provider`/`ServiceKind`/`Capability`/`CredentialField`); `auth::es256`; `providers/appstore` with `validate` + `fetch_apps` (`GET /v1/apps`, `links.next` pagination); facade `connect`/`credential_schema`/`available_services`. Google sample removed. 17 Rust tests + host smoke + simulator XCTest passing. *Remains to plug into iOS app (strangler) — start of Phase 2.*
- **Phase 2 — Full ASC capabilities + sync.** The ~31 resources as capabilities; 403 error *pending agreements*; generic `SyncService` over `BlobStore`. Migrate the rest of `AppleAccountConnection`.
- **Phase 3 — Cleanup.** Remove legacy Swift packages + `appstoreconnect-swift-sdk` usage; keep native only `WidgetIconCache`/`Log`/`AppGroup`.

> **Out of scope (for now): Firebase / Google Play.** These providers are **not** being
> migrated to the core at this time — they remain implemented **natively in the iOS app**
> (`APIProviderFirebase`/`APIProviderPlay`). If they are ported later, each is just a
> `providers/<x>/` + registration reusing `auth::oauth_jwt` (see §4), without touching core or
> facade.
- **Dart binding (flutter_rust_bridge).** Second binding generator, alongside UniFFI, for the
  Flutter Android + desktop apps (`../stack-connect/FLUTTER_PLAN.md`). FRB is a *separate* toolchain
  from UniFFI (not a `uniffi.toml` backend): add an **FRB facade behind a `frb` cargo feature**
  (e.g. `src/frb_api.rs` / `bindings/dart/`), mirroring the UniFFI `facade.rs` / `bindings/swift`
  split — the core modules and the UniFFI facade stay untouched. Re-expose `available_services` /
  `credential_schema` / `connect` / `make_sync_service` + `Provider`/capability objects (opaque
  handles; `async fn` → Dart `Future`). Adapt the `with_foreign` callbacks (`CredentialStore` /
  `BlobStore` / `DebugLogger`) to Dart implementations. Build matrix: Android via `cargo-ndk`
  (`*-linux-android`) + desktop cdylib (`*-pc-windows-msvc`, `*-unknown-linux-gnu`); the
  `crate-type` already includes `cdylib`. Add `build/build-android.sh` + `build/build-desktop.sh`
  mirroring `build/build-xcframework.sh`. It serves **Apple-only** Flutter apps (Firebase/Play stay
  native in the iOS app — see the out-of-scope note above).
- **Future — New services** (AWS via `auth::sigv4`, GitHub via `auth::oauth2`, …): each is just a `providers/<x>/` + registration (see §4). Without touching core or facade.

## 12. Phase 0 — Definition of Done ✅

1. `cargo build` + `cargo clippy -D warnings` + `cargo fmt --check` passing.
2. UniFFI facade exports an `async` provider object + the `CredentialStore` callback.
3. `build/build-xcframework.sh` generates `StackCore.xcframework` (iOS device + sim) and
   `build/gen-swift.sh` generates Swift bindings.
4. Host Swift smoke + **XCTest on iOS simulator** validate the call crossing the
   boundary (typed error + Rust→Swift callback).
5. CI (`cargo test`/`clippy`/`fmt`) on GitHub Actions.

## 13. Tests & CI

- **Rust (bulk of coverage):** unit tests for each `providers/<x>` with `wiremock` +
  JSON fixtures (URL/method/headers, DTO→domain, pagination, errors); *golden tests*
  for authenticators (ES256, OAuth-JWT); `registry` (kind→provider, schema);
  `sync` with in-memory fake `BlobStore`.
- **Contract:** one test per provider ensuring `kind()`/`capabilities()`/`validate()`
  and that missing capabilities return `Unsupported`.
- **Binding (smoke):** XCTest crossing the UniFFI boundary (host + simulator).
- **CI:** `cargo fmt --check` + `clippy -D warnings` + `cargo test` (with provider
  feature matrix); `.xcframework` build; (later) iOS app build.

## 14. Risks & Mitigation

| Risk | Mitigation |
|---|---|
| Abstraction too generic (capabilities that don't fit every service) | Optional `Capability` + `Unsupported`; sub-objects per capability only when necessary |
| FFI surface stability as it grows | New service doesn't change Swift API; only new capability adds method (rare) |
| `async` Rust → Swift via UniFFI | Validated in Phase 0; `uniffi` feature `tokio` |
| JWT parity (ES256/RS256) | Golden tests against tokens that Swift SDKs generate today |
| Binary bloat with many services | Providers/auth behind cargo features; `.xcframework` only with enabled ones |
| FFI boundary at each `BlobStore.save` in sync | Acceptable (sync is not hot-loop); batch writes per entity |
