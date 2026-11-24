use std::sync::Arc;

use async_trait::async_trait;

use crate::audit::{AuditBinding, AuditTarget};
use crate::domain::BetaAppLocalizationInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the Beta App Localizations (TestFlight
/// app-level "feedback email" + "test description") capability. Kept off the FFI
/// for the same reason as [`crate::service::provider::ProviderImpl`]: UniFFI
/// cannot export an async *trait* cleanly, so the public surface is the concrete
/// [`BetaAppLocalizations`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn BetaAppLocalizationsImpl>` can live inside an
/// `Arc<BetaAppLocalizations>` shared across the tokio runtime.
///
/// Covers reads (list an app's localizations) and writes (create/update a
/// per-locale feedback email + test description) — see RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait BetaAppLocalizationsImpl: Send + Sync {
    /// Lists the beta app localizations for `app_id`, up to `limit`.
    async fn fetch_beta_app_localizations(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<BetaAppLocalizationInfo>, StackError>;

    /// Creates a beta app localization for `app_id` in `locale`, optionally
    /// setting the `feedback_email` and `description`.
    async fn create_beta_app_localization(
        &self,
        app_id: String,
        locale: String,
        feedback_email: Option<String>,
        description: Option<String>,
    ) -> Result<BetaAppLocalizationInfo, StackError>;

    /// Updates the beta app localization `id`, replacing only the provided
    /// `feedback_email` and/or `description` attributes.
    async fn update_beta_app_localization(
        &self,
        id: String,
        feedback_email: Option<String>,
        description: Option<String>,
    ) -> Result<BetaAppLocalizationInfo, StackError>;
}

/// UniFFI-exported Beta App Localizations capability handle. A thin,
/// binding-friendly wrapper around a boxed [`BetaAppLocalizationsImpl`]; async
/// work runs on the tokio runtime. Reached via
/// [`crate::service::provider::Provider::beta_app_localizations`].
#[derive(uniffi::Object)]
pub struct BetaAppLocalizations {
    inner: Box<dyn BetaAppLocalizationsImpl>,
    /// The audit target the provider bound this handle to (see
    /// [`crate::audit`]).
    audit: AuditBinding,
}

impl BetaAppLocalizations {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn BetaAppLocalizationsImpl>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            audit: AuditBinding::default(),
        })
    }

    /// Audits this handle's calls into `target` (no-op for `None`).
    pub(crate) fn bind_audit(&self, target: Option<&AuditTarget>) {
        self.audit.bind(target);
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl BetaAppLocalizations {
    /// Lists the beta app localizations for `app_id`, up to `limit`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_beta_app_localizations(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<BetaAppLocalizationInfo>, StackError> {
        self.audit
            .run(
                "beta_app_localizations.fetch_beta_app_localizations",
                self.inner.fetch_beta_app_localizations(app_id, limit),
            )
            .await
    }

    /// Creates a beta app localization for `app_id` in `locale`, returning the
    /// created localization. `feedback_email` is the per-locale address testers'
    /// feedback is sent to, and `description` is the per-locale TestFlight test
    /// description shown to testers; both are optional.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_beta_app_localization(
        &self,
        app_id: String,
        locale: String,
        feedback_email: Option<String>,
        description: Option<String>,
    ) -> Result<BetaAppLocalizationInfo, StackError> {
        self.audit
            .run(
                "beta_app_localizations.create_beta_app_localization",
                self.inner.create_beta_app_localization(
                    app_id,
                    locale,
                    feedback_email,
                    description,
                ),
            )
            .await
    }

    /// Updates the beta app localization `id`, replacing only the provided
    /// `feedback_email` and/or `description` attributes, and returns the updated
    /// localization.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn update_beta_app_localization(
        &self,
        id: String,
        feedback_email: Option<String>,
        description: Option<String>,
    ) -> Result<BetaAppLocalizationInfo, StackError> {
        self.audit
            .run(
                "beta_app_localizations.update_beta_app_localization",
                self.inner
                    .update_beta_app_localization(id, feedback_email, description),
            )
            .await
    }
}
