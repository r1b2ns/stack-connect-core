use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{
    AppStoreLocalizationInfo, AppStoreVersionInfo, PhasedReleaseInfo, ScreenshotSetInfo,
};
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
/// version), plus the version lifecycle writes (submit for review, cancel an
/// in-flight review, manually release an approved version, and reject a
/// submission) and the phased-release writes (fetch, create, delete, and update
/// the staged rollout state of a version) — see RUST_CORE_PLAN.md Phase 2.
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

    /// Submits `version_id` (of `app_id`) for App Store review, optionally
    /// scoping the submission to `platform`.
    async fn submit_for_review(
        &self,
        app_id: String,
        version_id: String,
        platform: Option<String>,
    ) -> Result<(), StackError>;

    /// Cancels the active (waiting-for-review or in-review) submission for
    /// `app_id`, if any.
    async fn cancel_review(&self, app_id: String) -> Result<(), StackError>;

    /// Manually releases the approved version identified by `version_id`.
    async fn release_version(&self, version_id: String) -> Result<(), StackError>;

    /// Rejects the most recent submission for `app_id`, if any.
    async fn reject_version(&self, app_id: String) -> Result<(), StackError>;

    /// Fetches the phased release for `version_id`, or `None` when there is no
    /// phased release.
    async fn fetch_phased_release(
        &self,
        version_id: String,
    ) -> Result<Option<PhasedReleaseInfo>, StackError>;

    /// Creates a phased release for `version_id` with the initial `state`,
    /// returning the created phased release.
    async fn create_phased_release(
        &self,
        version_id: String,
        state: String,
    ) -> Result<PhasedReleaseInfo, StackError>;

    /// Deletes the phased release identified by `id`.
    async fn delete_phased_release(&self, id: String) -> Result<(), StackError>;

    /// Updates the `state` of the phased release identified by `id`, returning
    /// the updated phased release.
    async fn update_phased_release_state(
        &self,
        id: String,
        state: String,
    ) -> Result<PhasedReleaseInfo, StackError>;

    /// Lists the version localizations for `version_id`.
    async fn fetch_localizations(
        &self,
        version_id: String,
    ) -> Result<Vec<AppStoreLocalizationInfo>, StackError>;

    /// Updates the version localization identified by `id`, sending only the
    /// provided attributes.
    // The six independent, optional attributes plus `id` are mirrored verbatim
    // from the App Store Connect API; grouping them into a struct would add a
    // UniFFI-exported type for no semantic gain.
    #[allow(clippy::too_many_arguments)]
    async fn update_localization(
        &self,
        id: String,
        description: Option<String>,
        keywords: Option<String>,
        promotional_text: Option<String>,
        support_url: Option<String>,
        marketing_url: Option<String>,
        whats_new: Option<String>,
    ) -> Result<(), StackError>;

    /// Lists the screenshot sets (with their screenshots) for the version
    /// localization identified by `localization_id`.
    async fn fetch_screenshot_sets(
        &self,
        localization_id: String,
    ) -> Result<Vec<ScreenshotSetInfo>, StackError>;
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

    /// Submits the version `version_id` of `app_id` for App Store review.
    ///
    /// When `platform` is `Some`, the review submission is scoped to that raw
    /// ASC platform value (`IOS` / `MAC_OS` / `TV_OS` / `VISION_OS`); when
    /// `None`, the submission carries no platform attribute. This drives three
    /// sequential App Store Connect requests (create submission, attach the
    /// version as a submission item, then mark the submission submitted).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn submit_for_review(
        &self,
        app_id: String,
        version_id: String,
        platform: Option<String>,
    ) -> Result<(), StackError> {
        self.inner
            .submit_for_review(app_id, version_id, platform)
            .await
    }

    /// Cancels the active submission for `app_id`.
    ///
    /// Looks up the first submission in the `WAITING_FOR_REVIEW` or `IN_REVIEW`
    /// state for `app_id` and marks it canceled. When no such submission exists
    /// this is a no-op that returns `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn cancel_review(&self, app_id: String) -> Result<(), StackError> {
        self.inner.cancel_review(app_id).await
    }

    /// Manually releases the approved version identified by `version_id`.
    ///
    /// Issues a single App Store Connect release request for the version. Any
    /// 2xx is treated as success.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn release_version(&self, version_id: String) -> Result<(), StackError> {
        self.inner.release_version(version_id).await
    }

    /// Rejects the most recent submission for `app_id`.
    ///
    /// Looks up the first submission for `app_id` (regardless of state) and
    /// marks it canceled. When no submission exists this is a no-op that returns
    /// `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn reject_version(&self, app_id: String) -> Result<(), StackError> {
        self.inner.reject_version(app_id).await
    }

    /// Fetches the phased (staged) release for `version_id`.
    ///
    /// Resolves the singular `appStoreVersionPhasedRelease` relationship of the
    /// version. Returns `Ok(None)` when no phased release exists (the document's
    /// `data` is null/absent, or the relationship endpoint answers 404).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_phased_release(
        &self,
        version_id: String,
    ) -> Result<Option<PhasedReleaseInfo>, StackError> {
        self.inner.fetch_phased_release(version_id).await
    }

    /// Creates a phased (staged) release for `version_id` with the initial
    /// `state`, returning the created phased release. `state` is the raw ASC
    /// `phasedReleaseState` value (`INACTIVE` / `ACTIVE` / `PAUSED` /
    /// `COMPLETE`).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn create_phased_release(
        &self,
        version_id: String,
        state: String,
    ) -> Result<PhasedReleaseInfo, StackError> {
        self.inner.create_phased_release(version_id, state).await
    }

    /// Deletes the phased release identified by `id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_phased_release(&self, id: String) -> Result<(), StackError> {
        self.inner.delete_phased_release(id).await
    }

    /// Updates the `state` of the phased release identified by `id`, returning
    /// the updated phased release. `state` is the raw ASC `phasedReleaseState`
    /// value (`INACTIVE` / `ACTIVE` / `PAUSED` / `COMPLETE`).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn update_phased_release_state(
        &self,
        id: String,
        state: String,
    ) -> Result<PhasedReleaseInfo, StackError> {
        self.inner.update_phased_release_state(id, state).await
    }

    /// Lists the version localizations for `version_id`.
    ///
    /// Resolves the version's `appStoreVersionLocalizations` relationship,
    /// following `links.next` pagination until exhausted. Each localization
    /// carries the per-locale product-page metadata (`description`, `keywords`,
    /// `promotional_text`, `support_url`, `marketing_url`, `whats_new`).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_localizations(
        &self,
        version_id: String,
    ) -> Result<Vec<AppStoreLocalizationInfo>, StackError> {
        self.inner.fetch_localizations(version_id).await
    }

    /// Updates the version localization identified by `id`, sending only the
    /// provided attributes.
    ///
    /// Only the `Some` attributes are sent in the PATCH body; `None` attributes
    /// are omitted entirely (and so left untouched on App Store Connect). Any
    /// 2xx is treated as success.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_localization(
        &self,
        id: String,
        description: Option<String>,
        keywords: Option<String>,
        promotional_text: Option<String>,
        support_url: Option<String>,
        marketing_url: Option<String>,
        whats_new: Option<String>,
    ) -> Result<(), StackError> {
        self.inner
            .update_localization(
                id,
                description,
                keywords,
                promotional_text,
                support_url,
                marketing_url,
                whats_new,
            )
            .await
    }

    /// Lists the screenshot sets (with their screenshots) for the version
    /// localization identified by `localization_id`.
    ///
    /// Resolves the localization's `appScreenshotSets` relationship with the
    /// `appScreenshots` resources included, following `links.next` pagination
    /// until exhausted. Each set carries its `display_type` and its screenshots
    /// (resolved from the JSON:API `included` section, preserving relationship
    /// order). Each screenshot's `image_url` is computed from its `imageAsset`
    /// template exactly as the build icon URL is.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_screenshot_sets(
        &self,
        localization_id: String,
    ) -> Result<Vec<ScreenshotSetInfo>, StackError> {
        self.inner.fetch_screenshot_sets(localization_id).await
    }
}
