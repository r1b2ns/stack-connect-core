//! flutter_rust_bridge (FRB) facade: the public surface the Dart binding sees.
//!
//! This is a *second* binding alongside the UniFFI/Swift facade in
//! [`crate::facade`]. It is compiled only under the `frb` cargo feature
//! (default OFF) so the iOS staticlib and the default `cargo test` are
//! unaffected.
//!
//! Like the UniFFI facade, it owns its binding concerns: the core (`service`,
//! `providers`, `auth`) stays binding-agnostic. This facade MIRRORS the UniFFI
//! surface ([`crate::facade`]) in an FRB-idiomatic shape:
//!
//! - the core's `uniffi::Object`s (`Provider`, `Reviews`, `SyncService`) are
//!   re-exposed as thin opaque FRB wrappers ([`FrbProvider`], [`FrbReviews`],
//!   [`FrbSyncService`]) whose `async` methods delegate to the inner async core
//!   methods and cross to Dart as `Future`s on FRB's own executor;
//! - the core's three foreign-trait ports ([`crate::ports::CredentialStore`],
//!   [`crate::ports::BlobStore`], [`crate::ports::DebugLogger`]) are SYNC
//!   traits exported to UniFFI via `with_foreign`. FRB v2 cannot reuse that
//!   UniFFI foreign-trait export, and its only Dart-callback mechanism
//!   (`DartFnFuture`) is inherently *async*. Driving an async Dart callback
//!   from inside a sync port method would mean blocking on the executor — a
//!   deadlock risk. So this facade ADAPTS the ports without ever calling Dart
//!   from a sync context:
//!     * credentials are passed in as plain data ([`FrbCredential`]) and wrapped
//!       in an in-memory [`CredentialStore`]; `connect` only reads secrets
//!       synchronously (no network), so this is behaviourally identical to the
//!       UniFFI path;
//!     * the debug logger is an optional Dart async closure, adapted into a
//!       [`DebugLogger`] that *buffers* every formatted line; the buffer is
//!       drained to Dart after each async provider call (logging is
//!       fire-and-forget, so order across calls is preserved without ever
//!       blocking the sync `log`);
//!     * the blob store is the real core [`crate::service::sync::SyncService`]
//!       wired to a buffering [`BlobStore`]; `sync_apps` runs the real core
//!       sync (producing the exact iOS-facing camelCase blob JSON), then the
//!       buffered `(typeName,id,json)` saves are handed to a Dart async persist
//!       callback at the FRB layer.
//!
//! FRB mirrors the core records/enums it sees in signatures into matching Dart
//! types automatically (the `uniffi::*` derives they carry do not interfere with
//! FRB codegen), so [`crate::service::kind::ServiceKind`],
//! [`crate::service::provider::Capability`],
//! [`crate::service::kind::CredentialField`], [`crate::domain::AppInfo`],
//! [`crate::domain::CustomerReview`], [`crate::domain::ReviewResponse`],
//! [`crate::domain::CustomerReviewsPage`] and [`crate::error::StackError`] all
//! cross as proper Dart types without any mirror types defined here.

use std::sync::{Arc, Mutex};

use flutter_rust_bridge::DartFnFuture;

use crate::domain::{AppInfo, CustomerReview, CustomerReviewsPage, ReviewResponse};
use crate::error::StackError;
use crate::ports::{BlobStore, CredentialStore, DebugLogger};
use crate::service::kind::{CredentialField, ServiceKind};
use crate::service::provider::{Capability, Provider};
use crate::service::registry;
use crate::service::sync::SyncService;

// ---------------------------------------------------------------------------
// Free functions (mirror `crate::facade`)
// ---------------------------------------------------------------------------

/// Every service the core can connect today, for the Dart host's service picker.
///
/// Calls the same real core logic as the UniFFI facade
/// ([`crate::facade::available_services`]) — `registry::available_services()` —
/// and hands the result to the FRB binding. FRB mirrors [`ServiceKind`] into a
/// matching Dart enum, so the Dart side receives the real value from real core
/// code.
///
/// # Examples
///
/// ```
/// # use stack_core::frb_api::available_services;
/// # use stack_core::ServiceKind;
/// assert_eq!(available_services(), vec![ServiceKind::AppStoreConnect]);
/// ```
#[flutter_rust_bridge::frb(sync)]
pub fn available_services() -> Vec<ServiceKind> {
    registry::available_services()
}

/// The credential form the host should render to connect an account of `kind`.
///
/// Mirrors [`crate::facade::credential_schema`]. Synchronous: it only inspects
/// the static schema. FRB mirrors [`CredentialField`] into a Dart class.
#[flutter_rust_bridge::frb(sync)]
pub fn credential_schema(kind: ServiceKind) -> Vec<CredentialField> {
    registry::credential_schema(kind)
}

/// A single resolved credential the Dart host hands to [`connect`].
///
/// The UniFFI facade reads secrets through the host's (sync) `CredentialStore`
/// foreign trait. FRB cannot reuse that export and its only Dart-callback
/// mechanism is async, so the Dart host instead reads its own secure storage
/// and passes the already-resolved `(key, value)` pairs here as plain data. The
/// FRB facade wraps them in an in-memory [`CredentialStore`] and runs the exact
/// same `registry::build` the UniFFI `connect` runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrbCredential {
    /// The schema key (e.g. `issuerId`), matching a [`CredentialField::key`].
    pub key: String,
    /// The secret value for that key.
    pub value: String,
}

/// Reads the secrets the Dart host supplies and builds a connected provider.
///
/// Mirror of [`crate::facade::connect`]. Synchronous on purpose: it only wraps
/// the supplied secrets and parses the key material — no network. The returned
/// [`FrbProvider`] does the async work (`validate`, `fetch_apps`).
///
/// `debug_logger` is an async Dart closure invoked once per already-formatted
/// HTTP trace line (runnable cURL request + response). It is gated by
/// `debug_logging`: only when `debug_logging` is `true` is it wired into the
/// core's (sync) [`DebugLogger`] (via a buffer drained to Dart after each async
/// call, so the sync `log` never blocks on the Dart executor). When
/// `debug_logging` is `false` the closure is never called and the core never
/// logs — mirroring the UniFFI `debug_logger: None` release path.
///
/// (The flag exists because flutter_rust_bridge 2.12 cannot express an
/// `Option<DartFn>` parameter — its `DartFn` Rust-side representation is a
/// placeholder that an `Option` cannot wrap. A required callback plus a boolean
/// gate is the faithful, single-call equivalent. The host passes a no-op closure
/// and `false` in release builds.)
///
/// # Errors
/// [`StackError::InvalidCredentials`] if a required secret is missing from
/// `credentials`.
pub fn connect(
    kind: ServiceKind,
    account_id: String,
    credentials: Vec<FrbCredential>,
    debug_logging: bool,
    debug_logger: impl Fn(String) -> DartFnFuture<()> + Send + Sync + 'static,
) -> Result<FrbProvider, StackError> {
    let store: Arc<dyn CredentialStore> =
        Arc::new(MapCredentialStore::new(&account_id, credentials));

    let (core_logger, log_buffer): (Option<Arc<dyn DebugLogger>>, Option<Arc<LogBuffer>>) =
        if debug_logging {
            let buffer = Arc::new(LogBuffer::new(debug_logger));
            (Some(buffer.clone()), Some(buffer))
        } else {
            (None, None)
        };

    let inner = registry::build(kind, &account_id, &store, core_logger)?;
    Ok(FrbProvider {
        inner: Provider::new(inner),
        log_buffer,
    })
}

/// Builds an [`FrbSyncService`] that syncs `provider` for `account_id`.
///
/// Mirror of [`crate::facade::make_sync_service`]. The UniFFI version takes a
/// host `BlobStore` foreign trait up front; FRB instead takes the Dart blob
/// persistence callback later, at [`FrbSyncService::sync_apps`] time (FRB's Dart
/// callbacks are async, and `sync_apps` is the only place persistence runs).
/// Synchronous: it only wires the handles together.
#[flutter_rust_bridge::frb(sync)]
pub fn make_sync_service(provider: &FrbProvider, account_id: String) -> FrbSyncService {
    FrbSyncService {
        provider: provider.inner.clone(),
        account_id,
        log_buffer: provider.log_buffer.clone(),
    }
}

// ---------------------------------------------------------------------------
// Opaque handles
// ---------------------------------------------------------------------------

/// FRB-exposed provider handle: a thin opaque wrapper around the core
/// `Arc<Provider>`. Mirrors the async/sync split of the UniFFI [`Provider`]
/// surface for Phase 1 (validate, fetch_apps, reviews discovery), plus the cheap
/// `kind`/`capabilities` accessors. Adding a *service* never changes this
/// surface; only adding a *capability* would.
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbProvider {
    inner: Arc<Provider>,
    /// Present only when a Dart debug logger was supplied to [`connect`]; drained
    /// to Dart after each async call so the sync core `log` never blocks.
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbProvider {
    /// Which service this provider speaks to.
    #[flutter_rust_bridge::frb(sync)]
    pub fn kind(&self) -> ServiceKind {
        self.inner.kind()
    }

    /// The capabilities exposed for the connected account.
    #[flutter_rust_bridge::frb(sync)]
    pub fn capabilities(&self) -> Vec<Capability> {
        self.inner.capabilities()
    }

    /// Verifies the stored credentials against the live service.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Auth`] when the credentials are rejected, or a
    /// transport/decoding error.
    pub async fn validate(&self) -> Result<(), StackError> {
        let result = self.inner.validate().await;
        self.flush_logs().await;
        result
    }

    /// Lists the apps visible to the connected account.
    ///
    /// # Errors
    /// [`StackError::Unsupported`] if the provider lacks the Apps capability;
    /// otherwise a transport, HTTP, or decoding error.
    pub async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        let result = self.inner.fetch_apps().await;
        self.flush_logs().await;
        result
    }

    /// The Reviews capability handle, or `None` when this provider does not
    /// expose reviews. Mirrors [`Provider::reviews`]: the discovery mechanism is
    /// a `None` return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn reviews(&self) -> Option<FrbReviews> {
        self.inner.reviews().map(|inner| FrbReviews {
            inner,
            log_buffer: self.log_buffer.clone(),
        })
    }

    /// Drains any buffered debug lines to the Dart logger. No-op when no logger
    /// was supplied.
    async fn flush_logs(&self) {
        if let Some(buffer) = &self.log_buffer {
            buffer.flush().await;
        }
    }
}

/// FRB-exposed Reviews capability handle: a thin opaque wrapper around the core
/// `Arc<Reviews>`. Reached via [`FrbProvider::reviews`]. Exposes the Phase 1
/// review surface (list, paged list, reply).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbReviews {
    inner: Arc<crate::service::capabilities::reviews::Reviews>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbReviews {
    /// Lists the end-user reviews for `app_id`, newest first, including any
    /// developer responses.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_customer_reviews(
        &self,
        app_id: String,
    ) -> Result<Vec<CustomerReview>, StackError> {
        let result = self.inner.fetch_customer_reviews(app_id).await;
        self.flush_logs().await;
        result
    }

    /// Fetches a single page of customer reviews for incremental (load-more)
    /// paging, returning the page's reviews plus an opaque `nextToken`.
    ///
    /// `sort` is the raw ASC sort value (`-createdDate` | `createdDate` |
    /// `-rating` | `rating`), passed through unchanged. `filter_rating` is empty
    /// for no filter, else the ratings to include. `page_token` is `None` for
    /// the first page; otherwise pass back a previous call's `nextToken`
    /// verbatim. The returned `nextToken` is `None` once the last page is
    /// reached.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_customer_reviews_page(
        &self,
        app_id: String,
        sort: String,
        filter_rating: Vec<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<CustomerReviewsPage, StackError> {
        let result = self
            .inner
            .fetch_customer_reviews_page(app_id, sort, filter_rating, limit, page_token)
            .await;
        self.flush_logs().await;
        result
    }

    /// Creates or replaces the developer response for `review_id` with `body`,
    /// returning the resulting response (upsert).
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn reply_to_review(
        &self,
        review_id: String,
        body: String,
    ) -> Result<ReviewResponse, StackError> {
        let result = self.inner.reply_to_review(review_id, body).await;
        self.flush_logs().await;
        result
    }

    /// Drains any buffered debug lines to the Dart logger. No-op when no logger
    /// was supplied.
    async fn flush_logs(&self) {
        if let Some(buffer) = &self.log_buffer {
            buffer.flush().await;
        }
    }
}

/// FRB-exposed sync-service handle. Mirrors the core
/// [`crate::service::sync::SyncService`]; unlike the UniFFI version it takes the
/// Dart blob persistence callback at [`Self::sync_apps`] time rather than up
/// front (FRB Dart callbacks are async).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbSyncService {
    provider: Arc<Provider>,
    account_id: String,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbSyncService {
    /// Fetches every visible app and persists each as an AppModel-compatible base
    /// blob through the Dart `persist` callback, then returns the fetched apps.
    ///
    /// This reuses the real core [`SyncService::sync_apps`] so the persisted blob
    /// JSON is byte-for-byte the iOS-facing camelCase contract
    /// (`{id,name,bundleId,platform,accountId}`, keyed by the bare app id). The
    /// core writes each blob into a buffering [`BlobStore`]; once the async core
    /// sync completes, the buffered `(typeName, id, json)` saves are handed to
    /// `persist` in order. `persist` is the Dart async equivalent of
    /// [`BlobStore::save`].
    ///
    /// # Errors
    /// Propagates whatever [`SyncService::sync_apps`] returns
    /// (HTTP/Decode/Network), or [`StackError::Decode`] if an app fails to
    /// serialize.
    pub async fn sync_apps(
        &self,
        persist: impl Fn(String, String, String) -> DartFnFuture<()> + Send + Sync + 'static,
    ) -> Result<Vec<AppInfo>, StackError> {
        let store = Arc::new(BufferingBlobStore::default());
        let core: Arc<dyn BlobStore> = store.clone();
        let svc = SyncService::new(self.provider.clone(), core, self.account_id.clone());

        let result = svc.sync_apps().await;
        if let Some(buffer) = &self.log_buffer {
            buffer.flush().await;
        }
        let apps = result?;

        for (type_name, id, json) in store.take() {
            persist(type_name, id, json).await;
        }
        Ok(apps)
    }
}

// ---------------------------------------------------------------------------
// Port adapters (Dart data/callbacks -> core sync traits). FRB-only.
// ---------------------------------------------------------------------------

/// In-memory [`CredentialStore`] over the resolved secrets the Dart host passed
/// to [`connect`]. Read-only for the connect path: `set_secret`/`delete` are
/// no-ops because `connect` only reads (the host owns its own storage).
///
/// `#[frb(ignore)]`: an internal port adapter, never crosses the boundary.
#[flutter_rust_bridge::frb(ignore)]
struct MapCredentialStore {
    account_id: String,
    secrets: std::collections::HashMap<String, String>,
}

impl MapCredentialStore {
    fn new(account_id: &str, credentials: Vec<FrbCredential>) -> Self {
        Self {
            account_id: account_id.to_string(),
            secrets: credentials
                .into_iter()
                .map(|c| (c.key, c.value))
                .collect(),
        }
    }
}

impl CredentialStore for MapCredentialStore {
    fn secret(&self, account_id: String, key: String) -> Option<String> {
        if account_id == self.account_id {
            self.secrets.get(&key).cloned()
        } else {
            None
        }
    }

    fn set_secret(&self, _account_id: String, _key: String, _value: String) {}

    fn delete(&self, _account_id: String) {}
}

/// Buffers the formatted debug lines the core's (sync) [`DebugLogger`] emits, so
/// they can be flushed to the async Dart closure between calls without blocking
/// the sync `log`. Fire-and-forget by contract, so cross-call order is preserved
/// by flushing after each async provider/reviews call.
///
/// `#[frb(ignore)]`: an internal port adapter, never crosses the boundary.
#[flutter_rust_bridge::frb(ignore)]
struct LogBuffer {
    pending: Mutex<Vec<String>>,
    sink: Box<dyn Fn(String) -> DartFnFuture<()> + Send + Sync + 'static>,
}

impl LogBuffer {
    fn new(sink: impl Fn(String) -> DartFnFuture<()> + Send + Sync + 'static) -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
            sink: Box::new(sink),
        }
    }

    /// Hands every buffered line to the Dart sink in order, then clears the
    /// buffer.
    async fn flush(&self) {
        let lines = {
            let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *guard)
        };
        for line in lines {
            (self.sink)(line).await;
        }
    }
}

impl DebugLogger for LogBuffer {
    fn log(&self, message: String) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(message);
    }
}

/// [`BlobStore`] that records every `save` into a buffer so the FRB layer can
/// replay them to an async Dart persist callback after the core sync completes.
/// Only `save` is exercised by [`SyncService::sync_apps`]; the read/delete
/// methods are present to satisfy the trait and are unused on this path.
///
/// `#[frb(ignore)]`: an internal port adapter, never crosses the boundary.
#[derive(Default)]
#[flutter_rust_bridge::frb(ignore)]
struct BufferingBlobStore {
    saved: Mutex<Vec<(String, String, String)>>,
}

impl BufferingBlobStore {
    /// Drains the recorded saves in insertion order.
    fn take(&self) -> Vec<(String, String, String)> {
        std::mem::take(&mut *self.saved.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl BlobStore for BufferingBlobStore {
    fn save(&self, type_name: String, id: String, json: String) {
        self.saved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((type_name, id, json));
    }

    fn fetch(&self, _type_name: String, _id: String) -> Option<String> {
        None
    }

    fn fetch_all(&self, _type_name: String) -> Vec<String> {
        Vec::new()
    }

    fn delete(&self, _type_name: String, _id: String) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_services_returns_app_store_connect() {
        assert_eq!(available_services(), vec![ServiceKind::AppStoreConnect]);
    }

    #[test]
    fn credential_schema_matches_registry() {
        let schema = credential_schema(ServiceKind::AppStoreConnect);
        let keys: Vec<&str> = schema.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, vec!["issuerId", "keyId", "privateKeyP8"]);
    }

    #[test]
    fn map_credential_store_returns_matching_secret() {
        let store = MapCredentialStore::new(
            "acct-1",
            vec![
                FrbCredential {
                    key: "issuerId".to_string(),
                    value: "issuer-x".to_string(),
                },
                FrbCredential {
                    key: "keyId".to_string(),
                    value: "key-y".to_string(),
                },
            ],
        );

        assert_eq!(
            store.secret("acct-1".to_string(), "issuerId".to_string()),
            Some("issuer-x".to_string())
        );
        assert_eq!(
            store.secret("acct-1".to_string(), "keyId".to_string()),
            Some("key-y".to_string())
        );
        // Missing key -> None (registry::build turns this into InvalidCredentials).
        assert_eq!(
            store.secret("acct-1".to_string(), "privateKeyP8".to_string()),
            None
        );
    }

    #[test]
    fn map_credential_store_isolates_by_account() {
        let store = MapCredentialStore::new(
            "acct-1",
            vec![FrbCredential {
                key: "issuerId".to_string(),
                value: "issuer-x".to_string(),
            }],
        );
        // A lookup for a different account never leaks another account's secret.
        assert_eq!(
            store.secret("acct-2".to_string(), "issuerId".to_string()),
            None
        );
    }

    #[test]
    fn buffering_blob_store_records_saves_in_order_and_drains_once() {
        let store = BufferingBlobStore::default();
        store.save("app".to_string(), "1".to_string(), "{\"id\":\"1\"}".to_string());
        store.save("app".to_string(), "2".to_string(), "{\"id\":\"2\"}".to_string());

        let drained = store.take();
        assert_eq!(
            drained,
            vec![
                ("app".to_string(), "1".to_string(), "{\"id\":\"1\"}".to_string()),
                ("app".to_string(), "2".to_string(), "{\"id\":\"2\"}".to_string()),
            ]
        );
        // A second drain is empty: each save is replayed to Dart exactly once.
        assert!(store.take().is_empty());
    }

    #[test]
    fn buffering_blob_store_read_methods_are_inert() {
        let store = BufferingBlobStore::default();
        store.save("app".to_string(), "1".to_string(), "{}".to_string());
        // sync_apps never reads; these stay inert so a save is observed only via take().
        assert_eq!(store.fetch("app".to_string(), "1".to_string()), None);
        assert!(store.fetch_all("app".to_string()).is_empty());
    }

    #[test]
    fn log_buffer_collects_without_blocking_then_flushes_in_order() {
        // The async `flush` is exercised in the Dart integration slice (it needs a
        // Dart sink); here we assert the *sync* side: `log` only buffers, in order,
        // and never touches the async sink.
        let buffer = LogBuffer::new(|_line| Box::pin(async {}));
        buffer.log("request 1".to_string());
        buffer.log("response 1".to_string());

        let pending = buffer.pending.lock().unwrap();
        assert_eq!(*pending, vec!["request 1".to_string(), "response 1".to_string()]);
    }
}
