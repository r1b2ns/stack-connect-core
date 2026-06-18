use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{BetaGroupInfo, BetaTesterInfo};
use crate::error::StackError;

/// Internal, non-exported contract for the Beta Groups (TestFlight) capability.
/// Kept off the FFI for the same reason as [`crate::service::provider::ProviderImpl`]:
/// UniFFI cannot export an async *trait* cleanly, so the public surface is the
/// concrete [`BetaGroups`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn BetaGroupsImpl>` can live inside an
/// `Arc<BetaGroups>` shared across the tokio runtime.
///
/// Covers reads (list groups, list testers) and writes (create/update/delete a
/// group, add/remove a tester) — see RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait BetaGroupsImpl: Send + Sync {
    /// Lists the beta groups for `app_id`, up to `limit`.
    async fn fetch_beta_groups(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<BetaGroupInfo>, StackError>;

    /// Lists the beta testers belonging to `group_id`, up to `limit`.
    async fn fetch_beta_testers(
        &self,
        group_id: String,
        limit: u32,
    ) -> Result<Vec<BetaTesterInfo>, StackError>;

    /// Creates a beta group named `name` under `app_id`.
    async fn create_beta_group(
        &self,
        app_id: String,
        name: String,
        is_internal: bool,
        public_link_enabled: bool,
        has_access_to_all_builds: bool,
    ) -> Result<BetaGroupInfo, StackError>;

    /// Updates the beta group `group_id`, applying only the provided fields.
    async fn update_beta_group(
        &self,
        group_id: String,
        name: Option<String>,
        public_link_enabled: Option<bool>,
        public_link_limit: Option<i32>,
        feedback_enabled: Option<bool>,
    ) -> Result<BetaGroupInfo, StackError>;

    /// Deletes the beta group `group_id`.
    async fn delete_beta_group(&self, group_id: String) -> Result<(), StackError>;

    /// Adds a beta tester (created from `email` and optional name parts) to
    /// `group_id`.
    async fn add_beta_tester(
        &self,
        group_id: String,
        email: String,
        first_name: Option<String>,
        last_name: Option<String>,
    ) -> Result<BetaTesterInfo, StackError>;

    /// Removes the beta tester `tester_id` from `group_id`.
    async fn remove_beta_tester(
        &self,
        group_id: String,
        tester_id: String,
    ) -> Result<(), StackError>;

    /// Returns the number of beta testers belonging to `group_id`.
    async fn fetch_tester_count(&self, group_id: String) -> Result<u32, StackError>;

    /// Resends the TestFlight invite for `tester_id` on `app_id`.
    async fn resend_invite(&self, tester_id: String, app_id: String) -> Result<(), StackError>;
}

/// UniFFI-exported Beta Groups capability handle. A thin, binding-friendly
/// wrapper around a boxed [`BetaGroupsImpl`]; async work runs on the tokio
/// runtime. Reached via [`crate::service::provider::Provider::beta_groups`].
#[derive(uniffi::Object)]
pub struct BetaGroups {
    inner: Box<dyn BetaGroupsImpl>,
}

impl BetaGroups {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn BetaGroupsImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl BetaGroups {
    /// Lists the beta groups for `app_id`, up to `limit`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_beta_groups(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<BetaGroupInfo>, StackError> {
        self.inner.fetch_beta_groups(app_id, limit).await
    }

    /// Lists the beta testers belonging to `group_id`, up to `limit`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_beta_testers(
        &self,
        group_id: String,
        limit: u32,
    ) -> Result<Vec<BetaTesterInfo>, StackError> {
        self.inner.fetch_beta_testers(group_id, limit).await
    }

    /// Creates a beta group named `name` under `app_id`, returning the created
    /// group. `is_internal` selects an internal vs. external group;
    /// `public_link_enabled` toggles the TestFlight public link; and
    /// `has_access_to_all_builds` grants the group every build. Feedback is
    /// enabled on creation.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_beta_group(
        &self,
        app_id: String,
        name: String,
        is_internal: bool,
        public_link_enabled: bool,
        has_access_to_all_builds: bool,
    ) -> Result<BetaGroupInfo, StackError> {
        self.inner
            .create_beta_group(
                app_id,
                name,
                is_internal,
                public_link_enabled,
                has_access_to_all_builds,
            )
            .await
    }

    /// Updates the beta group `group_id`, applying only the fields that are
    /// `Some` and leaving the rest untouched. `public_link_limit` caps the number
    /// of testers who can join via the public link.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn update_beta_group(
        &self,
        group_id: String,
        name: Option<String>,
        public_link_enabled: Option<bool>,
        public_link_limit: Option<i32>,
        feedback_enabled: Option<bool>,
    ) -> Result<BetaGroupInfo, StackError> {
        self.inner
            .update_beta_group(
                group_id,
                name,
                public_link_enabled,
                public_link_limit,
                feedback_enabled,
            )
            .await
    }

    /// Deletes the beta group `group_id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_beta_group(&self, group_id: String) -> Result<(), StackError> {
        self.inner.delete_beta_group(group_id).await
    }

    /// Adds a beta tester to `group_id`, creating the tester from `email` and the
    /// optional `first_name`/`last_name`, and returns the created tester.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn add_beta_tester(
        &self,
        group_id: String,
        email: String,
        first_name: Option<String>,
        last_name: Option<String>,
    ) -> Result<BetaTesterInfo, StackError> {
        self.inner
            .add_beta_tester(group_id, email, first_name, last_name)
            .await
    }

    /// Removes the beta tester `tester_id` from `group_id` (unlinks the tester
    /// from the group; the tester itself is not deleted).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn remove_beta_tester(
        &self,
        group_id: String,
        tester_id: String,
    ) -> Result<(), StackError> {
        self.inner.remove_beta_tester(group_id, tester_id).await
    }

    /// Returns the number of beta testers belonging to `group_id`. Reads the
    /// total from App Store Connect's paging metadata without materializing the
    /// full tester list.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_tester_count(&self, group_id: String) -> Result<u32, StackError> {
        self.inner.fetch_tester_count(group_id).await
    }

    /// Resends the TestFlight invite for the beta tester `tester_id` on `app_id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn resend_invite(&self, tester_id: String, app_id: String) -> Result<(), StackError> {
        self.inner.resend_invite(tester_id, app_id).await
    }
}
