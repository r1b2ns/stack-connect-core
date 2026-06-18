use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::AppInfoLocalizationInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the App Metadata (App Store product-page
/// metadata) capability. Kept off the FFI for the same reason as
/// [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an async
/// *trait* cleanly, so the public surface is the concrete [`AppMetadata`] object
/// below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn AppMetadataImpl>` can live inside an
/// `Arc<AppMetadata>` shared across the tokio runtime.
///
/// This first batch covers only the per-locale app-info localizations (the
/// product-page `name`/`subtitle` and the three privacy links/text). The
/// capability will grow later with app-info read / categories / age rating —
/// see RUST_CORE_PLAN.md Phase 2.
///
/// Named `AppMetadata` (not `AppInfo`) on purpose: a domain record named
/// [`crate::domain::AppInfo`] already exists, and a UniFFI Object sharing a name
/// with a Record would clash in the generated bindings.
#[async_trait]
pub(crate) trait AppMetadataImpl: Send + Sync {
    /// Lists the app-info localizations for `app_info_id`.
    async fn fetch_app_info_localizations(
        &self,
        app_info_id: String,
    ) -> Result<Vec<AppInfoLocalizationInfo>, StackError>;

    /// Updates the app-info localization `id`, always sending `name` and sending
    /// `subtitle` only when provided.
    async fn update_app_info_localization(
        &self,
        id: String,
        name: String,
        subtitle: Option<String>,
    ) -> Result<AppInfoLocalizationInfo, StackError>;

    /// Updates the privacy attributes of the app-info localization `id`,
    /// replacing only the provided privacy URL/text attributes.
    async fn update_app_info_localization_privacy(
        &self,
        id: String,
        privacy_policy_url: Option<String>,
        privacy_choices_url: Option<String>,
        privacy_policy_text: Option<String>,
    ) -> Result<AppInfoLocalizationInfo, StackError>;

    /// Creates an app-info localization for `app_info_id` in `locale`, always
    /// setting `name` and setting `subtitle` only when provided.
    async fn create_app_info_localization(
        &self,
        app_info_id: String,
        locale: String,
        name: String,
        subtitle: Option<String>,
    ) -> Result<AppInfoLocalizationInfo, StackError>;

    /// Deletes the app-info localization `id`.
    async fn delete_app_info_localization(&self, id: String) -> Result<(), StackError>;
}

/// UniFFI-exported App Metadata capability handle. A thin, binding-friendly
/// wrapper around a boxed [`AppMetadataImpl`]; async work runs on the tokio
/// runtime. Reached via [`crate::service::provider::Provider::app_metadata`].
#[derive(uniffi::Object)]
pub struct AppMetadata {
    inner: Box<dyn AppMetadataImpl>,
}

impl AppMetadata {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn AppMetadataImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl AppMetadata {
    /// Lists the app-info localizations for `app_info_id`. Each carries the
    /// per-locale product-page `name`/`subtitle` and the three privacy
    /// links/text.
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
        self.inner.fetch_app_info_localizations(app_info_id).await
    }

    /// Updates the app-info localization `id`, returning the updated
    /// localization. `name` is always sent; `subtitle` is sent only when `Some`.
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
        self.inner
            .update_app_info_localization(id, name, subtitle)
            .await
    }

    /// Updates the privacy attributes of the app-info localization `id`,
    /// replacing only the provided `privacy_policy_url`, `privacy_choices_url`,
    /// and/or `privacy_policy_text` attributes, and returns the updated
    /// localization.
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
        self.inner
            .update_app_info_localization_privacy(
                id,
                privacy_policy_url,
                privacy_choices_url,
                privacy_policy_text,
            )
            .await
    }

    /// Creates an app-info localization for `app_info_id` in `locale`, returning
    /// the created localization. `name` is always set; `subtitle` is set only
    /// when `Some`.
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
        self.inner
            .create_app_info_localization(app_info_id, locale, name, subtitle)
            .await
    }

    /// Deletes the app-info localization `id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_app_info_localization(&self, id: String) -> Result<(), StackError> {
        self.inner.delete_app_info_localization(id).await
    }
}
