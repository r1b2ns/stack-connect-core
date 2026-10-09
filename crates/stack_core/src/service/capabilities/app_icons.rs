use std::sync::Arc;

use async_trait::async_trait;

use crate::error::StackError;

/// Internal, non-exported contract for the App Icons capability (where to load
/// an app's icon from). Kept off the FFI for the same reason as
/// [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an async
/// *trait* cleanly, so the public surface is the concrete [`AppIcons`] object
/// below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn AppIconsImpl>` can live inside an
/// `Arc<AppIcons>` shared across the tokio runtime.
///
/// Store-agnostic on purpose: every provider that can resolve an icon exposes
/// it here, so a host renders icons through one call whatever the service.
#[async_trait]
pub(crate) trait AppIconsImpl: Send + Sync {
    /// Resolves an icon URL for `app_id`, or `None` when the store has none to
    /// offer.
    async fn fetch_icon_url(&self, app_id: String) -> Result<Option<String>, StackError>;
}

/// UniFFI-exported App Icons capability handle. A thin, binding-friendly
/// wrapper around a boxed [`AppIconsImpl`]; async work runs on the tokio
/// runtime. Reached via [`crate::service::provider::Provider::app_icons`].
#[derive(uniffi::Object)]
pub struct AppIcons {
    inner: Box<dyn AppIconsImpl>,
}

impl AppIcons {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn AppIconsImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl AppIcons {
    /// Resolves a URL for the square icon of `app_id` (`AppInfo.id`), or `None`
    /// when the store has no icon to offer for it.
    ///
    /// - **App Store Connect:** computed from the `iconAssetToken` of the app's
    ///   most recent build (the same lookup as `AppMetadata::fetch_icon_url`),
    ///   at the size the asset declares (512 px when it declares none); `None`
    ///   when the app has no build or the build has no icon.
    /// - **Google Play** (`app_id` is the package name): read from the app's
    ///   **public** store page (`play.google.com/store/apps/details`), sized to
    ///   512 px, with no credentials and no Android Publisher call. Opening an
    ///   edit to list the app's images would invalidate any edit the same
    ///   service account has open (e.g. a CI upload), so icons never go through
    ///   edits. `None` when the app is not visible in the store (unpublished,
    ///   unknown, or an invalid package name, which is answered without a
    ///   request) or when the page has no usable icon (e.g. a consent page
    ///   served instead).
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx answer that does not mean "no such app"
    /// (e.g. `429`, `5xx`), [`StackError::Network`] on transport failure. App
    /// Store Connect can also return [`StackError::PendingAgreements`] and
    /// [`StackError::Decode`] (malformed JSON).
    pub async fn fetch_icon_url(&self, app_id: String) -> Result<Option<String>, StackError> {
        self.inner.fetch_icon_url(app_id).await
    }
}
