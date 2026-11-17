use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::AppDetailsInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the App Details capability (an app's
/// store contact details and default language). Kept off the FFI for the same
/// reason as [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an
/// async *trait* cleanly, so the public surface is the concrete [`AppDetails`]
/// object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn AppDetailsImpl>` can live inside an
/// `Arc<AppDetails>` shared across the tokio runtime.
///
/// Read-only today; Google Play is the only provider exposing it.
#[async_trait]
pub(crate) trait AppDetailsImpl: Send + Sync {
    /// Reads the store details of `app_id`.
    async fn fetch_app_details(&self, app_id: String) -> Result<AppDetailsInfo, StackError>;
}

/// UniFFI-exported App Details capability handle. A thin, binding-friendly
/// wrapper around a boxed [`AppDetailsImpl`]; async work runs on the tokio
/// runtime. Reached via [`crate::service::provider::Provider::app_details`].
#[derive(uniffi::Object)]
pub struct AppDetails {
    inner: Box<dyn AppDetailsImpl>,
}

impl AppDetails {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn AppDetailsImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl AppDetails {
    /// Reads the store details of `app_id` (for Google Play, the package name,
    /// i.e. `AppInfo.id`): default language and contact email, phone and
    /// website.
    ///
    /// This call also answers "can this account reach the app?" (e.g. before
    /// adding an app by hand): `Ok` means yes; [`StackError::Http`] with status
    /// `404` means there is no such app (an invalid package name is reported
    /// the same way, without contacting Google); [`StackError::Auth`] means the
    /// app exists or may exist but the account has no access to it, or the API
    /// is not enabled.
    ///
    /// Google Play reads it through a throwaway edit (insert, read, delete;
    /// nothing is committed). Google keeps one open edit per service account
    /// and app, so this invalidates any edit the same service account has open
    /// for the app elsewhere (e.g. a CI upload in progress).
    ///
    /// # Errors
    /// [`StackError::Auth`] when Google denies access or the API is disabled,
    /// [`StackError::Http`] on any other non-2xx response (`404`: app not found),
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_app_details(&self, app_id: String) -> Result<AppDetailsInfo, StackError> {
        self.inner.fetch_app_details(app_id).await
    }
}
