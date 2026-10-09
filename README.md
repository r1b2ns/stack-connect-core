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

**App Store Connect — fully migrated.** The `appstore` provider exposes 17 capabilities,
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
| `Analytics` | analytics report requests (create + list), reports, instances and segments (download, gunzip, TSV parse) |
| `AppIcons` | `fetch_icon_url(app_id)`: the latest build's `iconAssetToken` as a URL (the same lookup as `AppMetadata.fetch_icon_url`, which stays for existing callers); `None` without a build |

A generic `SyncService` drives persistence over the host's `BlobStore`. The typed
`StackError::PendingAgreements` surfaces App Store Connect 403 *pending agreements*
across every capability call. The iOS app runs on this core **only** — the legacy
`appstoreconnect-swift-sdk` has been removed.

**Google Play — apps, reviews, read-only app detail and icons in the core.** The
`googleplay` provider (`ServiceKind::GooglePlay`) exposes `Apps`, `Reviews`, `AppDetails`,
`StoreListings`, `Tracks` and `AppIcons`; every App Store Connect-only accessor returns
`None`. Everything is read-only except review replies, and no edit is ever committed.

| | |
| --- | --- |
| Credentials | `clientEmail` / `privateKeyId` / `privateKey`, which the host extracts from the service account's JSON key file. The key is parsed eagerly at `connect` (PKCS#8 or PKCS#1, raw or JSON-escaped, RSA ≥ 2048-bit), so a bad key fails there with `InvalidCredentials`. |
| Auth | `auth::oauth_jwt`: an RS256 JWT-bearer assertion (RFC 7523) exchanged at `https://oauth2.googleapis.com/token` for the `androidpublisher` + `playdeveloperreporting` scopes; the access token is cached with a 60 s refresh margin and dropped on a 401 (the call is then retried once). |
| `Apps` | `GET https://playdeveloperreporting.googleapis.com/v1beta1/apps:search` with `pageToken` pagination (the Android Publisher API has no list-apps endpoint). `AppInfo.id` = `bundleId` = the package name; `platform` = `"ANDROID"`. |
| `validate()` | Token exchange, then `apps:search?pageSize=1`. `Ok` means the key works and the Reporting API is enabled, not that any app is visible yet, nor that the Android Publisher API is enabled. |
| `Reviews` | The shared `Reviews` handle, on `androidpublisher/v3/applications/{package}/reviews`. `fetch_customer_reviews` follows `tokenPagination.nextPageToken` (`maxResults=100`, at most 100 pages). `fetch_customer_reviews_page`: Play has no server-side sort or filter, so `sort` must be `""` or `-createdDate` (Google's own order, most recent first; anything else is `Unsupported`), `filter_rating` is applied to each fetched page (a page can come back short or empty while `next_token` is still set), and `limit` is clamped to 1–100. `reply_to_review` posts `reviews/{id}:reply`; the text is sent as is and Google's own error comes back if it is too long. `delete_review_response` and the three review-submission methods are `Unsupported` (Play has no equivalent). |
| `AppDetails` | `fetch_app_details(app_id)` → `AppDetailsInfo` (default language, contact email/phone/website) from `edits.details`. Doubles as the "can this service account reach package X?" check: `Ok` = yes, `Http 404` = no such package, `Auth` = no access or API disabled. |
| `StoreListings` | `fetch_store_listings(app_id)` → `[StoreListingInfo]` (language, title, short/full description, video) from `edits.listings`. |
| `Tracks` | `fetch_tracks(app_id)` → `[TrackInfo]` (track name + releases: name, status, version codes as decimal strings, user fraction, release notes, in-app update priority) from `edits.tracks`; tracks without a release are included. |
| `AppIcons` | `fetch_icon_url(app_id)` → the app icon at 512 px, read from the **public** store page `GET https://play.google.com/store/apps/details?id={package}&hl=en` with no credentials (no token is minted, no `androidpublisher` call, no edit). See *App icons* below. |

*Reviews, as mapped onto `CustomerReview`:* `id` is an **opaque composite**,
`{packageName}/{reviewId}` (the reply endpoint needs the package, and the store-agnostic
`reply_to_review` only receives the review id). Hosts must pass it back unchanged and
never parse it; an id the core did not produce is rejected with `Decode` before any
request. `rating` = `starRating`; Play's `text` (`title<TAB>body` when the review has a
title) is split into `title`/`body`; `reviewer_nickname` = `authorName`; `created_date` is
the latest user comment's `lastModified` (Play has no creation date) as
`YYYY-MM-DDTHH:MM:SSZ`; `territory` is `None`; the developer reply becomes `response`
(same id as the review, `state` = `None`). App version, device and reviewer language are
not mapped yet.

*Google's review API limits* ([reply-to-reviews](https://developers.google.com/android-publisher/reply-to-reviews)):
it only returns reviews users **created or modified within the last week** (older ones
are only in the Play Console CSV export), only reviews **with a comment**, and only for
**production** versions of the app. Quotas: 200 `GET`s per hour, 2000 replies per day.
Replies of more than about 350 characters are rejected, and HTML is stripped.

*App icons (`AppIcons`):* the Android Publisher API only exposes an app's images through
`edits.images.list`, which needs an edit, and opening an edit invalidates any other edit
of the same service account and app (see *Edit sessions* below). Icons are wanted for
every app in a list, so they never go through edits: the core reads the public store
page instead and takes its `og:image` meta tag (attribute order, quoting and HTML
entities such as `&amp;` are tolerated). Only an `https://` URL on a
`*.googleusercontent.com` host (Play uses `play-lh.googleusercontent.com`) is accepted;
the image-server options after the last `=` (e.g. `=s0-br30`) are replaced with `=s512`,
the size the App Store icon defaults to. Results: a published app → `Some(url)`; `404`
(unpublished, unknown, or not visible in the store) → `None`; a `200` page without a
usable `og:image` (e.g. a consent page, or a foreign image host) → `None` plus a debug
log line; an invalid package name → `None` without a request; any other non-2xx →
`Http`; transport failure → `Network`. At most 8 MiB of the page is read (a page is about
1.3 MB, with the tag past the first megabyte), and the page body is never written to the
debug log, only its size. Store visibility, not Play Console access, decides the result,
so an app the service account cannot see may still have an icon, and an app that is not
live in the store (never published, or only on internal/closed testing tracks) has none.

*Edit sessions (`AppDetails`, `StoreListings`, `Tracks`):* the Android Publisher API only
serves these inside an edit, so each call inserts an edit, reads, and **always** deletes
the edit, even when the read fails. A failed delete is traced and otherwise ignored,
and never replaces the read's result. **Side effect:** Google keeps one open edit per
service account and app, and inserting one invalidates the previous one. A read here
therefore breaks an edit that the same service account has open elsewhere, e.g. a CI
upload in progress. Use a separate (read-only) service account for this app if CI
publishes with one. Within one connected provider, edit sessions are serialized so that
concurrent reads cannot invalidate each other.

*Google-side setup:*
1. In the service account's Google Cloud project, enable the **Google Play Developer
   Reporting API** (apps) **and** the **Google Play Android Developer API** (reviews,
   details, listings, tracks).
2. Invite the service-account email in Play Console › Users and permissions:
   - "View app information (read-only)" for `Apps`, reading `Reviews`, and
     `AppDetails` / `StoreListings` / `Tracks`;
   - "Reply to reviews" for `reply_to_review`.

   `AppIcons` needs neither an API nor a permission: it reads the public store page.

   Google does not document which permission `edits.insert` checks for a read-only use.
   If the edit-based reads are denied with view-only access, the error is an `Auth`
   that names the app and the service account. Access can take hours to propagate.

*Errors* map onto the existing `StackError` variants (no new variant). A disabled API
(`SERVICE_DISABLED`, named with its activation URL) or missing access (403, or the 401
`permissionDenied` the Android Publisher API sends) is `Auth` with an actionable
message that names the app and the permission. A package Google does not know is
`Http 404` ("Google Play found no app with package name …"). A package name that breaks
Android's naming rules gets the same `Http 404`, decided locally without a request.
Quota and server errors are `Http`. The iOS app connects Google Play accounts and lists
apps through the core; the Flutter apps only label the service so far, and the new
capabilities (`AppDetails`, `StoreListings`, `Tracks`, `AppIcons`) have no FRB wrappers
yet (Play reviews already work through `FrbReviews`; `Capability` itself crosses FRB).

**Design as built.** Each provider owns a concrete authenticator (`auth::es256` for App
Store Connect, `auth::oauth_jwt` for Google) and shares `http_debug` for sending and
tracing requests; there is no common `Authenticator` trait. Every provider is always
compiled in: the only cargo features are `frb` (Dart binding) and `cli` (embedded
`uniffi-bindgen`).

**Out of scope (for now):** Firebase stays implemented **natively in the iOS app**. When
revisited, it is just a `providers/<x>/` module + registration (plan §4), reusing
`auth::oauth_jwt`.

## Layout

```
crates/stack_core/                    # the core crate
  src/
    auth/es256.rs                     # ES256 (P-256 / .p8) JWT for App Store Connect
    auth/oauth_jwt.rs                 # Google service-account RS256 JWT-bearer -> access token
    auth/token.rs                     # token cache primitives shared by the authenticators
    http_debug.rs                     # shared HTTP send + debug tracing (runnable cURL; capped, body-less variant for HTML pages)
    domain.rs                         # uniffi::Record value types (AppInfo, ...)
    error.rs                          # StackError (uniffi::Error)
    ports.rs                          # foreign traits: CredentialStore, BlobStore, DebugLogger
    facade.rs                         # available_services / credential_schema / connect / make_sync_service
    service/
      provider.rs                     # Provider + Capability (the uniform contract)
      kind.rs                         # ServiceKind / CredentialField
      registry.rs                     # ServiceKind -> concrete plugin
      sync.rs                         # SyncService over BlobStore
      capabilities/                   # one module per capability (sub-object off Provider)
    providers/appstore/               # the App Store Connect plugin (client + provider)
    providers/googleplay/             # the Google Play plugin: client (transport + Reporting API),
                                      #   reviews, edits (edit-session reads), store_page (icons from the
                                      #   public store page), error, timestamp, provider
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
just test                  # cargo test -p stack_core (516 lib + 9 facade tests)
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
