use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{AppCategoryInfo, AppInfoDetails, AppInfoLocalizationInfo};
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
/// Beyond the per-locale app-info localizations (the product-page
/// `name`/`subtitle` and the three privacy links/text), this capability covers
/// App Info read (ids + category/age-rating wiring merged with the app's
/// `sku`/`primaryLocale`/`contentRightsDeclaration`), the App Store category
/// list, category / app / age-rating updates, and icon-URL resolution from the
/// latest build.
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

    /// Fetches the full App Info detail for `app_id`, merging the app-info
    /// resource (ids, categories, age rating, localizations) with the owning
    /// app's `sku`/`primary_locale`/`content_rights_declaration`.
    async fn fetch_app_info(&self, app_id: String) -> Result<AppInfoDetails, StackError>;

    /// Lists the top-level App Store categories (iOS), each with its
    /// subcategory ids.
    async fn fetch_app_categories(&self) -> Result<Vec<AppCategoryInfo>, StackError>;

    /// Updates the category relationships of the app-info `app_info_id`, wiring
    /// only the relationships whose id is `Some` and omitting the rest.
    async fn update_app_info_category(
        &self,
        app_info_id: String,
        primary_category_id: Option<String>,
        subcategory_one_id: Option<String>,
        secondary_category_id: Option<String>,
        secondary_subcategory_one_id: Option<String>,
    ) -> Result<(), StackError>;

    /// Updates the app `id`, sending `content_rights_declaration` and/or
    /// `primary_locale` only when `Some`.
    async fn update_app(
        &self,
        id: String,
        content_rights_declaration: Option<String>,
        primary_locale: Option<String>,
    ) -> Result<(), StackError>;

    /// Updates the age-rating declaration `id`. All 18 attributes are required
    /// from the host and are always sent.
    #[allow(clippy::too_many_arguments)]
    async fn update_age_rating(
        &self,
        id: String,
        alcohol_tobacco: String,
        contests: String,
        gambling_simulated: String,
        guns_or_other_weapons: String,
        medical_information: String,
        profanity: String,
        sexual_content_graphic: String,
        sexual_content_or_nudity: String,
        horror_or_fear: String,
        mature_or_suggestive: String,
        violence_cartoon: String,
        violence_realistic: String,
        violence_graphic: String,
        is_advertising: bool,
        is_gambling: bool,
        is_unrestricted_web_access: bool,
        is_user_generated_content: bool,
        age_rating_override: String,
    ) -> Result<(), StackError>;

    /// Resolves the icon URL for `app_id` from its most recent build, or `None`
    /// when there is no build / no icon token.
    async fn fetch_icon_url(&self, app_id: String) -> Result<Option<String>, StackError>;
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

    /// Fetches the full App Info detail for `app_id`: the app-info ids,
    /// category/age-rating wiring, and per-locale localizations, merged with the
    /// owning app's `sku`/`primary_locale`/`content_rights_declaration`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_app_info(&self, app_id: String) -> Result<AppInfoDetails, StackError> {
        self.inner.fetch_app_info(app_id).await
    }

    /// Lists the top-level App Store categories (iOS), each with the ids of its
    /// subcategories.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_app_categories(&self) -> Result<Vec<AppCategoryInfo>, StackError> {
        self.inner.fetch_app_categories().await
    }

    /// Updates the category relationships of the app-info `app_info_id`. Each of
    /// `primary_category_id`, `subcategory_one_id`, `secondary_category_id`, and
    /// `secondary_subcategory_one_id` is wired only when `Some`; the others are
    /// omitted (not sent as `null`).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn update_app_info_category(
        &self,
        app_info_id: String,
        primary_category_id: Option<String>,
        subcategory_one_id: Option<String>,
        secondary_category_id: Option<String>,
        secondary_subcategory_one_id: Option<String>,
    ) -> Result<(), StackError> {
        self.inner
            .update_app_info_category(
                app_info_id,
                primary_category_id,
                subcategory_one_id,
                secondary_category_id,
                secondary_subcategory_one_id,
            )
            .await
    }

    /// Updates the app `id`, sending `content_rights_declaration` and/or
    /// `primary_locale` only when `Some`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn update_app(
        &self,
        id: String,
        content_rights_declaration: Option<String>,
        primary_locale: Option<String>,
    ) -> Result<(), StackError> {
        self.inner
            .update_app(id, content_rights_declaration, primary_locale)
            .await
    }

    /// Updates the age-rating declaration `id`, sending all 18 attributes (all
    /// required from the host).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_age_rating(
        &self,
        id: String,
        alcohol_tobacco: String,
        contests: String,
        gambling_simulated: String,
        guns_or_other_weapons: String,
        medical_information: String,
        profanity: String,
        sexual_content_graphic: String,
        sexual_content_or_nudity: String,
        horror_or_fear: String,
        mature_or_suggestive: String,
        violence_cartoon: String,
        violence_realistic: String,
        violence_graphic: String,
        is_advertising: bool,
        is_gambling: bool,
        is_unrestricted_web_access: bool,
        is_user_generated_content: bool,
        age_rating_override: String,
    ) -> Result<(), StackError> {
        self.inner
            .update_age_rating(
                id,
                alcohol_tobacco,
                contests,
                gambling_simulated,
                guns_or_other_weapons,
                medical_information,
                profanity,
                sexual_content_graphic,
                sexual_content_or_nudity,
                horror_or_fear,
                mature_or_suggestive,
                violence_cartoon,
                violence_realistic,
                violence_graphic,
                is_advertising,
                is_gambling,
                is_unrestricted_web_access,
                is_user_generated_content,
                age_rating_override,
            )
            .await
    }

    /// Resolves the icon URL for `app_id` from its most recent build, or `None`
    /// when there is no build / no icon token.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_icon_url(&self, app_id: String) -> Result<Option<String>, StackError> {
        self.inner.fetch_icon_url(app_id).await
    }
}
