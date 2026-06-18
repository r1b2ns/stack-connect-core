use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::AppInfo;
use crate::error::StackError;
use crate::service::capabilities::app_metadata::AppMetadata;
use crate::service::capabilities::app_store_versions::AppStoreVersions;
use crate::service::capabilities::beta_app_localizations::BetaAppLocalizations;
use crate::service::capabilities::beta_app_review_detail::BetaAppReviewDetail;
use crate::service::capabilities::beta_build_localizations::BetaBuildLocalizations;
use crate::service::capabilities::beta_groups::BetaGroups;
use crate::service::capabilities::builds::Builds;
use crate::service::capabilities::reviews::Reviews;
use crate::service::kind::ServiceKind;

/// A capability a provider may expose. The host calls [`Provider::capabilities`]
/// to learn what a connected account can do; capabilities a provider lacks make
/// the corresponding accessor (e.g. [`Provider::reviews`]) return `None`. Grows
/// over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum Capability {
    Apps,
    Reviews,
    AppStoreVersions,
    Builds,
    BetaGroups,
    BetaBuildLocalizations,
    BetaAppLocalizations,
    BetaAppReviewDetail,
    AppMetadata,
}

/// Internal, non-exported contract every concrete plugin implements. Kept off the
/// FFI on purpose: UniFFI cannot export an async *trait* cleanly, so the public
/// surface is the concrete [`Provider`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn ProviderImpl>` can live inside an `Arc<Provider>`
/// shared across the tokio runtime.
#[async_trait]
pub(crate) trait ProviderImpl: Send + Sync {
    /// Which service this provider speaks to.
    fn kind(&self) -> ServiceKind;

    /// The capabilities this provider exposes for the connected account.
    fn capabilities(&self) -> Vec<Capability>;

    /// Cheaply verifies the stored credentials against the live service.
    async fn validate(&self) -> Result<(), StackError>;

    /// Lists the apps visible to the connected account.
    async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError>;

    /// The Reviews capability handle, or `None` if this provider lacks
    /// [`Capability::Reviews`]. Default `None` so providers opt in explicitly.
    fn reviews(&self) -> Option<Arc<Reviews>> {
        None
    }

    /// The App Store Versions capability handle, or `None` if this provider lacks
    /// [`Capability::AppStoreVersions`]. Default `None` so providers opt in
    /// explicitly.
    fn app_store_versions(&self) -> Option<Arc<AppStoreVersions>> {
        None
    }

    /// The Builds capability handle, or `None` if this provider lacks
    /// [`Capability::Builds`]. Default `None` so providers opt in explicitly.
    fn builds(&self) -> Option<Arc<Builds>> {
        None
    }

    /// The Beta Groups capability handle, or `None` if this provider lacks
    /// [`Capability::BetaGroups`]. Default `None` so providers opt in explicitly.
    fn beta_groups(&self) -> Option<Arc<BetaGroups>> {
        None
    }

    /// The Beta Build Localizations capability handle, or `None` if this provider
    /// lacks [`Capability::BetaBuildLocalizations`]. Default `None` so providers
    /// opt in explicitly.
    fn beta_build_localizations(&self) -> Option<Arc<BetaBuildLocalizations>> {
        None
    }

    /// The Beta App Localizations capability handle, or `None` if this provider
    /// lacks [`Capability::BetaAppLocalizations`]. Default `None` so providers
    /// opt in explicitly.
    fn beta_app_localizations(&self) -> Option<Arc<BetaAppLocalizations>> {
        None
    }

    /// The Beta App Review Detail capability handle, or `None` if this provider
    /// lacks [`Capability::BetaAppReviewDetail`]. Default `None` so providers opt
    /// in explicitly.
    fn beta_app_review_detail(&self) -> Option<Arc<BetaAppReviewDetail>> {
        None
    }

    /// The App Metadata capability handle, or `None` if this provider lacks
    /// [`Capability::AppMetadata`]. Default `None` so providers opt in
    /// explicitly.
    fn app_metadata(&self) -> Option<Arc<AppMetadata>> {
        None
    }
}

/// UniFFI-exported provider handle. A thin, binding-friendly wrapper around a
/// boxed [`ProviderImpl`]: synchronous metadata is exported directly, async work
/// runs on the tokio runtime. Adding a *service* never changes this surface —
/// only adding a *capability* would add a method here.
#[derive(uniffi::Object)]
pub struct Provider {
    inner: Box<dyn ProviderImpl>,
}

impl Provider {
    /// Wraps a concrete plugin built by the registry into the exported handle.
    pub(crate) fn new(inner: Box<dyn ProviderImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export]
impl Provider {
    /// Which service this provider speaks to.
    pub fn kind(&self) -> ServiceKind {
        self.inner.kind()
    }

    /// The capabilities exposed for the connected account.
    pub fn capabilities(&self) -> Vec<Capability> {
        self.inner.capabilities()
    }

    /// The Reviews capability handle, or `None` when this provider does not
    /// expose [`Capability::Reviews`]. This is the discovery mechanism: the host
    /// calls `provider.reviews()` and gets `None` when reviews are unsupported.
    pub fn reviews(&self) -> Option<Arc<Reviews>> {
        self.inner.reviews()
    }

    /// The App Store Versions capability handle, or `None` when this provider does
    /// not expose [`Capability::AppStoreVersions`]. This is the discovery
    /// mechanism: the host calls `provider.app_store_versions()` and gets `None`
    /// when versions are unsupported.
    pub fn app_store_versions(&self) -> Option<Arc<AppStoreVersions>> {
        self.inner.app_store_versions()
    }

    /// The Builds capability handle, or `None` when this provider does not expose
    /// [`Capability::Builds`]. This is the discovery mechanism: the host calls
    /// `provider.builds()` and gets `None` when builds are unsupported.
    pub fn builds(&self) -> Option<Arc<Builds>> {
        self.inner.builds()
    }

    /// The Beta Groups capability handle, or `None` when this provider does not
    /// expose [`Capability::BetaGroups`]. This is the discovery mechanism: the
    /// host calls `provider.beta_groups()` and gets `None` when beta groups are
    /// unsupported.
    pub fn beta_groups(&self) -> Option<Arc<BetaGroups>> {
        self.inner.beta_groups()
    }

    /// The Beta Build Localizations capability handle, or `None` when this
    /// provider does not expose [`Capability::BetaBuildLocalizations`]. This is
    /// the discovery mechanism: the host calls
    /// `provider.beta_build_localizations()` and gets `None` when beta build
    /// localizations are unsupported.
    pub fn beta_build_localizations(&self) -> Option<Arc<BetaBuildLocalizations>> {
        self.inner.beta_build_localizations()
    }

    /// The Beta App Localizations capability handle, or `None` when this provider
    /// does not expose [`Capability::BetaAppLocalizations`]. This is the discovery
    /// mechanism: the host calls `provider.beta_app_localizations()` and gets
    /// `None` when beta app localizations are unsupported.
    pub fn beta_app_localizations(&self) -> Option<Arc<BetaAppLocalizations>> {
        self.inner.beta_app_localizations()
    }

    /// The Beta App Review Detail capability handle, or `None` when this provider
    /// does not expose [`Capability::BetaAppReviewDetail`]. This is the discovery
    /// mechanism: the host calls `provider.beta_app_review_detail()` and gets
    /// `None` when the beta app review detail is unsupported.
    pub fn beta_app_review_detail(&self) -> Option<Arc<BetaAppReviewDetail>> {
        self.inner.beta_app_review_detail()
    }

    /// The App Metadata capability handle, or `None` when this provider does not
    /// expose [`Capability::AppMetadata`]. This is the discovery mechanism: the
    /// host calls `provider.app_metadata()` and gets `None` when app metadata is
    /// unsupported.
    pub fn app_metadata(&self) -> Option<Arc<AppMetadata>> {
        self.inner.app_metadata()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Provider {
    /// Verifies the stored credentials against the live service.
    ///
    /// # Errors
    /// [`StackError::Auth`] when the credentials are rejected (including App Store
    /// Connect "pending agreements"), or a transport/decoding error.
    pub async fn validate(&self) -> Result<(), StackError> {
        self.inner.validate().await
    }

    /// Lists the apps visible to the connected account.
    ///
    /// # Errors
    /// [`StackError::Unsupported`] if the provider lacks [`Capability::Apps`];
    /// otherwise a transport, HTTP, or decoding error.
    pub async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        self.inner.fetch_apps().await
    }
}
