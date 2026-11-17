use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::StoreListingInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the Store Listings capability (an app's
/// localized store listings). Kept off the FFI for the same reason as
/// [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an async
/// *trait* cleanly, so the public surface is the concrete [`StoreListings`]
/// object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn StoreListingsImpl>` can live inside an
/// `Arc<StoreListings>` shared across the tokio runtime.
///
/// Read-only today; Google Play is the only provider exposing it.
#[async_trait]
pub(crate) trait StoreListingsImpl: Send + Sync {
    /// Lists every localized store listing of `app_id`.
    async fn fetch_store_listings(
        &self,
        app_id: String,
    ) -> Result<Vec<StoreListingInfo>, StackError>;
}

/// UniFFI-exported Store Listings capability handle. A thin, binding-friendly
/// wrapper around a boxed [`StoreListingsImpl`]; async work runs on the tokio
/// runtime. Reached via [`crate::service::provider::Provider::store_listings`].
#[derive(uniffi::Object)]
pub struct StoreListings {
    inner: Box<dyn StoreListingsImpl>,
}

impl StoreListings {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn StoreListingsImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl StoreListings {
    /// Lists every localized store listing of `app_id` (for Google Play, the
    /// package name, i.e. `AppInfo.id`), in the store's order.
    ///
    /// Google Play reads them through a throwaway edit (insert, read, delete;
    /// nothing is committed). Google keeps one open edit per service account
    /// and app, so this invalidates any edit the same service account has open
    /// for the app elsewhere (e.g. a CI upload in progress).
    ///
    /// # Errors
    /// [`StackError::Auth`] when Google denies access or the API is disabled,
    /// [`StackError::Http`] on any other non-2xx response (`404`: app not found),
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_store_listings(
        &self,
        app_id: String,
    ) -> Result<Vec<StoreListingInfo>, StackError> {
        self.inner.fetch_store_listings(app_id).await
    }
}
