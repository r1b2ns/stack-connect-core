use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::AccessibilityDeclarationInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the Accessibility Declarations capability
/// (an app's per-device-family accessibility feature declarations). Kept off the
/// FFI for the same reason as [`crate::service::provider::ProviderImpl`]: UniFFI
/// cannot export an async *trait* cleanly, so the public surface is the concrete
/// [`AccessibilityDeclarations`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn AccessibilityDeclarationsImpl>` can live inside an
/// `Arc<AccessibilityDeclarations>` shared across the tokio runtime.
///
/// Covers reads (list an app's declarations) and writes (create a declaration for
/// a device family, update its supported features and optionally publish it, and
/// delete it) — see RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait AccessibilityDeclarationsImpl: Send + Sync {
    /// Lists the accessibility declarations for `app_id`, up to `limit` per page.
    async fn fetch_accessibility_declarations(
        &self,
        app_id: String,
        limit: i64,
    ) -> Result<Vec<AccessibilityDeclarationInfo>, StackError>;

    /// Creates an accessibility declaration for `app_id` targeting
    /// `device_family`.
    async fn create_accessibility_declaration(
        &self,
        app_id: String,
        device_family: String,
    ) -> Result<AccessibilityDeclarationInfo, StackError>;

    /// Updates the accessibility declaration `id`, setting all nine supported
    /// feature flags and optionally publishing it.
    #[allow(clippy::too_many_arguments)]
    async fn update_accessibility_declaration(
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
    ) -> Result<AccessibilityDeclarationInfo, StackError>;

    /// Deletes the accessibility declaration `id`.
    async fn delete_accessibility_declaration(&self, id: String) -> Result<(), StackError>;
}

/// UniFFI-exported Accessibility Declarations capability handle. A thin,
/// binding-friendly wrapper around a boxed [`AccessibilityDeclarationsImpl`];
/// async work runs on the tokio runtime. Reached via
/// [`crate::service::provider::Provider::accessibility_declarations`].
#[derive(uniffi::Object)]
pub struct AccessibilityDeclarations {
    inner: Box<dyn AccessibilityDeclarationsImpl>,
}

impl AccessibilityDeclarations {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn AccessibilityDeclarationsImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl AccessibilityDeclarations {
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
        self.inner
            .fetch_accessibility_declarations(app_id, limit)
            .await
    }

    /// Creates an accessibility declaration for `app_id` targeting
    /// `device_family` (an App Store Connect device-family value such as
    /// `IPHONE`, `IPAD`, `APPLE_TV`, `APPLE_WATCH`, `MAC`, or `VISION`), returning
    /// the created declaration. The core forwards `device_family` verbatim; App
    /// Store Connect rejects unknown values with an HTTP error.
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
        self.inner
            .create_accessibility_declaration(app_id, device_family)
            .await
    }

    /// Updates the accessibility declaration `id`, setting all nine supported
    /// feature flags and, when `publish` is `true`, publishing the declaration
    /// (the `publish` attribute is omitted entirely when `publish` is `false`).
    /// Returns the updated declaration.
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
        self.inner
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
            .await
    }

    /// Deletes the accessibility declaration `id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_accessibility_declaration(&self, id: String) -> Result<(), StackError> {
        self.inner.delete_accessibility_declaration(id).await
    }
}
