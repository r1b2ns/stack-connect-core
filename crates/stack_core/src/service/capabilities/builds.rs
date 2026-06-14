use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::BuildInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the Builds capability. Kept off the FFI
/// for the same reason as [`crate::service::provider::ProviderImpl`]: UniFFI cannot
/// export an async *trait* cleanly, so the public surface is the concrete
/// [`Builds`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn BuildsImpl>` can live inside an `Arc<Builds>`
/// shared across the tokio runtime.
///
/// Read-only today (list builds) — see RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait BuildsImpl: Send + Sync {
    /// Lists the builds for `app_id`, newest first, up to `limit`.
    async fn fetch_builds(&self, app_id: String, limit: u32) -> Result<Vec<BuildInfo>, StackError>;
}

/// UniFFI-exported Builds capability handle. A thin, binding-friendly wrapper
/// around a boxed [`BuildsImpl`]; async work runs on the tokio runtime. Reached
/// via [`crate::service::provider::Provider::builds`].
#[derive(uniffi::Object)]
pub struct Builds {
    inner: Box<dyn BuildsImpl>,
}

impl Builds {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn BuildsImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Builds {
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
        self.inner.fetch_builds(app_id, limit).await
    }
}
