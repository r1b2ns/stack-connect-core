use std::sync::Arc;

use async_trait::async_trait;

use super::client::AppStoreClient;
use crate::auth::es256::AppStoreAuthenticator;
use crate::domain::{
    AccessibilityDeclarationInfo, AppCategoryInfo, AppInfo, AppInfoDetails,
    AppInfoLocalizationInfo, AppReviewDetailInfo, AppStoreLocalizationInfo, AppStoreVersionInfo,
    BetaAppLocalizationInfo, BetaAppReviewDetailInfo, BetaBuildLocalizationInfo, BetaGroupInfo,
    BetaTesterInfo, BuildDetailInfo, BuildInfo, BuildsPage, BundleIdCapabilityInfo, BundleIdInfo,
    CustomerReview, CustomerReviewsPage, DeviceInfo, PhasedReleaseInfo, ReviewResponse,
    ReviewSubmission, ScreenshotSetInfo, TeamMemberInfo, UserInfo,
};
use crate::error::StackError;
use crate::ports::DebugLogger;
use crate::service::capabilities::accessibility_declarations::{
    AccessibilityDeclarations, AccessibilityDeclarationsImpl,
};
use crate::service::capabilities::app_metadata::{AppMetadata, AppMetadataImpl};
use crate::service::capabilities::app_store_versions::{AppStoreVersions, AppStoreVersionsImpl};
use crate::service::capabilities::beta_app_localizations::{
    BetaAppLocalizations, BetaAppLocalizationsImpl,
};
use crate::service::capabilities::beta_app_review_detail::{
    BetaAppReviewDetail, BetaAppReviewDetailImpl,
};
use crate::service::capabilities::beta_build_localizations::{
    BetaBuildLocalizations, BetaBuildLocalizationsImpl,
};
use crate::service::capabilities::beta_groups::{BetaGroups, BetaGroupsImpl};
use crate::service::capabilities::builds::{Builds, BuildsImpl};
use crate::service::capabilities::bundle_ids::{BundleIds, BundleIdsImpl};
use crate::service::capabilities::devices::{Devices, DevicesImpl};
use crate::service::capabilities::reviews::{Reviews, ReviewsImpl};
use crate::service::capabilities::users::{Users, UsersImpl};
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
    /// Builds the provider from the three required credentials. When
    /// `debug_logger` is `Some`, the underlying [`AppStoreClient`] logs every
    /// HTTP request/response through it (see [`crate::ports::DebugLogger`]).
    pub(crate) fn new(
        issuer_id: String,
        key_id: String,
        private_key_p8: Vec<u8>,
        debug_logger: Option<Arc<dyn DebugLogger>>,
    ) -> Self {
        let auth = AppStoreAuthenticator::new(issuer_id, key_id, private_key_p8);
        Self {
            client: Arc::new(AppStoreClient::new(auth).with_debug_logger(debug_logger)),
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
            Capability::BetaAppLocalizations,
            Capability::BetaAppReviewDetail,
            Capability::AppMetadata,
            Capability::AccessibilityDeclarations,
            Capability::Users,
            Capability::Devices,
            Capability::BundleIds,
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

    fn beta_app_localizations(&self) -> Option<Arc<BetaAppLocalizations>> {
        Some(BetaAppLocalizations::new(Box::new(
            AppStoreBetaAppLocalizations {
                client: Arc::clone(&self.client),
            },
        )))
    }

    fn beta_app_review_detail(&self) -> Option<Arc<BetaAppReviewDetail>> {
        Some(BetaAppReviewDetail::new(Box::new(
            AppStoreBetaAppReviewDetail {
                client: Arc::clone(&self.client),
            },
        )))
    }

    fn app_metadata(&self) -> Option<Arc<AppMetadata>> {
        Some(AppMetadata::new(Box::new(AppStoreAppMetadata {
            client: Arc::clone(&self.client),
        })))
    }

    fn accessibility_declarations(&self) -> Option<Arc<AccessibilityDeclarations>> {
        Some(AccessibilityDeclarations::new(Box::new(
            AppStoreAccessibilityDeclarations {
                client: Arc::clone(&self.client),
            },
        )))
    }

    fn users(&self) -> Option<Arc<Users>> {
        Some(Users::new(Box::new(AppStoreUsers {
            client: Arc::clone(&self.client),
        })))
    }

    fn devices(&self) -> Option<Arc<Devices>> {
        Some(Devices::new(Box::new(AppStoreDevices {
            client: Arc::clone(&self.client),
        })))
    }

    fn bundle_ids(&self) -> Option<Arc<BundleIds>> {
        Some(BundleIds::new(Box::new(AppStoreBundleIds {
            client: Arc::clone(&self.client),
        })))
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

    async fn submit_for_review(
        &self,
        app_id: String,
        version_id: String,
        platform: Option<String>,
    ) -> Result<(), StackError> {
        self.client
            .submit_for_review(&app_id, &version_id, platform.as_deref())
            .await
    }

    async fn cancel_review(&self, app_id: String) -> Result<(), StackError> {
        self.client.cancel_review(&app_id).await
    }

    async fn release_version(&self, version_id: String) -> Result<(), StackError> {
        self.client.release_version(&version_id).await
    }

    async fn reject_version(&self, app_id: String) -> Result<(), StackError> {
        self.client.reject_version(&app_id).await
    }

    async fn fetch_phased_release(
        &self,
        version_id: String,
    ) -> Result<Option<PhasedReleaseInfo>, StackError> {
        self.client.fetch_phased_release(&version_id).await
    }

    async fn create_phased_release(
        &self,
        version_id: String,
        state: String,
    ) -> Result<PhasedReleaseInfo, StackError> {
        self.client.create_phased_release(&version_id, &state).await
    }

    async fn delete_phased_release(&self, id: String) -> Result<(), StackError> {
        self.client.delete_phased_release(&id).await
    }

    async fn update_phased_release_state(
        &self,
        id: String,
        state: String,
    ) -> Result<PhasedReleaseInfo, StackError> {
        self.client.update_phased_release_state(&id, &state).await
    }

    async fn fetch_localizations(
        &self,
        version_id: String,
    ) -> Result<Vec<AppStoreLocalizationInfo>, StackError> {
        self.client.fetch_localizations(&version_id).await
    }

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
    ) -> Result<(), StackError> {
        self.client
            .update_localization(
                &id,
                description.as_deref(),
                keywords.as_deref(),
                promotional_text.as_deref(),
                support_url.as_deref(),
                marketing_url.as_deref(),
                whats_new.as_deref(),
            )
            .await
    }

    async fn fetch_screenshot_sets(
        &self,
        localization_id: String,
    ) -> Result<Vec<ScreenshotSetInfo>, StackError> {
        self.client.fetch_screenshot_sets(&localization_id).await
    }

    async fn fetch_app_review_detail(
        &self,
        version_id: String,
    ) -> Result<Option<AppReviewDetailInfo>, StackError> {
        self.client.fetch_app_review_detail(&version_id).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_app_review_detail(
        &self,
        detail_id: String,
        contact_first_name: Option<String>,
        contact_last_name: Option<String>,
        contact_email: Option<String>,
        contact_phone: Option<String>,
        notes: Option<String>,
        demo_account_name: Option<String>,
        demo_account_password: Option<String>,
        is_demo_account_required: Option<bool>,
    ) -> Result<AppReviewDetailInfo, StackError> {
        self.client
            .update_app_review_detail(
                &detail_id,
                contact_first_name.as_deref(),
                contact_last_name.as_deref(),
                contact_email.as_deref(),
                contact_phone.as_deref(),
                notes.as_deref(),
                demo_account_name.as_deref(),
                demo_account_password.as_deref(),
                is_demo_account_required,
            )
            .await
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

    async fn fetch_builds_page(
        &self,
        app_id: String,
        platform: Option<String>,
        processing_states: Vec<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<BuildsPage, StackError> {
        self.client
            .fetch_builds_page(
                &app_id,
                platform.as_deref(),
                &processing_states,
                limit,
                page_token.as_deref(),
            )
            .await
    }

    async fn fetch_builds_for_group(
        &self,
        group_id: String,
        limit: u32,
    ) -> Result<Vec<BuildInfo>, StackError> {
        self.client.fetch_builds_for_group(&group_id, limit).await
    }

    async fn fetch_build_detail(&self, build_id: String) -> Result<BuildDetailInfo, StackError> {
        self.client.fetch_build_detail(&build_id).await
    }

    async fn fetch_current_build(
        &self,
        version_id: String,
    ) -> Result<Option<BuildInfo>, StackError> {
        self.client.fetch_current_build(&version_id).await
    }

    async fn expire_build(&self, build_id: String) -> Result<(), StackError> {
        self.client.expire_build(&build_id).await
    }

    async fn attach_build(&self, version_id: String, build_id: String) -> Result<(), StackError> {
        self.client.attach_build(&version_id, &build_id).await
    }

    async fn submit_build_for_beta_review(&self, build_id: String) -> Result<(), StackError> {
        self.client.submit_build_for_beta_review(&build_id).await
    }

    async fn add_build_to_groups(
        &self,
        build_id: String,
        group_ids: Vec<String>,
    ) -> Result<(), StackError> {
        self.client.add_build_to_groups(&build_id, &group_ids).await
    }

    async fn remove_build_from_group(
        &self,
        build_id: String,
        group_id: String,
    ) -> Result<(), StackError> {
        self.client
            .remove_build_from_group(&build_id, &group_id)
            .await
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

    async fn fetch_tester_count(&self, group_id: String) -> Result<u32, StackError> {
        self.client.fetch_tester_count(&group_id).await
    }

    async fn resend_invite(&self, tester_id: String, app_id: String) -> Result<(), StackError> {
        self.client.resend_invite(&tester_id, &app_id).await
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

/// App Store Connect implementation of the [`BetaAppLocalizationsImpl`]
/// capability contract. Holds a shared [`AppStoreClient`] so it reuses the
/// provider's token cache.
struct AppStoreBetaAppLocalizations {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl BetaAppLocalizationsImpl for AppStoreBetaAppLocalizations {
    async fn fetch_beta_app_localizations(
        &self,
        app_id: String,
        limit: u32,
    ) -> Result<Vec<BetaAppLocalizationInfo>, StackError> {
        self.client
            .fetch_beta_app_localizations(&app_id, limit)
            .await
    }

    async fn create_beta_app_localization(
        &self,
        app_id: String,
        locale: String,
        feedback_email: Option<String>,
        description: Option<String>,
    ) -> Result<BetaAppLocalizationInfo, StackError> {
        self.client
            .create_beta_app_localization(
                &app_id,
                &locale,
                feedback_email.as_deref(),
                description.as_deref(),
            )
            .await
    }

    async fn update_beta_app_localization(
        &self,
        id: String,
        feedback_email: Option<String>,
        description: Option<String>,
    ) -> Result<BetaAppLocalizationInfo, StackError> {
        self.client
            .update_beta_app_localization(&id, feedback_email.as_deref(), description.as_deref())
            .await
    }
}

/// App Store Connect implementation of the [`BetaAppReviewDetailImpl`]
/// capability contract. Holds a shared [`AppStoreClient`] so it reuses the
/// provider's token cache.
struct AppStoreBetaAppReviewDetail {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl BetaAppReviewDetailImpl for AppStoreBetaAppReviewDetail {
    async fn fetch_beta_app_review_detail(
        &self,
        app_id: String,
    ) -> Result<BetaAppReviewDetailInfo, StackError> {
        self.client.fetch_beta_app_review_detail(&app_id).await
    }

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
    ) -> Result<BetaAppReviewDetailInfo, StackError> {
        self.client
            .update_beta_app_review_detail(
                &detail_id,
                contact_first_name.as_deref(),
                contact_last_name.as_deref(),
                contact_email.as_deref(),
                contact_phone.as_deref(),
                demo_account_name.as_deref(),
                demo_account_password.as_deref(),
                is_demo_account_required,
                notes.as_deref(),
            )
            .await
    }
}

/// App Store Connect implementation of the [`AppMetadataImpl`] capability
/// contract. Holds a shared [`AppStoreClient`] so it reuses the provider's token
/// cache.
struct AppStoreAppMetadata {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl AppMetadataImpl for AppStoreAppMetadata {
    async fn fetch_app_info_localizations(
        &self,
        app_info_id: String,
    ) -> Result<Vec<AppInfoLocalizationInfo>, StackError> {
        self.client.fetch_app_info_localizations(&app_info_id).await
    }

    async fn update_app_info_localization(
        &self,
        id: String,
        name: String,
        subtitle: Option<String>,
    ) -> Result<AppInfoLocalizationInfo, StackError> {
        self.client
            .update_app_info_localization(&id, &name, subtitle.as_deref())
            .await
    }

    async fn update_app_info_localization_privacy(
        &self,
        id: String,
        privacy_policy_url: Option<String>,
        privacy_choices_url: Option<String>,
        privacy_policy_text: Option<String>,
    ) -> Result<AppInfoLocalizationInfo, StackError> {
        self.client
            .update_app_info_localization_privacy(
                &id,
                privacy_policy_url.as_deref(),
                privacy_choices_url.as_deref(),
                privacy_policy_text.as_deref(),
            )
            .await
    }

    async fn create_app_info_localization(
        &self,
        app_info_id: String,
        locale: String,
        name: String,
        subtitle: Option<String>,
    ) -> Result<AppInfoLocalizationInfo, StackError> {
        self.client
            .create_app_info_localization(&app_info_id, &locale, &name, subtitle.as_deref())
            .await
    }

    async fn delete_app_info_localization(&self, id: String) -> Result<(), StackError> {
        self.client.delete_app_info_localization(&id).await
    }

    async fn fetch_app_info(&self, app_id: String) -> Result<AppInfoDetails, StackError> {
        self.client.fetch_app_info(&app_id).await
    }

    async fn fetch_app_categories(&self) -> Result<Vec<AppCategoryInfo>, StackError> {
        self.client.fetch_app_categories().await
    }

    async fn update_app_info_category(
        &self,
        app_info_id: String,
        primary_category_id: Option<String>,
        subcategory_one_id: Option<String>,
        secondary_category_id: Option<String>,
        secondary_subcategory_one_id: Option<String>,
    ) -> Result<(), StackError> {
        self.client
            .update_app_info_category(
                &app_info_id,
                primary_category_id.as_deref(),
                subcategory_one_id.as_deref(),
                secondary_category_id.as_deref(),
                secondary_subcategory_one_id.as_deref(),
            )
            .await
    }

    async fn update_app(
        &self,
        id: String,
        content_rights_declaration: Option<String>,
        primary_locale: Option<String>,
    ) -> Result<(), StackError> {
        self.client
            .update_app(
                &id,
                content_rights_declaration.as_deref(),
                primary_locale.as_deref(),
            )
            .await
    }

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
    ) -> Result<(), StackError> {
        self.client
            .update_age_rating(
                &id,
                &alcohol_tobacco,
                &contests,
                &gambling_simulated,
                &guns_or_other_weapons,
                &medical_information,
                &profanity,
                &sexual_content_graphic,
                &sexual_content_or_nudity,
                &horror_or_fear,
                &mature_or_suggestive,
                &violence_cartoon,
                &violence_realistic,
                &violence_graphic,
                is_advertising,
                is_gambling,
                is_unrestricted_web_access,
                is_user_generated_content,
                &age_rating_override,
            )
            .await
    }

    async fn fetch_icon_url(&self, app_id: String) -> Result<Option<String>, StackError> {
        self.client.fetch_icon_url(&app_id).await
    }
}

/// App Store Connect implementation of the [`AccessibilityDeclarationsImpl`]
/// capability contract. Holds a shared [`AppStoreClient`] so it reuses the
/// provider's token cache.
struct AppStoreAccessibilityDeclarations {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl AccessibilityDeclarationsImpl for AppStoreAccessibilityDeclarations {
    async fn fetch_accessibility_declarations(
        &self,
        app_id: String,
        limit: i64,
    ) -> Result<Vec<AccessibilityDeclarationInfo>, StackError> {
        self.client
            .fetch_accessibility_declarations(&app_id, limit)
            .await
    }

    async fn create_accessibility_declaration(
        &self,
        app_id: String,
        device_family: String,
    ) -> Result<AccessibilityDeclarationInfo, StackError> {
        self.client
            .create_accessibility_declaration(&app_id, &device_family)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_accessibility_declaration(
        &self,
        id: String,
        publish: bool,
        supports_audio_descriptions: bool,
        supports_captions: bool,
        supports_dark_interface: bool,
        supports_differentiate_without_color: bool,
        supports_larger_text: bool,
        supports_reduced_motion: bool,
        supports_sufficient_contrast: bool,
        supports_voice_control: bool,
        supports_voiceover: bool,
    ) -> Result<AccessibilityDeclarationInfo, StackError> {
        self.client
            .update_accessibility_declaration(
                &id,
                publish,
                supports_audio_descriptions,
                supports_captions,
                supports_dark_interface,
                supports_differentiate_without_color,
                supports_larger_text,
                supports_reduced_motion,
                supports_sufficient_contrast,
                supports_voice_control,
                supports_voiceover,
            )
            .await
    }

    async fn delete_accessibility_declaration(&self, id: String) -> Result<(), StackError> {
        self.client.delete_accessibility_declaration(&id).await
    }
}

/// App Store Connect implementation of the [`UsersImpl`] capability contract.
/// Holds a shared [`AppStoreClient`] so it reuses the provider's token cache.
struct AppStoreUsers {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl UsersImpl for AppStoreUsers {
    async fn fetch_team_members(&self) -> Result<Vec<TeamMemberInfo>, StackError> {
        self.client.fetch_team_members().await
    }

    async fn fetch_users(&self) -> Result<Vec<UserInfo>, StackError> {
        self.client.fetch_users().await
    }

    async fn invite_user(
        &self,
        email: String,
        first_name: String,
        last_name: String,
        roles: Vec<String>,
        all_apps_visible: bool,
        provisioning_allowed: bool,
    ) -> Result<(), StackError> {
        self.client
            .invite_user(
                &email,
                &first_name,
                &last_name,
                &roles,
                all_apps_visible,
                provisioning_allowed,
            )
            .await
    }

    async fn delete_user(&self, id: String, is_pending: bool) -> Result<(), StackError> {
        self.client.delete_user(&id, is_pending).await
    }
}

/// App Store Connect implementation of the [`DevicesImpl`] capability contract.
/// Holds a shared [`AppStoreClient`] so it reuses the provider's token cache.
struct AppStoreDevices {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl DevicesImpl for AppStoreDevices {
    async fn fetch_devices(&self) -> Result<Vec<DeviceInfo>, StackError> {
        self.client.fetch_devices().await
    }

    async fn create_device(
        &self,
        name: String,
        platform: String,
        udid: String,
    ) -> Result<DeviceInfo, StackError> {
        self.client.create_device(&name, &platform, &udid).await
    }

    async fn update_device(
        &self,
        id: String,
        name: Option<String>,
        status: Option<String>,
    ) -> Result<(), StackError> {
        self.client
            .update_device(&id, name.as_deref(), status.as_deref())
            .await
    }
}

/// App Store Connect implementation of the [`BundleIdsImpl`] capability contract.
/// Holds a shared [`AppStoreClient`] so it reuses the provider's token cache.
struct AppStoreBundleIds {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl BundleIdsImpl for AppStoreBundleIds {
    async fn fetch_bundle_ids(&self) -> Result<Vec<BundleIdInfo>, StackError> {
        self.client.fetch_bundle_ids().await
    }

    async fn create_bundle_id(
        &self,
        identifier: String,
        name: String,
        platform: String,
    ) -> Result<BundleIdInfo, StackError> {
        self.client
            .create_bundle_id(&identifier, &name, &platform)
            .await
    }

    async fn update_bundle_id(&self, id: String, name: String) -> Result<(), StackError> {
        self.client.update_bundle_id(&id, &name).await
    }

    async fn delete_bundle_id(&self, id: String) -> Result<(), StackError> {
        self.client.delete_bundle_id(&id).await
    }

    async fn fetch_bundle_id_capabilities(
        &self,
        bundle_id: String,
    ) -> Result<Vec<BundleIdCapabilityInfo>, StackError> {
        self.client.fetch_bundle_id_capabilities(&bundle_id).await
    }

    async fn enable_capability(
        &self,
        bundle_id: String,
        capability_type: String,
    ) -> Result<BundleIdCapabilityInfo, StackError> {
        self.client
            .enable_capability(&bundle_id, &capability_type)
            .await
    }

    async fn disable_capability(&self, capability_id: String) -> Result<(), StackError> {
        self.client.disable_capability(&capability_id).await
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
            None,
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
                Capability::BetaBuildLocalizations,
                Capability::BetaAppLocalizations,
                Capability::BetaAppReviewDetail,
                Capability::AppMetadata,
                Capability::AccessibilityDeclarations,
                Capability::Users,
                Capability::Devices,
                Capability::BundleIds
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

    #[test]
    fn exposes_beta_app_localizations_capability_handle() {
        // App Store Connect supports Beta App Localizations, so the accessor must
        // return `Some`.
        assert!(provider().beta_app_localizations().is_some());
        assert!(provider()
            .capabilities()
            .contains(&Capability::BetaAppLocalizations));
    }

    #[test]
    fn exposes_beta_app_review_detail_capability_handle() {
        // App Store Connect supports the Beta App Review Detail, so the accessor
        // must return `Some`.
        assert!(provider().beta_app_review_detail().is_some());
        assert!(provider()
            .capabilities()
            .contains(&Capability::BetaAppReviewDetail));
    }

    #[test]
    fn exposes_app_metadata_capability_handle() {
        // App Store Connect supports App Metadata, so the accessor must return
        // `Some`.
        assert!(provider().app_metadata().is_some());
        assert!(provider().capabilities().contains(&Capability::AppMetadata));
    }

    #[test]
    fn exposes_accessibility_declarations_capability_handle() {
        // App Store Connect supports Accessibility Declarations, so the accessor
        // must return `Some`.
        assert!(provider().accessibility_declarations().is_some());
        assert!(provider()
            .capabilities()
            .contains(&Capability::AccessibilityDeclarations));
    }

    #[test]
    fn exposes_users_capability_handle() {
        // App Store Connect supports Users, so the accessor must return `Some`.
        assert!(provider().users().is_some());
        assert!(provider().capabilities().contains(&Capability::Users));
    }

    #[test]
    fn exposes_devices_capability_handle() {
        // App Store Connect supports Devices, so the accessor must return `Some`.
        assert!(provider().devices().is_some());
        assert!(provider().capabilities().contains(&Capability::Devices));
    }

    #[test]
    fn exposes_bundle_ids_capability_handle() {
        // App Store Connect supports BundleIds, so the accessor must return `Some`.
        assert!(provider().bundle_ids().is_some());
        assert!(provider().capabilities().contains(&Capability::BundleIds));
    }
}
