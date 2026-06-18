use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{BuildDetailInfo, BuildInfo, BuildsPage};
use crate::error::StackError;

/// Internal, non-exported contract for the Builds capability. Kept off the FFI
/// for the same reason as [`crate::service::provider::ProviderImpl`]: UniFFI cannot
/// export an async *trait* cleanly, so the public surface is the concrete
/// [`Builds`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn BuildsImpl>` can live inside an `Arc<Builds>`
/// shared across the tokio runtime.
///
/// Covers reads (list builds) and writes (expire a build, attach a build to a
/// version, submit a build for beta review, and add/remove a build to/from beta
/// groups) — see RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait BuildsImpl: Send + Sync {
    /// Lists the builds for `app_id`, newest first, up to `limit`.
    async fn fetch_builds(&self, app_id: String, limit: u32) -> Result<Vec<BuildInfo>, StackError>;

    /// Fetches a single page of builds for `app_id`, optionally filtered by
    /// `platform` and `processing_states`, newest first, up to `limit`, returning
    /// an opaque `next_token` for load-more paging.
    async fn fetch_builds_page(
        &self,
        app_id: String,
        platform: Option<String>,
        processing_states: Vec<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<BuildsPage, StackError>;

    /// Lists the builds belonging to the beta group `group_id`, newest first, up
    /// to `limit`, following pagination to the end.
    async fn fetch_builds_for_group(
        &self,
        group_id: String,
        limit: u32,
    ) -> Result<Vec<BuildInfo>, StackError>;

    /// Fetches the full detail of the build `build_id` — the enriched build plus
    /// its beta groups and "What to Test" localizations.
    async fn fetch_build_detail(&self, build_id: String) -> Result<BuildDetailInfo, StackError>;

    /// Fetches the build currently attached to the App Store version `version_id`,
    /// or `None` when no build is attached.
    async fn fetch_current_build(
        &self,
        version_id: String,
    ) -> Result<Option<BuildInfo>, StackError>;

    /// Marks the build `build_id` as expired.
    async fn expire_build(&self, build_id: String) -> Result<(), StackError>;

    /// Attaches the build `build_id` to the App Store version `version_id`.
    async fn attach_build(&self, version_id: String, build_id: String) -> Result<(), StackError>;

    /// Submits the build `build_id` for beta (TestFlight) review.
    async fn submit_build_for_beta_review(&self, build_id: String) -> Result<(), StackError>;

    /// Adds the build `build_id` to each beta group in `group_ids`.
    async fn add_build_to_groups(
        &self,
        build_id: String,
        group_ids: Vec<String>,
    ) -> Result<(), StackError>;

    /// Removes the build `build_id` from the beta group `group_id`.
    async fn remove_build_from_group(
        &self,
        build_id: String,
        group_id: String,
    ) -> Result<(), StackError>;
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

    /// Fetches a single page of builds for `app_id`, newest first (by upload
    /// date), up to `limit`. When `platform` is `Some`, only builds for that
    /// platform are returned; when `processing_states` is non-empty, only builds
    /// in those states are returned. Pass a prior call's `next_token` back as
    /// `page_token` to load the next page.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_builds_page(
        &self,
        app_id: String,
        platform: Option<String>,
        processing_states: Vec<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<BuildsPage, StackError> {
        self.inner
            .fetch_builds_page(app_id, platform, processing_states, limit, page_token)
            .await
    }

    /// Lists the builds belonging to the beta group `group_id`, newest first (by
    /// upload date), up to `limit`, following pagination to the end.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_builds_for_group(
        &self,
        group_id: String,
        limit: u32,
    ) -> Result<Vec<BuildInfo>, StackError> {
        self.inner.fetch_builds_for_group(group_id, limit).await
    }

    /// Fetches the full detail of the build `build_id`: the enriched build plus
    /// its associated beta groups and per-locale "What to Test" localizations.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_build_detail(
        &self,
        build_id: String,
    ) -> Result<BuildDetailInfo, StackError> {
        self.inner.fetch_build_detail(build_id).await
    }

    /// Fetches the build currently attached to the App Store version `version_id`,
    /// or `None` when no build is attached.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_current_build(
        &self,
        version_id: String,
    ) -> Result<Option<BuildInfo>, StackError> {
        self.inner.fetch_current_build(version_id).await
    }

    /// Marks the build `build_id` as expired (sets its `expired` attribute to
    /// `true`).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn expire_build(&self, build_id: String) -> Result<(), StackError> {
        self.inner.expire_build(build_id).await
    }

    /// Attaches the build `build_id` to the App Store version `version_id` (sets
    /// the version's `build` to-one relationship).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn attach_build(
        &self,
        version_id: String,
        build_id: String,
    ) -> Result<(), StackError> {
        self.inner.attach_build(version_id, build_id).await
    }

    /// Submits the build `build_id` for beta (TestFlight) review by creating a
    /// beta app review submission.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn submit_build_for_beta_review(&self, build_id: String) -> Result<(), StackError> {
        self.inner.submit_build_for_beta_review(build_id).await
    }

    /// Adds the build `build_id` to each beta group in `group_ids` (appends to the
    /// build's `betaGroups` to-many relationship). An empty `group_ids` issues the
    /// request with an empty linkage array.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn add_build_to_groups(
        &self,
        build_id: String,
        group_ids: Vec<String>,
    ) -> Result<(), StackError> {
        self.inner.add_build_to_groups(build_id, group_ids).await
    }

    /// Removes the build `build_id` from the beta group `group_id` (deletes the
    /// build from the group's `builds` to-many relationship).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn remove_build_from_group(
        &self,
        build_id: String,
        group_id: String,
    ) -> Result<(), StackError> {
        self.inner.remove_build_from_group(build_id, group_id).await
    }
}
