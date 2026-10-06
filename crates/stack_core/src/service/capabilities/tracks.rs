use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::TrackInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the Tracks capability (an app's release
/// tracks and the releases on them). Kept off the FFI for the same reason as
/// [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an async
/// *trait* cleanly, so the public surface is the concrete [`Tracks`] object
/// below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn TracksImpl>` can live inside an `Arc<Tracks>`
/// shared across the tokio runtime.
///
/// Read-only today; Google Play is the only provider exposing it.
#[async_trait]
pub(crate) trait TracksImpl: Send + Sync {
    /// Lists every release track of `app_id` with its releases.
    async fn fetch_tracks(&self, app_id: String) -> Result<Vec<TrackInfo>, StackError>;
}

/// UniFFI-exported Tracks capability handle. A thin, binding-friendly wrapper
/// around a boxed [`TracksImpl`]; async work runs on the tokio runtime. Reached
/// via [`crate::service::provider::Provider::tracks`].
#[derive(uniffi::Object)]
pub struct Tracks {
    inner: Box<dyn TracksImpl>,
}

impl Tracks {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn TracksImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Tracks {
    /// Lists every release track of `app_id` (for Google Play, the package
    /// name, i.e. `AppInfo.id`) with its releases, in the store's order. Tracks
    /// without a release are included with an empty `releases`.
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
    pub async fn fetch_tracks(&self, app_id: String) -> Result<Vec<TrackInfo>, StackError> {
        self.inner.fetch_tracks(app_id).await
    }
}
