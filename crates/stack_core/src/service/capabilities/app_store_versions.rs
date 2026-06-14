use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::AppStoreVersionInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the App Store Versions capability. Kept off
/// the FFI for the same reason as [`crate::service::provider::ProviderImpl`]: UniFFI
/// cannot export an async *trait* cleanly, so the public surface is the concrete
/// [`AppStoreVersions`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn AppStoreVersionsImpl>` can live inside an
/// `Arc<AppStoreVersions>` shared across the tokio runtime.
///
/// Covers both reads (list versions) and writes (create, update, delete a
/// version) — see RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait AppStoreVersionsImpl: Send + Sync {
    /// Lists the App Store versions for `app_id`, up to `limit`.
    async fn fetch_versions(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<AppStoreVersionInfo>, StackError>;

    /// Creates a new App Store version for `app_id` on `platform` with
    /// `version_string`, returning the created version.
    async fn create_version(
        &self,
        app_id: String,
        platform: String,
        version_string: String,
    ) -> Result<AppStoreVersionInfo, StackError>;

    /// Updates the version identified by `id`, sending only the provided
    /// attributes.
    async fn update_version(
        &self,
        id: String,
        version_string: Option<String>,
        copyright: Option<String>,
        release_type: Option<String>,
        earliest_release_date: Option<String>,
    ) -> Result<(), StackError>;

    /// Deletes the version identified by `id`.
    async fn delete_version(&self, id: String) -> Result<(), StackError>;
}

/// UniFFI-exported App Store Versions capability handle. A thin, binding-friendly
/// wrapper around a boxed [`AppStoreVersionsImpl`]; async work runs on the tokio
/// runtime. Reached via [`crate::service::provider::Provider::app_store_versions`].
#[derive(uniffi::Object)]
pub struct AppStoreVersions {
    inner: Box<dyn AppStoreVersionsImpl>,
}

impl AppStoreVersions {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn AppStoreVersionsImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl AppStoreVersions {
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
        self.inner.fetch_versions(app_id, limit).await
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
        self.inner
            .create_version(app_id, platform, version_string)
            .await
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
        self.inner
            .update_version(
                id,
                version_string,
                copyright,
                release_type,
                earliest_release_date,
            )
            .await
    }

    /// Deletes the version identified by `id`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response or [`StackError::Network`] on
    /// transport failure.
    pub async fn delete_version(&self, id: String) -> Result<(), StackError> {
        self.inner.delete_version(id).await
    }
}
