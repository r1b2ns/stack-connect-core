use std::sync::Arc;

use async_trait::async_trait;

use super::client::AppStoreClient;
use crate::auth::es256::AppStoreAuthenticator;
use crate::domain::{
    AppInfo, AppStoreVersionInfo, BetaBuildLocalizationInfo, BetaGroupInfo, BetaTesterInfo,
    BuildInfo, CustomerReview, CustomerReviewsPage, ReviewResponse, ReviewSubmission,
};
use crate::error::StackError;
use crate::service::capabilities::app_store_versions::{AppStoreVersions, AppStoreVersionsImpl};
use crate::service::capabilities::beta_build_localizations::{
    BetaBuildLocalizations, BetaBuildLocalizationsImpl,
};
use crate::service::capabilities::beta_groups::{BetaGroups, BetaGroupsImpl};
use crate::service::capabilities::builds::{Builds, BuildsImpl};
use crate::service::capabilities::reviews::{Reviews, ReviewsImpl};
use crate::service::kind::ServiceKind;
use crate::service::provider::{Capability, ProviderImpl};

/// App Store Connect implementation of the internal [`ProviderImpl`] contract.
///
/// The [`AppStoreClient`] is held behind an `Arc` so capability sub-objects (the
/// [`Reviews`] handle) share the same client — and therefore the same
/// authenticator and token cache — as `validate`/`fetch_apps`.
pub(crate) struct AppStoreProvider {
    client: Arc<AppStoreClient>,
}

impl AppStoreProvider {
    /// Builds the provider from the three required credentials.
    pub(crate) fn new(issuer_id: String, key_id: String, private_key_p8: Vec<u8>) -> Self {
        let auth = AppStoreAuthenticator::new(issuer_id, key_id, private_key_p8);
        Self {
            client: Arc::new(AppStoreClient::new(auth)),
        }
    }
}

#[async_trait]
impl ProviderImpl for AppStoreProvider {
    fn kind(&self) -> ServiceKind {
        ServiceKind::AppStoreConnect
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Apps,
            Capability::Reviews,
            Capability::AppStoreVersions,
            Capability::Builds,
            Capability::BetaGroups,
            Capability::BetaBuildLocalizations,
        ]
    }

    async fn validate(&self) -> Result<(), StackError> {
        self.client.validate().await
    }

    async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        self.client.fetch_apps().await
    }

    fn reviews(&self) -> Option<Arc<Reviews>> {
        Some(Reviews::new(Box::new(AppStoreReviews {
            client: Arc::clone(&self.client),
        })))
    }

    fn app_store_versions(&self) -> Option<Arc<AppStoreVersions>> {
        Some(AppStoreVersions::new(Box::new(AppStoreAppStoreVersions {
            client: Arc::clone(&self.client),
        })))
    }

    fn builds(&self) -> Option<Arc<Builds>> {
        Some(Builds::new(Box::new(AppStoreBuilds {
            client: Arc::clone(&self.client),
        })))
    }

    fn beta_groups(&self) -> Option<Arc<BetaGroups>> {
        Some(BetaGroups::new(Box::new(AppStoreBetaGroups {
            client: Arc::clone(&self.client),
        })))
    }

    fn beta_build_localizations(&self) -> Option<Arc<BetaBuildLocalizations>> {
        Some(BetaBuildLocalizations::new(Box::new(
            AppStoreBetaBuildLocalizations {
                client: Arc::clone(&self.client),
            },
        )))
    }
}

/// App Store Connect implementation of the [`ReviewsImpl`] capability contract.
/// Holds a shared [`AppStoreClient`] so it reuses the provider's token cache.
struct AppStoreReviews {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl ReviewsImpl for AppStoreReviews {
    async fn fetch_customer_reviews(
        &self,
        app_id: String,
    ) -> Result<Vec<CustomerReview>, StackError> {
        self.client.fetch_customer_reviews(&app_id).await
    }

    async fn fetch_customer_reviews_page(
        &self,
        app_id: String,
        sort: String,
        filter_rating: Vec<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<CustomerReviewsPage, StackError> {
        self.client
            .fetch_customer_reviews_page(
                &app_id,
                &sort,
                &filter_rating,
                limit,
                page_token.as_deref(),
            )
            .await
    }

    async fn fetch_review_submissions(
        &self,
        app_id: String,
    ) -> Result<Vec<ReviewSubmission>, StackError> {
        self.client.fetch_review_submissions(&app_id).await
    }

    async fn reply_to_review(
        &self,
        review_id: String,
        body: String,
    ) -> Result<ReviewResponse, StackError> {
        self.client.reply_to_review(&review_id, &body).await
    }

    async fn delete_review_response(&self, response_id: String) -> Result<(), StackError> {
        self.client.delete_review_response(&response_id).await
    }
}

/// App Store Connect implementation of the [`AppStoreVersionsImpl`] capability
/// contract. Holds a shared [`AppStoreClient`] so it reuses the provider's token
/// cache.
struct AppStoreAppStoreVersions {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl AppStoreVersionsImpl for AppStoreAppStoreVersions {
    async fn fetch_versions(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<AppStoreVersionInfo>, StackError> {
        self.client.fetch_versions(&app_id, limit).await
    }

    async fn create_version(
        &self,
        app_id: String,
        platform: String,
        version_string: String,
    ) -> Result<AppStoreVersionInfo, StackError> {
        self.client
            .create_version(&app_id, &platform, &version_string)
            .await
    }

    async fn update_version(
        &self,
        id: String,
        version_string: Option<String>,
        copyright: Option<String>,
        release_type: Option<String>,
        earliest_release_date: Option<String>,
    ) -> Result<(), StackError> {
        self.client
            .update_version(
                &id,
                version_string.as_deref(),
                copyright.as_deref(),
                release_type.as_deref(),
                earliest_release_date.as_deref(),
            )
            .await
    }

    async fn delete_version(&self, id: String) -> Result<(), StackError> {
        self.client.delete_version(&id).await
    }
}

/// App Store Connect implementation of the [`BuildsImpl`] capability contract.
/// Holds a shared [`AppStoreClient`] so it reuses the provider's token cache.
struct AppStoreBuilds {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl BuildsImpl for AppStoreBuilds {
    async fn fetch_builds(&self, app_id: String, limit: u32) -> Result<Vec<BuildInfo>, StackError> {
        self.client.fetch_builds(&app_id, limit).await
    }
}

/// App Store Connect implementation of the [`BetaGroupsImpl`] capability
/// contract. Holds a shared [`AppStoreClient`] so it reuses the provider's token
/// cache.
struct AppStoreBetaGroups {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl BetaGroupsImpl for AppStoreBetaGroups {
    async fn fetch_beta_groups(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<BetaGroupInfo>, StackError> {
        self.client.fetch_beta_groups(&app_id, limit).await
    }

    async fn fetch_beta_testers(
        &self,
        group_id: String,
        limit: u32,
    ) -> Result<Vec<BetaTesterInfo>, StackError> {
        self.client.fetch_beta_testers(&group_id, limit).await
    }

    async fn create_beta_group(
        &self,
        app_id: String,
        name: String,
        is_internal: bool,
        public_link_enabled: bool,
        has_access_to_all_builds: bool,
    ) -> Result<BetaGroupInfo, StackError> {
        self.client
            .create_beta_group(
                &app_id,
                &name,
                is_internal,
                public_link_enabled,
                has_access_to_all_builds,
            )
            .await
    }

    async fn update_beta_group(
        &self,
        group_id: String,
        name: Option<String>,
        public_link_enabled: Option<bool>,
        public_link_limit: Option<i32>,
        feedback_enabled: Option<bool>,
    ) -> Result<BetaGroupInfo, StackError> {
        self.client
            .update_beta_group(
                &group_id,
                name.as_deref(),
                public_link_enabled,
                public_link_limit,
                feedback_enabled,
            )
            .await
    }

    async fn delete_beta_group(&self, group_id: String) -> Result<(), StackError> {
        self.client.delete_beta_group(&group_id).await
    }

    async fn add_beta_tester(
        &self,
        group_id: String,
        email: String,
        first_name: Option<String>,
        last_name: Option<String>,
    ) -> Result<BetaTesterInfo, StackError> {
        self.client
            .add_beta_tester(
                &group_id,
                &email,
                first_name.as_deref(),
                last_name.as_deref(),
            )
            .await
    }

    async fn remove_beta_tester(
        &self,
        group_id: String,
        tester_id: String,
    ) -> Result<(), StackError> {
        self.client.remove_beta_tester(&group_id, &tester_id).await
    }
}

/// App Store Connect implementation of the [`BetaBuildLocalizationsImpl`]
/// capability contract. Holds a shared [`AppStoreClient`] so it reuses the
/// provider's token cache.
struct AppStoreBetaBuildLocalizations {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl BetaBuildLocalizationsImpl for AppStoreBetaBuildLocalizations {
    async fn fetch_beta_build_localizations(
        &self,
        build_id: String,
        limit: u32,
    ) -> Result<Vec<BetaBuildLocalizationInfo>, StackError> {
        self.client
            .fetch_beta_build_localizations(&build_id, limit)
            .await
    }

    async fn create_beta_build_localization(
        &self,
        build_id: String,
        locale: String,
        whats_new: String,
    ) -> Result<BetaBuildLocalizationInfo, StackError> {
        self.client
            .create_beta_build_localization(&build_id, &locale, &whats_new)
            .await
    }

    async fn update_beta_build_localization(
        &self,
        id: String,
        whats_new: String,
    ) -> Result<BetaBuildLocalizationInfo, StackError> {
        self.client
            .update_beta_build_localization(&id, &whats_new)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> AppStoreProvider {
        AppStoreProvider::new(
            "issuer".into(),
            "kid".into(),
            include_bytes!("../../../tests/fixtures/test_ec_private.p8").to_vec(),
        )
    }

    #[test]
    fn reports_kind_and_capabilities() {
        let p = provider();
        assert_eq!(p.kind(), ServiceKind::AppStoreConnect);
        assert_eq!(
            p.capabilities(),
            vec![
                Capability::Apps,
                Capability::Reviews,
                Capability::AppStoreVersions,
                Capability::Builds,
                Capability::BetaGroups,
                Capability::BetaBuildLocalizations
            ]
        );
    }

    #[test]
    fn exposes_reviews_capability_handle() {
        // App Store Connect supports Reviews, so the accessor must return `Some`.
        // (Appstore is the only provider today; a `None` provider cannot yet be
        // constructed to assert the unsupported branch.)
        assert!(provider().reviews().is_some());
        assert!(provider().capabilities().contains(&Capability::Reviews));
    }

    #[test]
    fn exposes_app_store_versions_capability_handle() {
        // App Store Connect supports App Store Versions, so the accessor must
        // return `Some`.
        assert!(provider().app_store_versions().is_some());
        assert!(provider()
            .capabilities()
            .contains(&Capability::AppStoreVersions));
    }

    #[test]
    fn exposes_builds_capability_handle() {
        // App Store Connect supports Builds, so the accessor must return `Some`.
        assert!(provider().builds().is_some());
        assert!(provider().capabilities().contains(&Capability::Builds));
    }

    #[test]
    fn exposes_beta_groups_capability_handle() {
        // App Store Connect supports Beta Groups, so the accessor must return
        // `Some`.
        assert!(provider().beta_groups().is_some());
        assert!(provider().capabilities().contains(&Capability::BetaGroups));
    }

    #[test]
    fn exposes_beta_build_localizations_capability_handle() {
        // App Store Connect supports Beta Build Localizations, so the accessor
        // must return `Some`.
        assert!(provider().beta_build_localizations().is_some());
        assert!(provider()
            .capabilities()
            .contains(&Capability::BetaBuildLocalizations));
    }
}
