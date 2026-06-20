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

use crate::domain::{
    AccessibilityDeclarationInfo, AppCategoryInfo, AppInfo, AppInfoDetails,
    AppInfoLocalizationInfo, AppReviewDetailInfo, AppStoreLocalizationInfo, AppStoreVersionInfo,
    BetaAppLocalizationInfo, BetaAppReviewDetailInfo, BetaBuildLocalizationInfo, BetaGroupInfo,
    BetaTesterInfo, BuildDetailInfo, BuildInfo, BuildsPage, BundleIdCapabilityInfo, BundleIdInfo,
    CertificateInfo, CustomerReview, CustomerReviewsPage, DeviceInfo, PhasedReleaseInfo,
    ProvisioningProfileInfo, ReviewResponse, ScreenshotSetInfo, TeamMemberInfo, UserInfo,
};
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

    /// The Builds capability handle, or `None` when this provider does not expose
    /// builds. Mirrors [`Provider::builds`]: the discovery mechanism is a `None`
    /// return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn builds(&self) -> Option<FrbBuilds> {
        self.inner.builds().map(|inner| FrbBuilds {
            inner,
            log_buffer: self.log_buffer.clone(),
        })
    }

    /// The App Store Versions capability handle, or `None` when this provider does
    /// not expose versions. Mirrors [`Provider::app_store_versions`]: the discovery
    /// mechanism is a `None` return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn app_store_versions(&self) -> Option<FrbAppStoreVersions> {
        self.inner
            .app_store_versions()
            .map(|inner| FrbAppStoreVersions {
                inner,
                log_buffer: self.log_buffer.clone(),
            })
    }

    /// The Beta Groups (TestFlight) capability handle, or `None` when this provider
    /// does not expose beta groups. Mirrors [`Provider::beta_groups`]: the discovery
    /// mechanism is a `None` return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn beta_groups(&self) -> Option<FrbBetaGroups> {
        self.inner.beta_groups().map(|inner| FrbBetaGroups {
            inner,
            log_buffer: self.log_buffer.clone(),
        })
    }

    /// The App Metadata capability handle, or `None` when this provider does not
    /// expose app metadata. Mirrors [`Provider::app_metadata`]: the discovery
    /// mechanism is a `None` return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn app_metadata(&self) -> Option<FrbAppMetadata> {
        self.inner.app_metadata().map(|inner| FrbAppMetadata {
            inner,
            log_buffer: self.log_buffer.clone(),
        })
    }

    /// The Beta App Localizations capability handle, or `None` when this provider
    /// does not expose beta app localizations. Mirrors
    /// [`Provider::beta_app_localizations`]: the discovery mechanism is a `None`
    /// return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn beta_app_localizations(&self) -> Option<FrbBetaAppLocalizations> {
        self.inner
            .beta_app_localizations()
            .map(|inner| FrbBetaAppLocalizations {
                inner,
                log_buffer: self.log_buffer.clone(),
            })
    }

    /// The Beta Build Localizations capability handle, or `None` when this provider
    /// does not expose beta build localizations. Mirrors
    /// [`Provider::beta_build_localizations`]: the discovery mechanism is a `None`
    /// return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn beta_build_localizations(&self) -> Option<FrbBetaBuildLocalizations> {
        self.inner
            .beta_build_localizations()
            .map(|inner| FrbBetaBuildLocalizations {
                inner,
                log_buffer: self.log_buffer.clone(),
            })
    }

    /// The Beta App Review Detail capability handle, or `None` when this provider
    /// does not expose the beta app review detail. Mirrors
    /// [`Provider::beta_app_review_detail`]: the discovery mechanism is a `None`
    /// return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn beta_app_review_detail(&self) -> Option<FrbBetaAppReviewDetail> {
        self.inner
            .beta_app_review_detail()
            .map(|inner| FrbBetaAppReviewDetail {
                inner,
                log_buffer: self.log_buffer.clone(),
            })
    }

    /// The Accessibility Declarations capability handle, or `None` when this
    /// provider does not expose accessibility declarations. Mirrors
    /// [`Provider::accessibility_declarations`]: the discovery mechanism is a `None`
    /// return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn accessibility_declarations(&self) -> Option<FrbAccessibilityDeclarations> {
        self.inner
            .accessibility_declarations()
            .map(|inner| FrbAccessibilityDeclarations {
                inner,
                log_buffer: self.log_buffer.clone(),
            })
    }

    /// The Users capability handle, or `None` when this provider does not expose
    /// user management. Mirrors [`Provider::users`]: the discovery mechanism is a
    /// `None` return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn users(&self) -> Option<FrbUsers> {
        self.inner.users().map(|inner| FrbUsers {
            inner,
            log_buffer: self.log_buffer.clone(),
        })
    }

    /// The Devices capability handle, or `None` when this provider does not expose
    /// device management. Mirrors [`Provider::devices`]: the discovery mechanism is
    /// a `None` return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn devices(&self) -> Option<FrbDevices> {
        self.inner.devices().map(|inner| FrbDevices {
            inner,
            log_buffer: self.log_buffer.clone(),
        })
    }

    /// The BundleIds capability handle, or `None` when this provider does not
    /// expose bundle ID management. Mirrors [`Provider::bundle_ids`]: the discovery
    /// mechanism is a `None` return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn bundle_ids(&self) -> Option<FrbBundleIds> {
        self.inner.bundle_ids().map(|inner| FrbBundleIds {
            inner,
            log_buffer: self.log_buffer.clone(),
        })
    }

    /// The Certificates capability handle, or `None` when this provider does not
    /// expose certificate management. Mirrors [`Provider::certificates`]: the
    /// discovery mechanism is a `None` return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn certificates(&self) -> Option<FrbCertificates> {
        self.inner.certificates().map(|inner| FrbCertificates {
            inner,
            log_buffer: self.log_buffer.clone(),
        })
    }

    /// The Profiles capability handle, or `None` when this provider does not expose
    /// provisioning profile management. Mirrors [`Provider::profiles`]: the
    /// discovery mechanism is a `None` return, not an error.
    #[flutter_rust_bridge::frb(sync)]
    pub fn profiles(&self) -> Option<FrbProfiles> {
        self.inner.profiles().map(|inner| FrbProfiles {
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

/// FRB-exposed Builds capability handle: a thin opaque wrapper around the core
/// `Arc<Builds>`. Reached via [`FrbProvider::builds`]. Mirrors the TestFlight/
/// release build surface (list/page builds, group builds, detail, current build,
/// expire, attach, submit for beta review, add/remove to groups).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbBuilds {
    inner: Arc<crate::service::capabilities::builds::Builds>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbBuilds {
    /// Lists the builds for `app_id`, newest first (by upload date), up to
    /// `limit`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_builds(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<BuildInfo>, StackError> {
        let result = self.inner.fetch_builds(app_id, limit).await;
        self.flush_logs().await;
        result
    }

    /// Fetches a single page of builds for `app_id`, newest first (by upload
    /// date), up to `limit`. When `platform` is `Some`, only builds for that
    /// platform are returned; when `processing_states` is non-empty, only builds
    /// in those states are returned. Pass a prior call's `next_token` back as
    /// `page_token` to load the next page.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_builds_page(
        &self,
        app_id: String,
        platform: Option<String>,
        processing_states: Vec<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<BuildsPage, StackError> {
        let result = self
            .inner
            .fetch_builds_page(app_id, platform, processing_states, limit, page_token)
            .await;
        self.flush_logs().await;
        result
    }

    /// Lists the builds belonging to the beta group `group_id`, newest first (by
    /// upload date), up to `limit`, following pagination to the end.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_builds_for_group(
        &self,
        group_id: String,
        limit: u32,
    ) -> Result<Vec<BuildInfo>, StackError> {
        let result = self.inner.fetch_builds_for_group(group_id, limit).await;
        self.flush_logs().await;
        result
    }

    /// Fetches the full detail of the build `build_id`: the enriched build plus
    /// its associated beta groups and per-locale "What to Test" localizations.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_build_detail(
        &self,
        build_id: String,
    ) -> Result<BuildDetailInfo, StackError> {
        let result = self.inner.fetch_build_detail(build_id).await;
        self.flush_logs().await;
        result
    }

    /// Fetches the build currently attached to the App Store version `version_id`,
    /// or `None` when no build is attached.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_current_build(
        &self,
        version_id: String,
    ) -> Result<Option<BuildInfo>, StackError> {
        let result = self.inner.fetch_current_build(version_id).await;
        self.flush_logs().await;
        result
    }

    /// Marks the build `build_id` as expired (sets its `expired` attribute to
    /// `true`).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn expire_build(&self, build_id: String) -> Result<(), StackError> {
        let result = self.inner.expire_build(build_id).await;
        self.flush_logs().await;
        result
    }

    /// Attaches the build `build_id` to the App Store version `version_id` (sets
    /// the version's `build` to-one relationship).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn attach_build(
        &self,
        version_id: String,
        build_id: String,
    ) -> Result<(), StackError> {
        let result = self.inner.attach_build(version_id, build_id).await;
        self.flush_logs().await;
        result
    }

    /// Submits the build `build_id` for beta (TestFlight) review by creating a
    /// beta app review submission.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn submit_build_for_beta_review(&self, build_id: String) -> Result<(), StackError> {
        let result = self.inner.submit_build_for_beta_review(build_id).await;
        self.flush_logs().await;
        result
    }

    /// Adds the build `build_id` to each beta group in `group_ids` (appends to the
    /// build's `betaGroups` to-many relationship). An empty `group_ids` issues the
    /// request with an empty linkage array.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn add_build_to_groups(
        &self,
        build_id: String,
        group_ids: Vec<String>,
    ) -> Result<(), StackError> {
        let result = self.inner.add_build_to_groups(build_id, group_ids).await;
        self.flush_logs().await;
        result
    }

    /// Removes the build `build_id` from the beta group `group_id` (deletes the
    /// build from the group's `builds` to-many relationship).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn remove_build_from_group(
        &self,
        build_id: String,
        group_id: String,
    ) -> Result<(), StackError> {
        let result = self.inner.remove_build_from_group(build_id, group_id).await;
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

/// FRB-exposed App Store Versions capability handle: a thin opaque wrapper around
/// the core `Arc<AppStoreVersions>`. Reached via
/// [`FrbProvider::app_store_versions`]. Mirrors the version CRUD, lifecycle
/// (submit/cancel/release/reject), phased-release, localization, screenshot, and
/// app-review-detail surface.
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbAppStoreVersions {
    inner: Arc<crate::service::capabilities::app_store_versions::AppStoreVersions>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbAppStoreVersions {
    /// Lists the App Store versions for `app_id`, up to `limit`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_versions(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<AppStoreVersionInfo>, StackError> {
        let result = self.inner.fetch_versions(app_id, limit).await;
        self.flush_logs().await;
        result
    }

    /// Creates a new App Store version for `app_id` on `platform` with
    /// `version_string`, returning the created version. `platform` is the raw ASC
    /// value (`IOS` / `MAC_OS` / `TV_OS` / `VISION_OS`).
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn create_version(
        &self,
        app_id: String,
        platform: String,
        version_string: String,
    ) -> Result<AppStoreVersionInfo, StackError> {
        let result = self
            .inner
            .create_version(app_id, platform, version_string)
            .await;
        self.flush_logs().await;
        result
    }

    /// Updates the version identified by `id`, sending only the provided
    /// attributes. `earliest_release_date` is a raw ISO8601 string passed through
    /// verbatim — the core does no date parsing.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response or [`StackError::Network`] on
    /// transport failure.
    pub async fn update_version(
        &self,
        id: String,
        version_string: Option<String>,
        copyright: Option<String>,
        release_type: Option<String>,
        earliest_release_date: Option<String>,
    ) -> Result<(), StackError> {
        let result = self
            .inner
            .update_version(
                id,
                version_string,
                copyright,
                release_type,
                earliest_release_date,
            )
            .await;
        self.flush_logs().await;
        result
    }

    /// Deletes the version identified by `id`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response or [`StackError::Network`] on
    /// transport failure.
    pub async fn delete_version(&self, id: String) -> Result<(), StackError> {
        let result = self.inner.delete_version(id).await;
        self.flush_logs().await;
        result
    }

    /// Submits the version `version_id` of `app_id` for App Store review. When
    /// `platform` is `Some`, the review submission is scoped to that raw ASC
    /// platform value (`IOS` / `MAC_OS` / `TV_OS` / `VISION_OS`); when `None`, the
    /// submission carries no platform attribute.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn submit_for_review(
        &self,
        app_id: String,
        version_id: String,
        platform: Option<String>,
    ) -> Result<(), StackError> {
        let result = self
            .inner
            .submit_for_review(app_id, version_id, platform)
            .await;
        self.flush_logs().await;
        result
    }

    /// Cancels the active (waiting-for-review or in-review) submission for
    /// `app_id`. When no such submission exists this is a no-op that returns
    /// `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn cancel_review(&self, app_id: String) -> Result<(), StackError> {
        let result = self.inner.cancel_review(app_id).await;
        self.flush_logs().await;
        result
    }

    /// Manually releases the approved version identified by `version_id`. Any 2xx
    /// is treated as success.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn release_version(&self, version_id: String) -> Result<(), StackError> {
        let result = self.inner.release_version(version_id).await;
        self.flush_logs().await;
        result
    }

    /// Cancels the active submission for `app_id`, removing a not-yet-approved
    /// submission from review. When there is no active submission this is a no-op
    /// that returns `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn cancel_submission(&self, app_id: String) -> Result<(), StackError> {
        let result = self.inner.cancel_submission(app_id).await;
        self.flush_logs().await;
        result
    }

    /// Rejects the approved (pending-developer-release) version identified by
    /// `version_id`. When the version has no submission this is a no-op that
    /// returns `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn reject_version(&self, version_id: String) -> Result<(), StackError> {
        let result = self.inner.reject_version(version_id).await;
        self.flush_logs().await;
        result
    }

    /// Fetches the phased (staged) release for `version_id`. Returns `Ok(None)`
    /// when no phased release exists.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_phased_release(
        &self,
        version_id: String,
    ) -> Result<Option<PhasedReleaseInfo>, StackError> {
        let result = self.inner.fetch_phased_release(version_id).await;
        self.flush_logs().await;
        result
    }

    /// Creates a phased (staged) release for `version_id` with the initial
    /// `state`, returning the created phased release. `state` is the raw ASC
    /// `phasedReleaseState` value (`INACTIVE` / `ACTIVE` / `PAUSED` / `COMPLETE`).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn create_phased_release(
        &self,
        version_id: String,
        state: String,
    ) -> Result<PhasedReleaseInfo, StackError> {
        let result = self.inner.create_phased_release(version_id, state).await;
        self.flush_logs().await;
        result
    }

    /// Deletes the phased release identified by `id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_phased_release(&self, id: String) -> Result<(), StackError> {
        let result = self.inner.delete_phased_release(id).await;
        self.flush_logs().await;
        result
    }

    /// Updates the `state` of the phased release identified by `id`, returning the
    /// updated phased release. `state` is the raw ASC `phasedReleaseState` value
    /// (`INACTIVE` / `ACTIVE` / `PAUSED` / `COMPLETE`).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn update_phased_release_state(
        &self,
        id: String,
        state: String,
    ) -> Result<PhasedReleaseInfo, StackError> {
        let result = self.inner.update_phased_release_state(id, state).await;
        self.flush_logs().await;
        result
    }

    /// Lists the version localizations for `version_id`. Each localization carries
    /// the per-locale product-page metadata.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_localizations(
        &self,
        version_id: String,
    ) -> Result<Vec<AppStoreLocalizationInfo>, StackError> {
        let result = self.inner.fetch_localizations(version_id).await;
        self.flush_logs().await;
        result
    }

    /// Updates the version localization identified by `id`, sending only the
    /// `Some` attributes; `None` attributes are omitted entirely.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_localization(
        &self,
        id: String,
        description: Option<String>,
        keywords: Option<String>,
        promotional_text: Option<String>,
        support_url: Option<String>,
        marketing_url: Option<String>,
        whats_new: Option<String>,
    ) -> Result<(), StackError> {
        let result = self
            .inner
            .update_localization(
                id,
                description,
                keywords,
                promotional_text,
                support_url,
                marketing_url,
                whats_new,
            )
            .await;
        self.flush_logs().await;
        result
    }

    /// Lists the screenshot sets (with their screenshots) for the version
    /// localization identified by `localization_id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_screenshot_sets(
        &self,
        localization_id: String,
    ) -> Result<Vec<ScreenshotSetInfo>, StackError> {
        let result = self.inner.fetch_screenshot_sets(localization_id).await;
        self.flush_logs().await;
        result
    }

    /// Fetches the single app review detail for `version_id` — the version's "App
    /// Review Information". Returns `Ok(None)` when no app review detail exists.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_app_review_detail(
        &self,
        version_id: String,
    ) -> Result<Option<AppReviewDetailInfo>, StackError> {
        let result = self.inner.fetch_app_review_detail(version_id).await;
        self.flush_logs().await;
        result
    }

    /// Updates the app review detail `detail_id`, replacing only the provided
    /// attributes, and returns the updated detail. Only the `Some` attributes are
    /// sent in the PATCH body; `None` attributes are omitted entirely.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_app_review_detail(
        &self,
        detail_id: String,
        contact_first_name: Option<String>,
        contact_last_name: Option<String>,
        contact_email: Option<String>,
        contact_phone: Option<String>,
        notes: Option<String>,
        demo_account_name: Option<String>,
        demo_account_password: Option<String>,
        is_demo_account_required: Option<bool>,
    ) -> Result<AppReviewDetailInfo, StackError> {
        let result = self
            .inner
            .update_app_review_detail(
                detail_id,
                contact_first_name,
                contact_last_name,
                contact_email,
                contact_phone,
                notes,
                demo_account_name,
                demo_account_password,
                is_demo_account_required,
            )
            .await;
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

/// FRB-exposed Beta Groups (TestFlight) capability handle: a thin opaque wrapper
/// around the core `Arc<BetaGroups>`. Reached via [`FrbProvider::beta_groups`].
/// Mirrors the group reads (list groups, list testers, tester count) and writes
/// (create/update/delete a group, add/remove a tester, resend an invite).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbBetaGroups {
    inner: Arc<crate::service::capabilities::beta_groups::BetaGroups>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbBetaGroups {
    /// Lists the beta groups for `app_id`, up to `limit`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_beta_groups(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<BetaGroupInfo>, StackError> {
        let result = self.inner.fetch_beta_groups(app_id, limit).await;
        self.flush_logs().await;
        result
    }

    /// Lists the beta testers belonging to `group_id`, up to `limit`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_beta_testers(
        &self,
        group_id: String,
        limit: u32,
    ) -> Result<Vec<BetaTesterInfo>, StackError> {
        let result = self.inner.fetch_beta_testers(group_id, limit).await;
        self.flush_logs().await;
        result
    }

    /// Creates a beta group named `name` under `app_id`, returning the created
    /// group. `is_internal` selects an internal vs. external group;
    /// `public_link_enabled` toggles the TestFlight public link; and
    /// `has_access_to_all_builds` grants the group every build.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_beta_group(
        &self,
        app_id: String,
        name: String,
        is_internal: bool,
        public_link_enabled: bool,
        has_access_to_all_builds: bool,
    ) -> Result<BetaGroupInfo, StackError> {
        let result = self
            .inner
            .create_beta_group(
                app_id,
                name,
                is_internal,
                public_link_enabled,
                has_access_to_all_builds,
            )
            .await;
        self.flush_logs().await;
        result
    }

    /// Updates the beta group `group_id`, applying only the fields that are `Some`
    /// and leaving the rest untouched. `public_link_limit` caps the number of
    /// testers who can join via the public link.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn update_beta_group(
        &self,
        group_id: String,
        name: Option<String>,
        public_link_enabled: Option<bool>,
        public_link_limit: Option<i32>,
        feedback_enabled: Option<bool>,
    ) -> Result<BetaGroupInfo, StackError> {
        let result = self
            .inner
            .update_beta_group(
                group_id,
                name,
                public_link_enabled,
                public_link_limit,
                feedback_enabled,
            )
            .await;
        self.flush_logs().await;
        result
    }

    /// Deletes the beta group `group_id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_beta_group(&self, group_id: String) -> Result<(), StackError> {
        let result = self.inner.delete_beta_group(group_id).await;
        self.flush_logs().await;
        result
    }

    /// Adds a beta tester to `group_id`, creating the tester from `email` and the
    /// optional `first_name`/`last_name`, and returns the created tester.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn add_beta_tester(
        &self,
        group_id: String,
        email: String,
        first_name: Option<String>,
        last_name: Option<String>,
    ) -> Result<BetaTesterInfo, StackError> {
        let result = self
            .inner
            .add_beta_tester(group_id, email, first_name, last_name)
            .await;
        self.flush_logs().await;
        result
    }

    /// Removes the beta tester `tester_id` from `group_id` (unlinks the tester
    /// from the group; the tester itself is not deleted).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn remove_beta_tester(
        &self,
        group_id: String,
        tester_id: String,
    ) -> Result<(), StackError> {
        let result = self.inner.remove_beta_tester(group_id, tester_id).await;
        self.flush_logs().await;
        result
    }

    /// Returns the number of beta testers belonging to `group_id`, read from App
    /// Store Connect's paging metadata without materializing the full list.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_tester_count(&self, group_id: String) -> Result<u32, StackError> {
        let result = self.inner.fetch_tester_count(group_id).await;
        self.flush_logs().await;
        result
    }

    /// Resends the TestFlight invite for the beta tester `tester_id` on `app_id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn resend_invite(&self, tester_id: String, app_id: String) -> Result<(), StackError> {
        let result = self.inner.resend_invite(tester_id, app_id).await;
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

/// FRB-exposed App Metadata capability handle: a thin opaque wrapper around the
/// core `Arc<AppMetadata>`. Reached via [`FrbProvider::app_metadata`]. Mirrors the
/// App Store product-page metadata surface: app-info localization CRUD (name/
/// subtitle + privacy links/text), App Info read, category list, category / app /
/// age-rating updates, and icon-URL resolution.
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbAppMetadata {
    inner: Arc<crate::service::capabilities::app_metadata::AppMetadata>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbAppMetadata {
    /// Lists the app-info localizations for `app_info_id`. Each carries the
    /// per-locale product-page `name`/`subtitle` and the three privacy links/text.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_app_info_localizations(
        &self,
        app_info_id: String,
    ) -> Result<Vec<AppInfoLocalizationInfo>, StackError> {
        let result = self.inner.fetch_app_info_localizations(app_info_id).await;
        self.flush_logs().await;
        result
    }

    /// Updates the app-info localization `id`, returning the updated localization.
    /// `name` is always sent; `subtitle` is sent only when `Some`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn update_app_info_localization(
        &self,
        id: String,
        name: String,
        subtitle: Option<String>,
    ) -> Result<AppInfoLocalizationInfo, StackError> {
        let result = self
            .inner
            .update_app_info_localization(id, name, subtitle)
            .await;
        self.flush_logs().await;
        result
    }

    /// Updates the privacy attributes of the app-info localization `id`, replacing
    /// only the provided `privacy_policy_url`, `privacy_choices_url`, and/or
    /// `privacy_policy_text` attributes, and returns the updated localization.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn update_app_info_localization_privacy(
        &self,
        id: String,
        privacy_policy_url: Option<String>,
        privacy_choices_url: Option<String>,
        privacy_policy_text: Option<String>,
    ) -> Result<AppInfoLocalizationInfo, StackError> {
        let result = self
            .inner
            .update_app_info_localization_privacy(
                id,
                privacy_policy_url,
                privacy_choices_url,
                privacy_policy_text,
            )
            .await;
        self.flush_logs().await;
        result
    }

    /// Creates an app-info localization for `app_info_id` in `locale`, returning
    /// the created localization. `name` is always set; `subtitle` is set only when
    /// `Some`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_app_info_localization(
        &self,
        app_info_id: String,
        locale: String,
        name: String,
        subtitle: Option<String>,
    ) -> Result<AppInfoLocalizationInfo, StackError> {
        let result = self
            .inner
            .create_app_info_localization(app_info_id, locale, name, subtitle)
            .await;
        self.flush_logs().await;
        result
    }

    /// Deletes the app-info localization `id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_app_info_localization(&self, id: String) -> Result<(), StackError> {
        let result = self.inner.delete_app_info_localization(id).await;
        self.flush_logs().await;
        result
    }

    /// Fetches the full App Info detail for `app_id`: the app-info ids,
    /// category/age-rating wiring, and per-locale localizations, merged with the
    /// owning app's `sku`/`primary_locale`/`content_rights_declaration`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_app_info(&self, app_id: String) -> Result<AppInfoDetails, StackError> {
        let result = self.inner.fetch_app_info(app_id).await;
        self.flush_logs().await;
        result
    }

    /// Lists the top-level App Store categories (iOS), each with the ids of its
    /// subcategories.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_app_categories(&self) -> Result<Vec<AppCategoryInfo>, StackError> {
        let result = self.inner.fetch_app_categories().await;
        self.flush_logs().await;
        result
    }

    /// Updates the category relationships of the app-info `app_info_id`. Each of
    /// `primary_category_id`, `subcategory_one_id`, `secondary_category_id`, and
    /// `secondary_subcategory_one_id` is wired only when `Some`; the others are
    /// omitted (not sent as `null`).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn update_app_info_category(
        &self,
        app_info_id: String,
        primary_category_id: Option<String>,
        subcategory_one_id: Option<String>,
        secondary_category_id: Option<String>,
        secondary_subcategory_one_id: Option<String>,
    ) -> Result<(), StackError> {
        let result = self
            .inner
            .update_app_info_category(
                app_info_id,
                primary_category_id,
                subcategory_one_id,
                secondary_category_id,
                secondary_subcategory_one_id,
            )
            .await;
        self.flush_logs().await;
        result
    }

    /// Updates the app `id`, sending `content_rights_declaration` and/or
    /// `primary_locale` only when `Some`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn update_app(
        &self,
        id: String,
        content_rights_declaration: Option<String>,
        primary_locale: Option<String>,
    ) -> Result<(), StackError> {
        let result = self
            .inner
            .update_app(id, content_rights_declaration, primary_locale)
            .await;
        self.flush_logs().await;
        result
    }

    /// Updates the age-rating declaration `id`, sending all 18 attributes (all
    /// required from the host).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_age_rating(
        &self,
        id: String,
        alcohol_tobacco: String,
        contests: String,
        gambling_simulated: String,
        guns_or_other_weapons: String,
        medical_information: String,
        profanity: String,
        sexual_content_graphic: String,
        sexual_content_or_nudity: String,
        horror_or_fear: String,
        mature_or_suggestive: String,
        violence_cartoon: String,
        violence_realistic: String,
        violence_graphic: String,
        is_advertising: bool,
        is_gambling: bool,
        is_unrestricted_web_access: bool,
        is_user_generated_content: bool,
        age_rating_override: String,
    ) -> Result<(), StackError> {
        let result = self
            .inner
            .update_age_rating(
                id,
                alcohol_tobacco,
                contests,
                gambling_simulated,
                guns_or_other_weapons,
                medical_information,
                profanity,
                sexual_content_graphic,
                sexual_content_or_nudity,
                horror_or_fear,
                mature_or_suggestive,
                violence_cartoon,
                violence_realistic,
                violence_graphic,
                is_advertising,
                is_gambling,
                is_unrestricted_web_access,
                is_user_generated_content,
                age_rating_override,
            )
            .await;
        self.flush_logs().await;
        result
    }

    /// Resolves the icon URL for `app_id` from its most recent build, or `None`
    /// when there is no build / no icon token.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_icon_url(&self, app_id: String) -> Result<Option<String>, StackError> {
        let result = self.inner.fetch_icon_url(app_id).await;
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

/// FRB-exposed Beta App Localizations capability handle: a thin opaque wrapper
/// around the core `Arc<BetaAppLocalizations>`. Reached via
/// [`FrbProvider::beta_app_localizations`]. Mirrors the TestFlight app-level
/// per-locale "feedback email" + "test description" surface (list/create/update).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbBetaAppLocalizations {
    inner: Arc<crate::service::capabilities::beta_app_localizations::BetaAppLocalizations>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbBetaAppLocalizations {
    /// Lists the beta app localizations for `app_id`, up to `limit`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_beta_app_localizations(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<BetaAppLocalizationInfo>, StackError> {
        let result = self.inner.fetch_beta_app_localizations(app_id, limit).await;
        self.flush_logs().await;
        result
    }

    /// Creates a beta app localization for `app_id` in `locale`, returning the
    /// created localization. `feedback_email` is the per-locale address testers'
    /// feedback is sent to, and `description` is the per-locale TestFlight test
    /// description shown to testers; both are optional.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_beta_app_localization(
        &self,
        app_id: String,
        locale: String,
        feedback_email: Option<String>,
        description: Option<String>,
    ) -> Result<BetaAppLocalizationInfo, StackError> {
        let result = self
            .inner
            .create_beta_app_localization(app_id, locale, feedback_email, description)
            .await;
        self.flush_logs().await;
        result
    }

    /// Updates the beta app localization `id`, replacing only the provided
    /// `feedback_email` and/or `description` attributes, and returns the updated
    /// localization.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn update_beta_app_localization(
        &self,
        id: String,
        feedback_email: Option<String>,
        description: Option<String>,
    ) -> Result<BetaAppLocalizationInfo, StackError> {
        let result = self
            .inner
            .update_beta_app_localization(id, feedback_email, description)
            .await;
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

/// FRB-exposed Beta Build Localizations capability handle: a thin opaque wrapper
/// around the core `Arc<BetaBuildLocalizations>`. Reached via
/// [`FrbProvider::beta_build_localizations`]. Mirrors the TestFlight per-locale
/// "What to Test" surface (list/create/update).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbBetaBuildLocalizations {
    inner: Arc<crate::service::capabilities::beta_build_localizations::BetaBuildLocalizations>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbBetaBuildLocalizations {
    /// Lists the beta build localizations for `build_id`, up to `limit`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_beta_build_localizations(
        &self,
        build_id: String,
        limit: u32,
    ) -> Result<Vec<BetaBuildLocalizationInfo>, StackError> {
        let result = self
            .inner
            .fetch_beta_build_localizations(build_id, limit)
            .await;
        self.flush_logs().await;
        result
    }

    /// Creates a beta build localization for `build_id` in `locale`, returning the
    /// created localization. `whats_new` is the per-locale "What to Test" text
    /// shown to testers.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_beta_build_localization(
        &self,
        build_id: String,
        locale: String,
        whats_new: String,
    ) -> Result<BetaBuildLocalizationInfo, StackError> {
        let result = self
            .inner
            .create_beta_build_localization(build_id, locale, whats_new)
            .await;
        self.flush_logs().await;
        result
    }

    /// Updates the beta build localization `id`, replacing its `whats_new` testing
    /// notes, and returns the updated localization.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn update_beta_build_localization(
        &self,
        id: String,
        whats_new: String,
    ) -> Result<BetaBuildLocalizationInfo, StackError> {
        let result = self
            .inner
            .update_beta_build_localization(id, whats_new)
            .await;
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

/// FRB-exposed Beta App Review Detail capability handle: a thin opaque wrapper
/// around the core `Arc<BetaAppReviewDetail>`. Reached via
/// [`FrbProvider::beta_app_review_detail`]. Mirrors the TestFlight "Test
/// Information" surface (fetch the app's single detail, update the contact /
/// demo-account / notes attributes).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbBetaAppReviewDetail {
    inner: Arc<crate::service::capabilities::beta_app_review_detail::BetaAppReviewDetail>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbBetaAppReviewDetail {
    /// Fetches the single beta app review detail for `app_id` — the TestFlight
    /// "Test Information" containing the beta review contact and optional demo
    /// account credentials.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_beta_app_review_detail(
        &self,
        app_id: String,
    ) -> Result<BetaAppReviewDetailInfo, StackError> {
        let result = self.inner.fetch_beta_app_review_detail(app_id).await;
        self.flush_logs().await;
        result
    }

    /// Updates the beta app review detail `detail_id`, replacing only the provided
    /// attributes, and returns the updated detail. Every attribute is optional:
    /// `contact_*` set the beta review contact, `demo_account_*` set the demo
    /// account credentials, `is_demo_account_required` toggles whether a demo
    /// account is needed, and `notes` are the reviewer notes.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_beta_app_review_detail(
        &self,
        detail_id: String,
        contact_first_name: Option<String>,
        contact_last_name: Option<String>,
        contact_email: Option<String>,
        contact_phone: Option<String>,
        demo_account_name: Option<String>,
        demo_account_password: Option<String>,
        is_demo_account_required: Option<bool>,
        notes: Option<String>,
    ) -> Result<BetaAppReviewDetailInfo, StackError> {
        let result = self
            .inner
            .update_beta_app_review_detail(
                detail_id,
                contact_first_name,
                contact_last_name,
                contact_email,
                contact_phone,
                demo_account_name,
                demo_account_password,
                is_demo_account_required,
                notes,
            )
            .await;
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

/// FRB-exposed Accessibility Declarations capability handle: a thin opaque wrapper
/// around the core `Arc<AccessibilityDeclarations>`. Reached via
/// [`FrbProvider::accessibility_declarations`]. Mirrors the per-device-family
/// accessibility declaration surface (list, create, update features/publish,
/// delete).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbAccessibilityDeclarations {
    inner: Arc<crate::service::capabilities::accessibility_declarations::AccessibilityDeclarations>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbAccessibilityDeclarations {
    /// Lists the accessibility declarations for `app_id`, up to `limit` per page,
    /// following pagination until exhausted.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_accessibility_declarations(
        &self,
        app_id: String,
        limit: i64,
    ) -> Result<Vec<AccessibilityDeclarationInfo>, StackError> {
        let result = self
            .inner
            .fetch_accessibility_declarations(app_id, limit)
            .await;
        self.flush_logs().await;
        result
    }

    /// Creates an accessibility declaration for `app_id` targeting `device_family`
    /// (an App Store Connect device-family value such as `IPHONE`, `IPAD`,
    /// `APPLE_TV`, `APPLE_WATCH`, `MAC`, or `VISION`), returning the created
    /// declaration. The core forwards `device_family` verbatim; App Store Connect
    /// rejects unknown values with an HTTP error.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_accessibility_declaration(
        &self,
        app_id: String,
        device_family: String,
    ) -> Result<AccessibilityDeclarationInfo, StackError> {
        let result = self
            .inner
            .create_accessibility_declaration(app_id, device_family)
            .await;
        self.flush_logs().await;
        result
    }

    /// Updates the accessibility declaration `id`, setting all nine supported
    /// feature flags and, when `publish` is `true`, publishing the declaration (the
    /// `publish` attribute is omitted entirely when `publish` is `false`). Returns
    /// the updated declaration.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_accessibility_declaration(
        &self,
        id: String,
        publish: bool,
        supports_audio_descriptions: bool,
        supports_captions: bool,
        supports_dark_interface: bool,
        supports_differentiate_without_color: bool,
        supports_larger_text: bool,
        supports_reduced_motion: bool,
        supports_sufficient_contrast: bool,
        supports_voice_control: bool,
        supports_voiceover: bool,
    ) -> Result<AccessibilityDeclarationInfo, StackError> {
        let result = self
            .inner
            .update_accessibility_declaration(
                id,
                publish,
                supports_audio_descriptions,
                supports_captions,
                supports_dark_interface,
                supports_differentiate_without_color,
                supports_larger_text,
                supports_reduced_motion,
                supports_sufficient_contrast,
                supports_voice_control,
                supports_voiceover,
            )
            .await;
        self.flush_logs().await;
        result
    }

    /// Deletes the accessibility declaration `id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_accessibility_declaration(&self, id: String) -> Result<(), StackError> {
        let result = self.inner.delete_accessibility_declaration(id).await;
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

/// FRB-exposed Users capability handle: a thin opaque wrapper around the core
/// `Arc<Users>`. Reached via [`FrbProvider::users`]. Mirrors the account
/// team-member surface: the lightweight team-member list, the unified active +
/// pending user list, inviting a user, and deleting a user / cancelling an
/// invitation.
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbUsers {
    inner: Arc<crate::service::capabilities::users::Users>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbUsers {
    /// Lists the team members of the connected account — the lightweight
    /// projection of the active `users` resources (no pending invitations),
    /// carrying only `first_name`/`last_name`/`username` and the raw ASC `roles`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_team_members(&self) -> Result<Vec<TeamMemberInfo>, StackError> {
        let result = self.inner.fetch_team_members().await;
        self.flush_logs().await;
        result
    }

    /// Lists every user of the connected account: the active members (`users`)
    /// followed by the outstanding invitations (`userInvitations`), unified into
    /// one list and discriminated by `is_pending`. For active members `email` is
    /// taken from the `username` attribute; pending invitations carry their own
    /// `email` and `expiration_date`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_users(&self) -> Result<Vec<UserInfo>, StackError> {
        let result = self.inner.fetch_users().await;
        self.flush_logs().await;
        result
    }

    /// Invites a new user to the connected account, granting the raw ASC `roles`
    /// (e.g. `"ADMIN"`, `"DEVELOPER"`, `"APP_MANAGER"`), passed through verbatim.
    /// `all_apps_visible` and `provisioning_allowed` set the corresponding
    /// invitation flags.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn invite_user(
        &self,
        email: String,
        first_name: String,
        last_name: String,
        roles: Vec<String>,
        all_apps_visible: bool,
        provisioning_allowed: bool,
    ) -> Result<(), StackError> {
        let result = self
            .inner
            .invite_user(
                email,
                first_name,
                last_name,
                roles,
                all_apps_visible,
                provisioning_allowed,
            )
            .await;
        self.flush_logs().await;
        result
    }

    /// Deletes the user `id`. When `is_pending` is `true` the id is an outstanding
    /// `userInvitations` resource and the invitation is cancelled; otherwise the id
    /// is an active `users` resource and the member is removed.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_user(&self, id: String, is_pending: bool) -> Result<(), StackError> {
        let result = self.inner.delete_user(id, is_pending).await;
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

/// FRB-exposed Devices capability handle: a thin opaque wrapper around the core
/// `Arc<Devices>`. Reached via [`FrbProvider::devices`]. Mirrors the registered
/// device surface (list, register, rename/disable).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbDevices {
    inner: Arc<crate::service::capabilities::devices::Devices>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbDevices {
    /// Lists every registered device of the connected account, sorted by name,
    /// following pagination until exhausted.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_devices(&self) -> Result<Vec<DeviceInfo>, StackError> {
        let result = self.inner.fetch_devices().await;
        self.flush_logs().await;
        result
    }

    /// Registers a new device with `name`, ASC `platform` (a raw `BundleIdPlatform`
    /// value such as `IOS`, `MAC_OS`, or `UNIVERSAL`, forwarded verbatim — App
    /// Store Connect rejects unknown values with an HTTP error), and `udid`,
    /// returning the created device.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_device(
        &self,
        name: String,
        platform: String,
        udid: String,
    ) -> Result<DeviceInfo, StackError> {
        let result = self.inner.create_device(name, platform, udid).await;
        self.flush_logs().await;
        result
    }

    /// Updates the device `id`, sending only the attributes that are `Some`: `name`
    /// renames the device, and `status` (`"DISABLED"` to remove it from the
    /// account, `"ENABLED"` to re-enable it) changes its status. Attributes left
    /// `None` are omitted from the request entirely.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn update_device(
        &self,
        id: String,
        name: Option<String>,
        status: Option<String>,
    ) -> Result<(), StackError> {
        let result = self.inner.update_device(id, name, status).await;
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

/// FRB-exposed BundleIds capability handle: a thin opaque wrapper around the core
/// `Arc<BundleIds>`. Reached via [`FrbProvider::bundle_ids`]. Mirrors the bundle
/// ID reads/writes (list, create, rename, delete) plus the per-bundle capability
/// sub-collection (list, enable, disable).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbBundleIds {
    inner: Arc<crate::service::capabilities::bundle_ids::BundleIds>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbBundleIds {
    /// Lists every bundle ID of the connected account, sorted by name, following
    /// pagination until exhausted.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_bundle_ids(&self) -> Result<Vec<BundleIdInfo>, StackError> {
        let result = self.inner.fetch_bundle_ids().await;
        self.flush_logs().await;
        result
    }

    /// Registers a new bundle ID with `identifier`, `name`, and ASC `platform` (a
    /// raw `BundleIdPlatform` value such as `IOS`, `MAC_OS`, or `UNIVERSAL`,
    /// forwarded verbatim — App Store Connect rejects unknown values with an HTTP
    /// error), returning the created bundle ID.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_bundle_id(
        &self,
        identifier: String,
        name: String,
        platform: String,
    ) -> Result<BundleIdInfo, StackError> {
        let result = self
            .inner
            .create_bundle_id(identifier, name, platform)
            .await;
        self.flush_logs().await;
        result
    }

    /// Renames the bundle ID `id`. Only the `name` is mutable; the identifier and
    /// platform are fixed at creation.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn update_bundle_id(&self, id: String, name: String) -> Result<(), StackError> {
        let result = self.inner.update_bundle_id(id, name).await;
        self.flush_logs().await;
        result
    }

    /// Deletes the bundle ID `id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_bundle_id(&self, id: String) -> Result<(), StackError> {
        let result = self.inner.delete_bundle_id(id).await;
        self.flush_logs().await;
        result
    }

    /// Lists the capabilities enabled on `bundle_id`, following pagination until
    /// exhausted. Entries whose `capabilityType` is missing or empty are skipped.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_bundle_id_capabilities(
        &self,
        bundle_id: String,
    ) -> Result<Vec<BundleIdCapabilityInfo>, StackError> {
        let result = self.inner.fetch_bundle_id_capabilities(bundle_id).await;
        self.flush_logs().await;
        result
    }

    /// Enables `capability_type` (a raw ASC `capabilityType` string, forwarded
    /// verbatim) on `bundle_id`, returning the created capability.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn enable_capability(
        &self,
        bundle_id: String,
        capability_type: String,
    ) -> Result<BundleIdCapabilityInfo, StackError> {
        let result = self
            .inner
            .enable_capability(bundle_id, capability_type)
            .await;
        self.flush_logs().await;
        result
    }

    /// Disables the capability `capability_id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn disable_capability(&self, capability_id: String) -> Result<(), StackError> {
        let result = self.inner.disable_capability(capability_id).await;
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

/// FRB-exposed Certificates capability handle: a thin opaque wrapper around the
/// core `Arc<Certificates>`. Reached via [`FrbProvider::certificates`]. Mirrors
/// the signing-certificate reads (list, fetch content) and writes (create from a
/// CSR, revoke).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbCertificates {
    inner: Arc<crate::service::capabilities::certificates::Certificates>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbCertificates {
    /// Lists every certificate of the connected account, sorted by display name,
    /// following pagination until exhausted. The list does not include certificate
    /// content, so every entry's `certificate_content` is `None`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_certificates(&self) -> Result<Vec<CertificateInfo>, StackError> {
        let result = self.inner.fetch_certificates().await;
        self.flush_logs().await;
        result
    }

    /// Fetches the base64-encoded `certificateContent` of the certificate `id`,
    /// returning `None` when App Store Connect omits the attribute.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_certificate_content(
        &self,
        id: String,
    ) -> Result<Option<String>, StackError> {
        let result = self.inner.fetch_certificate_content(id).await;
        self.flush_logs().await;
        result
    }

    /// Creates a certificate from `csr_content` (a base64/PEM CSR) of
    /// `certificate_type` (a raw ASC `CertificateType` value, forwarded verbatim —
    /// App Store Connect rejects unknown values with an HTTP error). When
    /// `pass_type_id` is `Some` and non-empty it is attached as the `passTypeId`
    /// relationship; otherwise when `merchant_id` is `Some` and non-empty it is
    /// attached as the `merchantId` relationship; otherwise no relationship is
    /// sent. The returned certificate includes its `certificate_content`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_certificate(
        &self,
        csr_content: String,
        certificate_type: String,
        pass_type_id: Option<String>,
        merchant_id: Option<String>,
    ) -> Result<CertificateInfo, StackError> {
        let result = self
            .inner
            .create_certificate(csr_content, certificate_type, pass_type_id, merchant_id)
            .await;
        self.flush_logs().await;
        result
    }

    /// Revokes (deletes) the certificate `id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn revoke_certificate(&self, id: String) -> Result<(), StackError> {
        let result = self.inner.revoke_certificate(id).await;
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

/// FRB-exposed Profiles capability handle: a thin opaque wrapper around the core
/// `Arc<Profiles>`. Reached via [`FrbProvider::profiles`]. Mirrors the
/// provisioning-profile reads (list with resolved bundle ID, fetch content) and
/// writes (create from a bundle ID + certificates + optional devices, delete).
#[flutter_rust_bridge::frb(opaque)]
pub struct FrbProfiles {
    inner: Arc<crate::service::capabilities::profiles::Profiles>,
    log_buffer: Option<Arc<LogBuffer>>,
}

impl FrbProfiles {
    /// Lists every provisioning profile of the connected account, sorted by name,
    /// following pagination until exhausted. Each profile's `bundle_id` is resolved
    /// to the referenced bundle ID's `identifier` string via the response's
    /// `included[]` bundleIds (or `None` when the relationship is missing or the
    /// bundle ID is absent from `included[]`). The list does not include profile
    /// content, so every entry's `profile_content` is `None`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_profiles(&self) -> Result<Vec<ProvisioningProfileInfo>, StackError> {
        let result = self.inner.fetch_profiles().await;
        self.flush_logs().await;
        result
    }

    /// Creates a provisioning profile named `name` of `profile_type` (a raw ASC
    /// `ProfileType` value such as `IOS_APP_DEVELOPMENT`, `IOS_APP_STORE`, or
    /// `MAC_APP_STORE`, forwarded verbatim — App Store Connect rejects unknown
    /// values with an HTTP error), related to the bundle ID `bundle_id_id` and the
    /// signing certificates `certificate_ids`. When `device_ids` is non-empty the
    /// `devices` relationship is attached; when empty it is omitted entirely (App
    /// Store Connect rejects an empty `devices` array for App Store profiles). The
    /// `certificates` relationship is always sent, even when `certificate_ids` is
    /// empty. The returned profile includes its `profile_content`; its `bundle_id`
    /// is `None` (not resolved on create).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_profile(
        &self,
        name: String,
        profile_type: String,
        bundle_id_id: String,
        certificate_ids: Vec<String>,
        device_ids: Vec<String>,
    ) -> Result<ProvisioningProfileInfo, StackError> {
        let result = self
            .inner
            .create_profile(
                name,
                profile_type,
                bundle_id_id,
                certificate_ids,
                device_ids,
            )
            .await;
        self.flush_logs().await;
        result
    }

    /// Deletes the profile `id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_profile(&self, id: String) -> Result<(), StackError> {
        let result = self.inner.delete_profile(id).await;
        self.flush_logs().await;
        result
    }

    /// Fetches the base64-encoded `profileContent` of the profile `id`, returning
    /// `None` when App Store Connect omits the attribute.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_profile_content(&self, id: String) -> Result<Option<String>, StackError> {
        let result = self.inner.fetch_profile_content(id).await;
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
            secrets: credentials.into_iter().map(|c| (c.key, c.value)).collect(),
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
        store.save(
            "app".to_string(),
            "1".to_string(),
            "{\"id\":\"1\"}".to_string(),
        );
        store.save(
            "app".to_string(),
            "2".to_string(),
            "{\"id\":\"2\"}".to_string(),
        );

        let drained = store.take();
        assert_eq!(
            drained,
            vec![
                (
                    "app".to_string(),
                    "1".to_string(),
                    "{\"id\":\"1\"}".to_string()
                ),
                (
                    "app".to_string(),
                    "2".to_string(),
                    "{\"id\":\"2\"}".to_string()
                ),
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
        assert_eq!(
            *pending,
            vec!["request 1".to_string(), "response 1".to_string()]
        );
    }
}
