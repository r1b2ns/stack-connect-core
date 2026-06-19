use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{BundleIdCapabilityInfo, BundleIdInfo};
use crate::error::StackError;

/// Internal, non-exported contract for the BundleIds (App Store Connect bundle
/// IDs / App IDs and their enabled capabilities) capability. Kept off the FFI for
/// the same reason as [`crate::service::provider::ProviderImpl`]: UniFFI cannot
/// export an async *trait* cleanly, so the public surface is the concrete
/// [`BundleIds`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn BundleIdsImpl>` can live inside an
/// `Arc<BundleIds>` shared across the tokio runtime.
///
/// Covers bundle ID reads/writes (list, create, rename, delete) plus the
/// per-bundle capability sub-collection (list, enable, disable) — see
/// RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait BundleIdsImpl: Send + Sync {
    /// Lists every bundle ID of the connected account, sorted by name.
    async fn fetch_bundle_ids(&self) -> Result<Vec<BundleIdInfo>, StackError>;

    /// Registers a new bundle ID with `identifier`, `name`, and ASC `platform`.
    async fn create_bundle_id(
        &self,
        identifier: String,
        name: String,
        platform: String,
    ) -> Result<BundleIdInfo, StackError>;

    /// Renames the bundle ID `id` (only `name` is mutable).
    async fn update_bundle_id(&self, id: String, name: String) -> Result<(), StackError>;

    /// Deletes the bundle ID `id`.
    async fn delete_bundle_id(&self, id: String) -> Result<(), StackError>;

    /// Lists the capabilities enabled on `bundle_id`.
    async fn fetch_bundle_id_capabilities(
        &self,
        bundle_id: String,
    ) -> Result<Vec<BundleIdCapabilityInfo>, StackError>;

    /// Enables `capability_type` on `bundle_id`, returning the created capability.
    async fn enable_capability(
        &self,
        bundle_id: String,
        capability_type: String,
    ) -> Result<BundleIdCapabilityInfo, StackError>;

    /// Disables the capability `capability_id`.
    async fn disable_capability(&self, capability_id: String) -> Result<(), StackError>;
}

/// UniFFI-exported BundleIds capability handle. A thin, binding-friendly wrapper
/// around a boxed [`BundleIdsImpl`]; async work runs on the tokio runtime.
/// Reached via [`crate::service::provider::Provider::bundle_ids`].
#[derive(uniffi::Object)]
pub struct BundleIds {
    inner: Box<dyn BundleIdsImpl>,
}

impl BundleIds {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn BundleIdsImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl BundleIds {
    /// Lists every bundle ID of the connected account, sorted by name, following
    /// pagination until exhausted.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_bundle_ids(&self) -> Result<Vec<BundleIdInfo>, StackError> {
        self.inner.fetch_bundle_ids().await
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
        self.inner
            .create_bundle_id(identifier, name, platform)
            .await
    }

    /// Renames the bundle ID `id`. Only the `name` is mutable; the identifier and
    /// platform are fixed at creation. Any 2xx → `Ok(())` (the response is
    /// discarded).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn update_bundle_id(&self, id: String, name: String) -> Result<(), StackError> {
        self.inner.update_bundle_id(id, name).await
    }

    /// Deletes the bundle ID `id`. Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_bundle_id(&self, id: String) -> Result<(), StackError> {
        self.inner.delete_bundle_id(id).await
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
        self.inner.fetch_bundle_id_capabilities(bundle_id).await
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
        self.inner
            .enable_capability(bundle_id, capability_type)
            .await
    }

    /// Disables the capability `capability_id`. Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn disable_capability(&self, capability_id: String) -> Result<(), StackError> {
        self.inner.disable_capability(capability_id).await
    }
}
