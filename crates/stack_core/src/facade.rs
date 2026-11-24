//! UniFFI-exported facade: the entire public surface Swift sees. Everything else
//! in the crate is internal. The facade owns binding concerns so `service`,
//! `providers`, and `auth` stay binding-agnostic.

use std::sync::Arc;

use crate::audit::{AuditTarget, NetworkAuditProvider};
use crate::error::StackError;
use crate::logging::{self, LogLevel};
use crate::ports::{
    AccessTokenProvider, BlobStore, CoreLogger, CredentialStore, DebugLogger, NetworkAuditSink,
    SessionStore,
};
use crate::service::kind::{CredentialField, ServiceKind};
use crate::service::provider::Provider;
use crate::service::registry;
use crate::service::sync::SyncService;

/// Every service the core can connect today. Drives the host's service picker.
#[uniffi::export]
pub fn available_services() -> Vec<ServiceKind> {
    registry::available_services()
}

/// The credential form the host should render to connect an account of `kind`.
#[uniffi::export]
pub fn credential_schema(kind: ServiceKind) -> Vec<CredentialField> {
    registry::credential_schema(kind)
}

/// Reads the secrets for `(kind, account_id)` from the host `store` and builds a
/// connected [`Provider`].
///
/// Synchronous on purpose: it only reads secrets through the (synchronous)
/// callback and builds the provider — no network. The returned provider does
/// the async work (`validate`, `fetch_apps`).
///
/// Key handling differs per service. App Store Connect keeps its `.p8` as-is
/// and parses it on the first request. Google Play parses its private key here,
/// including one local test signature, so a blank field or an unusable key is
/// rejected at connect rather than on the first request.
///
/// When `debug_logger` is `Some`, every HTTP request the provider issues
/// (App Store Connect calls; for Google Play, the OAuth token exchange and every
/// API call) is logged as a runnable cURL (with pretty-printed JSON body) and
/// its response (status + pretty-printed JSON). The host passes `None` in
/// release builds and a native logger only when its debug launch flag is set.
///
/// When `audit_sink` is `Some`, every HTTP exchange of the provider and of
/// the capability handles it returns is handed to it as a structured, raw
/// [`crate::audit::NetworkAuditEntry`] (see [`NetworkAuditSink`]), tagged
/// with `account_id`. Independent of `debug_logger`. Defaults to `None`.
///
/// # Errors
/// [`StackError::InvalidCredentials`] if a required secret is missing, or, for
/// Google Play, if a field is blank or the private key is not a usable RSA key.
#[uniffi::export(default(audit_sink = None))]
pub fn connect(
    kind: ServiceKind,
    account_id: String,
    store: Arc<dyn CredentialStore>,
    debug_logger: Option<Arc<dyn DebugLogger>>,
    audit_sink: Option<Arc<dyn NetworkAuditSink>>,
) -> Result<Arc<Provider>, StackError> {
    let inner = registry::build(kind, &account_id, &store, debug_logger)?;
    let provider = match kind {
        ServiceKind::AppStoreConnect => NetworkAuditProvider::AppleApi,
        ServiceKind::GooglePlay => NetworkAuditProvider::GooglePlay,
    };
    let audit = AuditTarget::new(audit_sink, Some(&account_id), provider);
    Ok(Provider::audited(inner, audit))
}

/// Builds a connected [`Provider`] for an account added with the user's own
/// OAuth sign-in (Google Play: "Sign in with Google"), authorized with the
/// access token `token_provider` returns for `account_id` instead of the
/// service-account secrets [`connect`] reads.
///
/// The core asks `token_provider` before every request and never caches,
/// stores or refreshes the token; the host owns the refresh token. The token
/// must carry the service's scopes (Google Play: `androidpublisher` and
/// `playdeveloperreporting`). The returned provider exposes the same
/// capabilities as one built by [`connect`].
///
/// Errors on this path: a `token_provider` failure surfaces as
/// [`StackError::Auth`] (a host `Auth` keeps its message verbatim; a host
/// `Network` passes through unchanged), and an HTTP 401 from Google is
/// [`StackError::Auth`] without a retry. Quota / rate limits keep the same
/// [`StackError::Http`] `429` shape as the service-account path.
///
/// Synchronous and offline, like [`connect`]. When `debug_logger` is `Some`,
/// requests are traced like [`connect`]'s, except that the `Authorization`
/// header is redacted. `audit_sink` is as in [`connect`]; its entries carry
/// the host's token verbatim (the sink is the host's own).
///
/// # Errors
/// [`StackError::Unsupported`] for a service without this auth path (App
/// Store Connect); [`StackError::InvalidCredentials`] for a blank `account_id`.
#[uniffi::export(default(audit_sink = None))]
pub fn connect_with_access_token(
    kind: ServiceKind,
    account_id: String,
    token_provider: Arc<dyn AccessTokenProvider>,
    debug_logger: Option<Arc<dyn DebugLogger>>,
    audit_sink: Option<Arc<dyn NetworkAuditSink>>,
) -> Result<Arc<Provider>, StackError> {
    let inner = registry::build_with_access_token(kind, &account_id, token_provider, debug_logger)?;
    let audit = AuditTarget::new(
        audit_sink,
        Some(&account_id),
        NetworkAuditProvider::GooglePlay,
    );
    Ok(Provider::audited(inner, audit))
}

/// Builds a connected App Store Connect [`Provider`] for an account added with
/// "Sign in with Apple ID", authorized with the web session `session_store`
/// holds for `account_id` (the blob [`crate::apple_id::AppleIdSignIn::finish`]
/// returned) instead of an API key.
///
/// `team_id` is the account's App Store Connect team (an
/// [`crate::auth::apple_session::session::AppleTeam::provider_id`]); the
/// session is switched to it before the first request when it acts for
/// another team. Requests go to App Store Connect's web API
/// (`appstoreconnect.apple.com/iris/v1`). After every response that refreshes
/// cookies the core saves the session back through `session_store`.
///
/// Provisioning (devices, bundle ids, certificates, profiles) goes to the
/// Apple Developer Portal (`developer.apple.com/services-account`) with the
/// account's *Developer Portal* team, which the core resolves from the Apple
/// ID's portal teams on the first provisioning call (matched to the App Store
/// Connect team by name) and saves with the session; a team it cannot match
/// unambiguously makes provisioning calls fail with
/// [`StackError::Unsupported`]. Renaming or deleting a bundle id and
/// Analytics Reports are not reachable with a session: Analytics Reports are
/// left out of `capabilities()`, and those calls fail with
/// [`StackError::Unsupported`] without a request.
///
/// Errors on this path: an HTTP 401 from App Store Connect, or a session it
/// refuses while switching the team, is [`StackError::SessionExpired`]: the
/// host must ask the user to sign in again (the core has no password to retry
/// with).
///
/// Synchronous and offline, like [`connect`]. When `debug_logger` is `Some`,
/// requests are traced like [`connect`]'s, with the session's cookies, CSRF
/// tokens and sign-in headers redacted. `audit_sink` is as in [`connect`]
/// (provider `AppleWebSession`; the jar's cookies are not in the entries'
/// request headers, the CSRF and session headers and every `Set-Cookie` are).
///
/// # Errors
/// [`StackError::InvalidCredentials`] for a blank `account_id` or when the
/// store has no session for it; [`StackError::Decode`] for a blob the core
/// cannot read.
#[uniffi::export(default(audit_sink = None))]
pub fn connect_with_apple_session(
    account_id: String,
    team_id: u64,
    session_store: Arc<dyn SessionStore>,
    debug_logger: Option<Arc<dyn DebugLogger>>,
    audit_sink: Option<Arc<dyn NetworkAuditSink>>,
) -> Result<Arc<Provider>, StackError> {
    let inner =
        registry::build_with_apple_session(&account_id, team_id, session_store, debug_logger)?;
    let audit = AuditTarget::new(
        audit_sink,
        Some(&account_id),
        NetworkAuditProvider::AppleWebSession,
    );
    Ok(Provider::audited(inner, audit))
}

/// Builds a [`SyncService`] that syncs `provider` into the host `store` for
/// `account_id`. The account id is stamped into every persisted app blob
/// (`accountId`) so the host can derive its composite key and attribute apps.
///
/// Synchronous on purpose: it only wires the handles together. The returned
/// object does the async work (`sync_apps`).
#[uniffi::export]
pub fn make_sync_service(
    provider: Arc<Provider>,
    store: Arc<dyn BlobStore>,
    account_id: String,
) -> Arc<SyncService> {
    SyncService::new(provider, store, account_id)
}

/// Installs the process-wide diagnostic log sink (`Some`) or removes it
/// (`None`), receiving `min_level` and every more severe level.
///
/// Independent of `connect`'s `debug_logger`: install it once at startup and
/// it covers every provider, including those connected earlier. Messages are
/// redacted before they reach `logger` (see [`CoreLogger`]). Without a sink the
/// core formats nothing. Call again to change the level.
#[uniffi::export]
pub fn set_core_logger(logger: Option<Arc<dyn CoreLogger>>, min_level: LogLevel) {
    logging::install(logger, min_level);
}
