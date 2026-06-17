use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::BetaAppReviewDetailInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the Beta App Review Detail (TestFlight
/// "Test Information": beta review contact + demo account details) capability.
/// Kept off the FFI for the same reason as
/// [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an async
/// *trait* cleanly, so the public surface is the concrete
/// [`BetaAppReviewDetail`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn BetaAppReviewDetailImpl>` can live inside an
/// `Arc<BetaAppReviewDetail>` shared across the tokio runtime.
///
/// Covers reads (fetch an app's single beta review detail) and writes (update
/// the contact/demo-account/notes attributes) — see RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait BetaAppReviewDetailImpl: Send + Sync {
    /// Fetches the single beta app review detail for `app_id`.
    async fn fetch_beta_app_review_detail(
        &self,
        app_id: String,
    ) -> Result<BetaAppReviewDetailInfo, StackError>;

    /// Updates the beta app review detail `detail_id`, replacing only the
    /// provided attributes.
    #[allow(clippy::too_many_arguments)]
    async fn update_beta_app_review_detail(
        &self,
        detail_id: String,
        contact_first_name: Option<String>,
        contact_last_name: Option<String>,
        contact_email: Option<String>,
        contact_phone: Option<String>,
        demo_account_name: Option<String>,
        demo_account_password: Option<String>,
        is_demo_account_required: Option<bool>,
        notes: Option<String>,
    ) -> Result<BetaAppReviewDetailInfo, StackError>;
}

/// UniFFI-exported Beta App Review Detail capability handle. A thin,
/// binding-friendly wrapper around a boxed [`BetaAppReviewDetailImpl`]; async
/// work runs on the tokio runtime. Reached via
/// [`crate::service::provider::Provider::beta_app_review_detail`].
#[derive(uniffi::Object)]
pub struct BetaAppReviewDetail {
    inner: Box<dyn BetaAppReviewDetailImpl>,
}

impl BetaAppReviewDetail {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn BetaAppReviewDetailImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl BetaAppReviewDetail {
    /// Fetches the single beta app review detail for `app_id` — the TestFlight
    /// "Test Information" containing the beta review contact and optional demo
    /// account credentials.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_beta_app_review_detail(
        &self,
        app_id: String,
    ) -> Result<BetaAppReviewDetailInfo, StackError> {
        self.inner.fetch_beta_app_review_detail(app_id).await
    }

    /// Updates the beta app review detail `detail_id`, replacing only the
    /// provided attributes, and returns the updated detail. Every attribute is
    /// optional: `contact_*` set the beta review contact, `demo_account_*` set
    /// the demo account credentials, `is_demo_account_required` toggles whether a
    /// demo account is needed, and `notes` are the reviewer notes.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_beta_app_review_detail(
        &self,
        detail_id: String,
        contact_first_name: Option<String>,
        contact_last_name: Option<String>,
        contact_email: Option<String>,
        contact_phone: Option<String>,
        demo_account_name: Option<String>,
        demo_account_password: Option<String>,
        is_demo_account_required: Option<bool>,
        notes: Option<String>,
    ) -> Result<BetaAppReviewDetailInfo, StackError> {
        self.inner
            .update_beta_app_review_detail(
                detail_id,
                contact_first_name,
                contact_last_name,
                contact_email,
                contact_phone,
                demo_account_name,
                demo_account_password,
                is_demo_account_required,
                notes,
            )
            .await
    }
}
