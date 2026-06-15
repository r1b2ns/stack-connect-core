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
/// Read-only today (list groups, list testers) — see RUST_CORE_PLAN.md Phase 2.
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
}
