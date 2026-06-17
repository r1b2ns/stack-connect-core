use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::BetaBuildLocalizationInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the Beta Build Localizations (TestFlight
/// "What to Test") capability. Kept off the FFI for the same reason as
/// [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an async
/// *trait* cleanly, so the public surface is the concrete
/// [`BetaBuildLocalizations`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn BetaBuildLocalizationsImpl>` can live inside an
/// `Arc<BetaBuildLocalizations>` shared across the tokio runtime.
///
/// Covers reads (list a build's localizations) and writes (create/update a
/// per-locale "What to Test" entry) — see RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait BetaBuildLocalizationsImpl: Send + Sync {
    /// Lists the beta build localizations for `build_id`, up to `limit`.
    async fn fetch_beta_build_localizations(
        &self,
        build_id: String,
        limit: u32,
    ) -> Result<Vec<BetaBuildLocalizationInfo>, StackError>;

    /// Creates a beta build localization for `build_id` in `locale` with the
    /// given `whats_new` testing notes.
    async fn create_beta_build_localization(
        &self,
        build_id: String,
        locale: String,
        whats_new: String,
    ) -> Result<BetaBuildLocalizationInfo, StackError>;

    /// Updates the beta build localization `id`, replacing its `whats_new`
    /// testing notes.
    async fn update_beta_build_localization(
        &self,
        id: String,
        whats_new: String,
    ) -> Result<BetaBuildLocalizationInfo, StackError>;
}

/// UniFFI-exported Beta Build Localizations capability handle. A thin,
/// binding-friendly wrapper around a boxed [`BetaBuildLocalizationsImpl`]; async
/// work runs on the tokio runtime. Reached via
/// [`crate::service::provider::Provider::beta_build_localizations`].
#[derive(uniffi::Object)]
pub struct BetaBuildLocalizations {
    inner: Box<dyn BetaBuildLocalizationsImpl>,
}

impl BetaBuildLocalizations {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn BetaBuildLocalizationsImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl BetaBuildLocalizations {
    /// Lists the beta build localizations for `build_id`, up to `limit`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_beta_build_localizations(
        &self,
        build_id: String,
        limit: u32,
    ) -> Result<Vec<BetaBuildLocalizationInfo>, StackError> {
        self.inner
            .fetch_beta_build_localizations(build_id, limit)
            .await
    }

    /// Creates a beta build localization for `build_id` in `locale`, returning the
    /// created localization. `whats_new` is the per-locale "What to Test" text
    /// shown to testers.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_beta_build_localization(
        &self,
        build_id: String,
        locale: String,
        whats_new: String,
    ) -> Result<BetaBuildLocalizationInfo, StackError> {
        self.inner
            .create_beta_build_localization(build_id, locale, whats_new)
            .await
    }

    /// Updates the beta build localization `id`, replacing its `whats_new`
    /// testing notes, and returns the updated localization.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn update_beta_build_localization(
        &self,
        id: String,
        whats_new: String,
    ) -> Result<BetaBuildLocalizationInfo, StackError> {
        self.inner
            .update_beta_build_localization(id, whats_new)
            .await
    }
}
