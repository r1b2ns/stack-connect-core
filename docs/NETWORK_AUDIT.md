# Network audit

The core can hand the host a structured record of **every HTTP exchange it
performs**: App Store Connect API calls, the Apple ID web session (sign-in,
olympus, iris, Developer Portal), Google Play and Google OAuth. The host stores
and displays them (StackConnect's *Network Audit* module); the core keeps
nothing. This page covers the sink, the entry, the hints (operation, provider,
credential) and the operation names.

## Three sinks, three purposes

| | `NetworkAuditSink` (this page) | `CoreLogger` ([LOGGING.md](LOGGING.md)) | `DebugLogger` |
| --- | --- | --- | --- |
| Injected with | `audit_sink` of `connect`, `connect_with_access_token`, `connect_with_apple_session`, `AppleIdSignIn(…)` | `set_core_logger`, process-wide | `debug_logger` of the `connect*` functions |
| Content | one structured `NetworkAuditEntry` per exchange | one redacted text line per event | runnable cURL + pretty JSON |
| Secrets | **raw**: the host encrypts at rest and masks | never | verbatim (some redacted) |

They are independent: none feeds, changes or requires another.

## API

Rust (`crates/stack_core/src/ports.rs`, `audit/types.rs`, `facade.rs`):

```rust
#[uniffi::export(with_foreign)]
pub trait NetworkAuditSink: Send + Sync {
    fn record(&self, entry: NetworkAuditEntry);
    fn capture_bodies(&self) -> bool;
}

#[uniffi::export(default(audit_sink = None))]
pub fn connect(kind, account_id, store, debug_logger,
               audit_sink: Option<Arc<dyn NetworkAuditSink>>) -> Result<Arc<Provider>, StackError>;
// same trailing `audit_sink` (default None) on connect_with_access_token,
// connect_with_apple_session, AppleIdSignIn::new and AppleIdSignIn::with_saved_session
```

Swift (UniFFI):

```swift
public protocol NetworkAuditSink: AnyObject, Sendable {
    func record(entry: NetworkAuditEntry)
    func captureBodies() -> Bool
}

let provider = try connect(kind: .appStoreConnect, accountId: id, store: keychain,
                           debugLogger: nil, auditSink: auditStore)
let signIn = try AppleIdSignIn(auditSink: auditStore)
```

The argument defaults to `nil`, so existing callers compile unchanged and are
not audited. The sink is bound to the provider (and to every capability handle
the provider returns, e.g. `provider.reviews()`), so one sink per account is
the natural shape; one shared sink for every account works too (entries carry
`accountId`).

Both methods are called **synchronously** on the core's worker thread, once
per exchange (`captureBodies` right before the request, `record` after it):
implement them as a cheap hand-off (append to a queue that a background task
encrypts and persists). A panic in either is caught: the entry is dropped and
the request is unaffected.

The Flutter binding (`frb_api.rs`) does not expose the hook: its providers are
unaudited. Adding it there means an FRB-side buffer like the debug logger's
(FRB has no synchronous Dart callbacks).

## The entry

`NetworkAuditEntry` (Swift field names in parentheses):

| Field | Meaning |
| --- | --- |
| `id` | random UUID v4 |
| `started_at_ms` (`startedAtMs`) | Unix ms when the request was handed to the transport |
| `duration_ms` (`durationMs`) | until the response was read (or the failure) |
| `attempt` | `1`; a retry of the same call is `2` (Google Play retries once on a 401 with a service account) |
| `operation`, `parent_operation` (`parentOperation`) | why the call happened (see below) |
| `provider` | `appleApi`, `appleWebSession`, `googlePlay`, `googleOauth`, `firebase` (reserved), `other` |
| `account_id` (`accountId`) | the host's account id; `nil` during an Apple ID sign-in |
| `credential_kind` (`credentialKind`) | `ascJwt`, `googleAccessToken`, `appleSessionCookie`, `oauthAssertion`, `unauthenticated` |
| `token_expires_at_ms` (`tokenExpiresAtMs`) | the `exp` of a JWT credential (App Store Connect bearer, OAuth assertion), in Unix **ms**; `nil` for opaque tokens |
| `method`, `url` | the full URL, query included |
| `http_version` (`httpVersion`) | `HTTP/1.1`, `HTTP/2.0`; `nil` without a response |
| `request_headers` (`requestHeaders`) | ordered `NetworkAuditHeader { name, value }`, raw |
| `request_body`, `request_body_state` | see *Bodies* |
| `request_size_bytes` (`requestSizeBytes`) | request body bytes |
| `response_status` (`responseStatus`) | `nil` for a transport failure |
| `response_headers` (`responseHeaders`) | every header in wire order, each `Set-Cookie` separately |
| `response_body`, `response_body_state` | see *Bodies* |
| `response_size_bytes` (`responseSizeBytes`) | response body bytes the core read (after decompression where the core decompresses) |
| `error` | the transport or decoding failure (reqwest's text, as `StackError.Network` carries it) |

An HTTP error (4xx/5xx) is a completed exchange: status and body are set and
`error` is `nil`. A request the core rejects before sending (local validation)
produces no entry, because nothing went on the wire.

**Not in `request_headers`:** headers the HTTP stack adds while sending: `Host`,
`Content-Length`, the Apple session client's `User-Agent` and, for
`appleSessionCookie` requests, the `Cookie` header the session jar adds below
request building. The session's CSRF and sign-in headers are in the entry, and
every cookie Apple sets arrives as a `Set-Cookie` response header.

### Bodies

`NetworkAuditBodyState`:

| State | Body field | When |
| --- | --- | --- |
| `captured` | the whole text | |
| `empty` | `nil` | zero bytes |
| `truncated` | the first part | over 4 MiB, or the core stopped reading early (the Play Store page is read up to a cap) |
| `binary` | `nil` | an image/archive/octet-stream media type, or bytes that are not UTF-8 text |
| `notCaptured` | `nil` | `captureBodies()` returned `false` (sizes still reported) |
| `unavailable` | `nil` | a transport failure, a streamed request body, a response that failed to decode |

Response bodies are recorded **decompressed** wherever the core decompresses
them (the Apple ID session and the Developer Portal gzip some answers).

### Credential hints

Read from the request as sent, never from the authenticators:

- `Authorization: Bearer …` → `ascJwt` for App Store Connect, `googleAccessToken`
  for Google (a service account's exchanged token or the host's own Sign in
  with Google token). For a JWT, `tokenExpiresAtMs` is its `exp`: the host can
  decide whether a cURL export may include the live bearer.
- The OAuth token exchange → `oauthAssertion` with the assertion's `exp`. Its
  response body carries the new access token.
- Apple ID web session requests → `appleSessionCookie`.
- Pre-signed upload/download URLs, the `HEAD /logout` widget-key probe and the
  public Play Store page → `unauthenticated`.

## Operation names

`operation` is stable and names the core call that caused the exchange. The
exported calls are `<handle>.<method>`, the Rust names of the UniFFI objects
and methods (`Reviews.fetchCustomerReviews` in Swift is
`reviews.fetch_customer_reviews`); a provider-level call is `provider.<method>`.
Internal steps that run inside another operation get their own name, with the
surrounding operation in `parent_operation`:

| Operation | Caused by |
| --- | --- |
| `google.oauth.token` | a service account's token exchange (cache empty or expiring, or after a 401) |
| `apple_session.select_team` | the Apple ID session switching to the account's team before its first request |
| `apple_session.portal_team` | the Apple ID session resolving the Developer Portal team before the first provisioning call |

Provider, sync and sign-in:

| Handle | Operations |
| --- | --- |
| `provider` | `provider.validate`, `provider.fetch_apps` |
| `sync` | `sync.sync_apps` (its `provider.fetch_apps` has it as parent) |
| `apple_id` | `apple_id.start`, `apple_id.request_sms`, `apple_id.submit_code` |

Capability handles:

| Handle | Operations |
| --- | --- |
| `accessibility_declarations` | `accessibility_declarations.fetch_accessibility_declarations`, `accessibility_declarations.create_accessibility_declaration`, `accessibility_declarations.update_accessibility_declaration`, `accessibility_declarations.delete_accessibility_declaration` |
| `analytics` | `analytics.create_analytics_report_request`, `analytics.fetch_analytics_report_requests_page`, `analytics.fetch_analytics_reports_page`, `analytics.fetch_analytics_report_instances_page`, `analytics.fetch_analytics_report_segments_page`, `analytics.download_analytics_segment` |
| `app_details` | `app_details.fetch_app_details` |
| `app_icons` | `app_icons.fetch_icon_url` |
| `app_metadata` | `app_metadata.fetch_app_info_localizations`, `app_metadata.update_app_info_localization`, `app_metadata.update_app_info_localization_privacy`, `app_metadata.create_app_info_localization`, `app_metadata.delete_app_info_localization`, `app_metadata.fetch_app_info`, `app_metadata.fetch_app_categories`, `app_metadata.update_app_info_category`, `app_metadata.update_app`, `app_metadata.update_age_rating`, `app_metadata.fetch_icon_url` |
| `app_store_versions` | `app_store_versions.fetch_versions`, `app_store_versions.create_version`, `app_store_versions.update_version`, `app_store_versions.delete_version`, `app_store_versions.submit_for_review`, `app_store_versions.cancel_review`, `app_store_versions.release_version`, `app_store_versions.cancel_submission`, `app_store_versions.reject_version`, `app_store_versions.fetch_phased_release`, `app_store_versions.create_phased_release`, `app_store_versions.delete_phased_release`, `app_store_versions.update_phased_release_state`, `app_store_versions.fetch_localizations`, `app_store_versions.update_localization`, `app_store_versions.fetch_screenshot_sets`, `app_store_versions.create_screenshot_set`, `app_store_versions.upload_screenshot`, `app_store_versions.delete_screenshot`, `app_store_versions.delete_screenshot_set`, `app_store_versions.fetch_app_review_detail`, `app_store_versions.update_app_review_detail` |
| `beta_app_localizations` | `beta_app_localizations.fetch_beta_app_localizations`, `beta_app_localizations.create_beta_app_localization`, `beta_app_localizations.update_beta_app_localization` |
| `beta_app_review_detail` | `beta_app_review_detail.fetch_beta_app_review_detail`, `beta_app_review_detail.update_beta_app_review_detail` |
| `beta_build_localizations` | `beta_build_localizations.fetch_beta_build_localizations`, `beta_build_localizations.create_beta_build_localization`, `beta_build_localizations.update_beta_build_localization` |
| `beta_groups` | `beta_groups.fetch_beta_groups`, `beta_groups.fetch_beta_testers`, `beta_groups.create_beta_group`, `beta_groups.update_beta_group`, `beta_groups.delete_beta_group`, `beta_groups.add_beta_tester`, `beta_groups.remove_beta_tester`, `beta_groups.fetch_tester_count`, `beta_groups.resend_invite` |
| `builds` | `builds.fetch_builds`, `builds.fetch_builds_page`, `builds.fetch_builds_for_group`, `builds.fetch_build_detail`, `builds.fetch_current_build`, `builds.expire_build`, `builds.attach_build`, `builds.submit_build_for_beta_review`, `builds.add_build_to_groups`, `builds.remove_build_from_group` |
| `bundle_ids` | `bundle_ids.fetch_bundle_ids`, `bundle_ids.create_bundle_id`, `bundle_ids.update_bundle_id`, `bundle_ids.delete_bundle_id`, `bundle_ids.fetch_bundle_id_capabilities`, `bundle_ids.enable_capability`, `bundle_ids.disable_capability` |
| `certificates` | `certificates.fetch_certificates`, `certificates.fetch_certificate_content`, `certificates.create_certificate`, `certificates.revoke_certificate` |
| `devices` | `devices.fetch_devices`, `devices.create_device`, `devices.update_device` |
| `play_users` | `play_users.fetch_users`, `play_users.invite_user`, `play_users.update_user`, `play_users.delete_user`, `play_users.set_grant`, `play_users.delete_grant` |
| `profiles` | `profiles.fetch_profiles`, `profiles.create_profile`, `profiles.delete_profile`, `profiles.fetch_profile_content` |
| `reviews` | `reviews.fetch_customer_reviews`, `reviews.fetch_customer_reviews_page`, `reviews.fetch_review_submissions`, `reviews.reply_to_review`, `reviews.delete_review_response`, `reviews.submit_review_submission`, `reviews.discard_review_submission` |
| `store_listings` | `store_listings.fetch_store_listings` |
| `tracks` | `tracks.fetch_tracks`, `tracks.fetch_release_summaries`, `tracks.fetch_bundles`, `tracks.apply_release_change` |
| `users` | `users.fetch_team_members`, `users.fetch_users`, `users.invite_user`, `users.delete_user`, `users.update_user`, `users.fetch_user_visible_apps`, `users.update_user_visible_apps` |

`unknown` marks an exchange outside every operation (none exists today; a new
exported method must wrap its body in its handle's `audit.run("<handle>.<method>", …)`,
and a test checks that every name in the code is listed here).

## Overhead

Without a sink nothing is entered or captured: each exported call does one
task-local lookup. With a sink, the request headers and body are copied before
sending and up to 4 MiB of each response body is kept until the entry is
emitted.

## Implementation

- `audit/scope.rs`: the task-local `AuditScope` (sink, account, operation,
  attempt, overrides). `Provider` and every capability handle carry an
  `AuditBinding`; `Provider`'s accessors bind the handles they return.
- `audit/recorder.rs`: builds the entry at the single HTTP choke point
  (`http_debug::send`, `send_decoding`, `send_unbuffered`; every request in the
  crate goes through one of them), and emits it once the body is read (or the
  streamed response is dropped).
- `audit/credential.rs`: the credential hint and the JWT `exp` decoding.
