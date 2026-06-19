use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::json;

use crate::auth::es256::AppStoreAuthenticator;
use crate::domain::{
    AccessibilityDeclarationInfo, AgeRatingDeclarationInfo, AppCategoryInfo, AppInfo,
    AppInfoDetails, AppInfoLocalizationInfo, AppReviewDetailInfo, AppStoreLocalizationInfo,
    AppStoreVersionInfo, BetaAppLocalizationInfo, BetaAppReviewDetailInfo,
    BetaBuildLocalizationInfo, BetaGroupInfo, BetaTesterInfo, BuildDetailInfo, BuildInfo,
    BuildsPage, BundleIdCapabilityInfo, BundleIdInfo, CertificateInfo, CustomerReview,
    CustomerReviewsPage, DeviceInfo, PhasedReleaseInfo, ProvisioningProfileInfo, ReviewResponse,
    ReviewSubmission, ScreenshotInfo, ScreenshotSetInfo, TeamMemberInfo, UserInfo,
};
use crate::error::StackError;
use crate::ports::DebugLogger;

const DEFAULT_BASE_URL: &str = "https://api.appstoreconnect.apple.com";

/// A JSON:API document page of `apps` resources.
#[derive(Deserialize)]
struct AppsResponse {
    #[serde(default)]
    data: Vec<AppResource>,
    #[serde(default)]
    links: Links,
}

#[derive(Deserialize, Default)]
struct Links {
    #[serde(default)]
    next: Option<String>,
}

#[derive(Deserialize)]
struct AppResource {
    id: String,
    #[serde(default)]
    attributes: AppAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppAttributes {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    bundle_id: Option<String>,
}

impl AppResource {
    fn into_app_info(self) -> AppInfo {
        AppInfo {
            id: self.id,
            name: self.attributes.name.unwrap_or_default(),
            bundle_id: self.attributes.bundle_id.unwrap_or_default(),
            platform: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Customer reviews (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `customerReviews`, with `customerReviewResponses`
/// carried in `included[]`.
#[derive(Deserialize)]
struct CustomerReviewsResponse {
    #[serde(default)]
    data: Vec<CustomerReviewResource>,
    #[serde(default)]
    included: Vec<ReviewResponseResource>,
    #[serde(default)]
    links: Links,
}

#[derive(Deserialize)]
struct CustomerReviewResource {
    id: String,
    #[serde(default)]
    attributes: CustomerReviewAttributes,
    #[serde(default)]
    relationships: CustomerReviewRelationships,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CustomerReviewAttributes {
    #[serde(default)]
    rating: i32,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    reviewer_nickname: Option<String>,
    #[serde(default)]
    created_date: Option<String>,
    #[serde(default)]
    territory: Option<String>,
}

#[derive(Deserialize, Default)]
struct CustomerReviewRelationships {
    #[serde(default)]
    response: ToOneRelationship,
}

/// A JSON:API to-one relationship: `{ "data": { "type": ..., "id": ... } }`.
#[derive(Deserialize, Default)]
struct ToOneRelationship {
    #[serde(default)]
    data: Option<ResourceIdentifier>,
}

#[derive(Deserialize)]
struct ResourceIdentifier {
    id: String,
}

/// A JSON:API single-resource document wrapping one `customerReviewResponses`,
/// as returned by the reply (POST) endpoint: `{ "data": { ... } }`.
#[derive(Deserialize)]
struct ReviewResponseDocument {
    data: ReviewResponseResource,
}

#[derive(Deserialize)]
struct ReviewResponseResource {
    id: String,
    #[serde(default)]
    attributes: ReviewResponseAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ReviewResponseAttributes {
    #[serde(default)]
    response_body: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    last_modified_date: Option<String>,
}

impl ReviewResponseResource {
    fn into_review_response(self) -> ReviewResponse {
        ReviewResponse {
            id: self.id,
            body: self.attributes.response_body,
            state: self.attributes.state,
            last_modified_date: self.attributes.last_modified_date,
        }
    }
}

impl CustomerReviewResource {
    fn into_customer_review(self, response: Option<ReviewResponse>) -> CustomerReview {
        CustomerReview {
            id: self.id,
            rating: self.attributes.rating,
            title: self.attributes.title,
            body: self.attributes.body,
            reviewer_nickname: self.attributes.reviewer_nickname,
            created_date: self.attributes.created_date,
            territory: self.attributes.territory,
            response,
        }
    }
}

// ---------------------------------------------------------------------------
// Review submissions (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `reviewSubmissions`, with `appStoreVersions` and
/// `actors` carried in the heterogeneous `included[]`.
#[derive(Deserialize)]
struct ReviewSubmissionsResponse {
    #[serde(default)]
    data: Vec<ReviewSubmissionResource>,
    #[serde(default)]
    included: Vec<IncludedResource>,
    #[serde(default)]
    links: Links,
}

#[derive(Deserialize)]
struct ReviewSubmissionResource {
    id: String,
    #[serde(default)]
    attributes: ReviewSubmissionAttributes,
    #[serde(default)]
    relationships: ReviewSubmissionRelationships,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ReviewSubmissionAttributes {
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    submitted_date: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ReviewSubmissionRelationships {
    #[serde(default)]
    app_store_version_for_review: ToOneRelationship,
    #[serde(default)]
    submitted_by_actor: ToOneRelationship,
}

/// A JSON:API document page of `reviewSubmissionItems`, returned by
/// `GET /v1/reviewSubmissions/{id}/items`. Only each item's `id` is needed (to
/// `DELETE` it) plus `links.next` to follow pagination; any `included[]` is
/// ignored.
#[derive(Deserialize, Default)]
struct ReviewSubmissionItemsResponse {
    #[serde(default)]
    data: Vec<ReviewSubmissionItemResource>,
    #[serde(default)]
    links: Links,
}

#[derive(Deserialize)]
struct ReviewSubmissionItemResource {
    id: String,
}

/// The heterogeneous `included[]` entries, dispatched by their JSON:API `type`.
/// Unknown types deserialize to [`IncludedResource::Other`] and are ignored.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum IncludedResource {
    #[serde(rename = "appStoreVersions")]
    AppStoreVersions {
        id: String,
        #[serde(default)]
        attributes: AppStoreVersionAttributes,
    },
    #[serde(rename = "actors")]
    Actors {
        id: String,
        #[serde(default)]
        attributes: ActorAttributes,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppStoreVersionAttributes {
    #[serde(default)]
    version_string: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ActorAttributes {
    #[serde(default)]
    user_first_name: Option<String>,
    #[serde(default)]
    user_last_name: Option<String>,
    #[serde(default)]
    user_email: Option<String>,
    #[serde(default)]
    api_key_id: Option<String>,
    #[serde(default)]
    actor_type: Option<String>,
}

impl ActorAttributes {
    /// Resolves a display name: "first last" when both name parts are present,
    /// else "API Key (<id>)" for an API key actor, else "Apple" for an Apple
    /// actor, else `None`.
    fn display_name(&self) -> Option<String> {
        match (
            self.user_first_name.as_deref(),
            self.user_last_name.as_deref(),
        ) {
            (Some(first), Some(last)) => Some(format!("{first} {last}")),
            _ => {
                if let Some(api_key_id) = self.api_key_id.as_deref() {
                    Some(format!("API Key ({api_key_id})"))
                } else if self.actor_type.as_deref() == Some("APPLE") {
                    Some("Apple".to_string())
                } else {
                    None
                }
            }
        }
    }
}

impl ReviewSubmissionResource {
    fn into_review_submission(
        self,
        app_id: &str,
        versions: &HashMap<String, Option<String>>,
        actors: &HashMap<String, ActorAttributes>,
    ) -> ReviewSubmission {
        let version_id = self
            .relationships
            .app_store_version_for_review
            .data
            .map(|rel| rel.id);
        let version_string = version_id
            .as_ref()
            .and_then(|id| versions.get(id).cloned())
            .flatten();

        let actor = self
            .relationships
            .submitted_by_actor
            .data
            .and_then(|rel| actors.get(&rel.id));
        let submitted_by_name = actor.and_then(ActorAttributes::display_name);
        let submitted_by_email = actor.and_then(|a| a.user_email.clone());

        ReviewSubmission {
            id: self.id,
            app_id: app_id.to_string(),
            platform: self.attributes.platform,
            submitted_date: self.attributes.submitted_date,
            state: self.attributes.state,
            version_string,
            version_id,
            submitted_by_name,
            submitted_by_email,
        }
    }
}

/// The single-resource document returned by `POST /v1/reviewSubmissions`. Only
/// the created submission's `id` is needed to chain the follow-up requests, so
/// the rest of the resource is ignored.
#[derive(Deserialize)]
struct ReviewSubmissionCreateDocument {
    data: ReviewSubmissionCreateResource,
}

#[derive(Deserialize)]
struct ReviewSubmissionCreateResource {
    id: String,
}

// ---------------------------------------------------------------------------
// App Store versions (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `appStoreVersions` resources.
#[derive(Deserialize)]
struct AppStoreVersionsResponse {
    #[serde(default)]
    data: Vec<AppStoreVersionResource>,
}

/// A JSON:API single-resource document wrapping one `appStoreVersions`, as
/// returned by the create (POST) endpoint: `{ "data": { ... } }`.
#[derive(Deserialize)]
struct AppStoreVersionDocument {
    data: AppStoreVersionResource,
}

#[derive(Deserialize)]
struct AppStoreVersionResource {
    id: String,
    #[serde(default)]
    attributes: AppStoreVersionResourceAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppStoreVersionResourceAttributes {
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    app_store_state: Option<String>,
    #[serde(default)]
    app_version_state: Option<String>,
    #[serde(default)]
    version_string: Option<String>,
    #[serde(default)]
    copyright: Option<String>,
    #[serde(default)]
    release_type: Option<String>,
    #[serde(default)]
    created_date: Option<String>,
}

impl AppStoreVersionResource {
    fn into_version_info(self, app_id: &str) -> AppStoreVersionInfo {
        AppStoreVersionInfo {
            id: self.id,
            app_id: app_id.to_string(),
            platform: self.attributes.platform,
            app_store_state: self.attributes.app_store_state,
            app_version_state: self.attributes.app_version_state,
            version_string: self.attributes.version_string,
            copyright: self.attributes.copyright,
            release_type: self.attributes.release_type,
            created_date: self.attributes.created_date,
        }
    }
}

/// A JSON:API single-resource document wrapping one
/// `appStoreVersionPhasedReleases`. `data` may be `null`/absent when the version
/// has no phased release (the singular relationship endpoint), so it is
/// optional. The create/update endpoints always populate it.
#[derive(Deserialize)]
struct PhasedReleaseDocument {
    #[serde(default)]
    data: Option<PhasedReleaseResource>,
}

#[derive(Deserialize)]
struct PhasedReleaseResource {
    id: String,
    #[serde(default)]
    attributes: PhasedReleaseResourceAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PhasedReleaseResourceAttributes {
    // The ASC attribute is `phasedReleaseState`; it maps onto
    // `PhasedReleaseInfo.state`.
    #[serde(default)]
    phased_release_state: Option<String>,
    #[serde(default)]
    start_date: Option<String>,
    #[serde(default)]
    total_pause_duration: Option<i32>,
    #[serde(default)]
    current_day_number: Option<i32>,
}

impl PhasedReleaseResource {
    fn into_phased_release_info(self) -> PhasedReleaseInfo {
        PhasedReleaseInfo {
            id: self.id,
            state: self.attributes.phased_release_state,
            start_date: self.attributes.start_date,
            total_pause_duration: self.attributes.total_pause_duration,
            current_day_number: self.attributes.current_day_number,
        }
    }
}

// ---------------------------------------------------------------------------
// Builds (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `builds` resources, with related resources
/// (`preReleaseVersion`, `buildBetaDetail`, `betaAppReviewSubmission`,
/// `betaGroups`, `betaBuildLocalizations`) carried in the heterogeneous
/// `included[]` when requested via `include`.
#[derive(Deserialize)]
struct BuildsResponse {
    #[serde(default)]
    data: Vec<BuildResource>,
    #[serde(default)]
    included: Vec<BuildIncluded>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document wrapping one `builds`, with related
/// resources carried in `included[]`. Used by the build-detail and
/// current-build paths.
#[derive(Deserialize)]
struct BuildDocument {
    #[serde(default)]
    data: Option<BuildResource>,
    #[serde(default)]
    included: Vec<BuildIncluded>,
}

#[derive(Deserialize)]
struct BuildResource {
    id: String,
    #[serde(default)]
    attributes: BuildAttributes,
    #[serde(default)]
    relationships: BuildRelationships,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BuildAttributes {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    uploaded_date: Option<String>,
    #[serde(default)]
    expired: Option<bool>,
    #[serde(default)]
    processing_state: Option<String>,
    #[serde(default)]
    min_os_version: Option<String>,
    #[serde(default)]
    expiration_date: Option<String>,
    #[serde(default)]
    computed_min_mac_os_version: Option<String>,
    #[serde(default)]
    computed_min_vision_os_version: Option<String>,
    #[serde(default)]
    build_audience_type: Option<String>,
    #[serde(default)]
    uses_non_exempt_encryption: Option<bool>,
    #[serde(default)]
    icon_asset_token: Option<IconAssetToken>,
}

/// The build's `iconAssetToken` template object. The icon URL is computed by
/// substituting `{w}`/`{h}`/`{f}` placeholders in `template_url`.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IconAssetToken {
    #[serde(default)]
    template_url: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

impl IconAssetToken {
    /// Computes the concrete icon URL by substituting the `{w}`, `{h}`, and `{f}`
    /// placeholders in `template_url` (defaults: width/height `512`, format
    /// `png`). Returns `None` when no template URL is present. Mirrors the iOS
    /// `toIconUrl()` helper.
    fn to_icon_url(&self) -> Option<String> {
        let template = self.template_url.as_deref()?;
        let width = self.width.unwrap_or(512);
        let height = self.height.unwrap_or(512);
        Some(
            template
                .replace("{w}", &width.to_string())
                .replace("{h}", &height.to_string())
                .replace("{f}", "png"),
        )
    }
}

/// The `builds` to-one and to-many relationships we resolve against `included`.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BuildRelationships {
    #[serde(default)]
    pre_release_version: ToOneRelationship,
    #[serde(default)]
    build_beta_detail: ToOneRelationship,
    #[serde(default)]
    beta_app_review_submission: ToOneRelationship,
    #[serde(default)]
    beta_groups: ToManyRelationship,
    #[serde(default)]
    beta_build_localizations: ToManyRelationship,
}

/// A JSON:API to-many relationship: `{ "data": [{ "type": ..., "id": ... }, ...] }`.
#[derive(Deserialize, Default)]
struct ToManyRelationship {
    #[serde(default)]
    data: Vec<ResourceIdentifier>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PreReleaseVersionAttributes {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    platform: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BuildBetaDetailAttributes {
    #[serde(default)]
    external_build_state: Option<String>,
    #[serde(default)]
    internal_build_state: Option<String>,
    #[serde(default)]
    auto_notify_enabled: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BetaAppReviewSubmissionAttributes {
    #[serde(default)]
    beta_review_state: Option<String>,
    #[serde(default)]
    submitted_date: Option<String>,
}

/// The heterogeneous `included[]` entries of a build document, dispatched by
/// their JSON:API `type`. Unknown types deserialize to [`BuildIncluded::Other`]
/// and are ignored.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum BuildIncluded {
    #[serde(rename = "preReleaseVersions")]
    PreReleaseVersions {
        id: String,
        #[serde(default)]
        attributes: PreReleaseVersionAttributes,
    },
    #[serde(rename = "buildBetaDetails")]
    BuildBetaDetails {
        id: String,
        #[serde(default)]
        attributes: BuildBetaDetailAttributes,
    },
    #[serde(rename = "betaAppReviewSubmissions")]
    BetaAppReviewSubmissions {
        id: String,
        #[serde(default)]
        attributes: BetaAppReviewSubmissionAttributes,
    },
    #[serde(rename = "betaGroups")]
    BetaGroups(BetaGroupResource),
    #[serde(rename = "betaBuildLocalizations")]
    BetaBuildLocalizations(BetaBuildLocalizationResource),
    #[serde(other)]
    Other,
}

/// Typed index of a build document's `included[]`, keyed by resource id, so each
/// of a build's relationship ids can be resolved into the enrichment it carries.
#[derive(Default)]
struct IncludedIndex {
    pre_release_versions: HashMap<String, PreReleaseVersionAttributes>,
    build_beta_details: HashMap<String, BuildBetaDetailAttributes>,
    beta_app_review_submissions: HashMap<String, BetaAppReviewSubmissionAttributes>,
    beta_groups: HashMap<String, BetaGroupResource>,
    beta_build_localizations: HashMap<String, BetaBuildLocalizationResource>,
}

impl IncludedIndex {
    /// Builds the index by consuming the heterogeneous `included[]`, routing each
    /// entry into its per-type map and discarding unknown types.
    fn from_included(included: Vec<BuildIncluded>) -> Self {
        let mut index = Self::default();
        for resource in included {
            match resource {
                BuildIncluded::PreReleaseVersions { id, attributes } => {
                    index.pre_release_versions.insert(id, attributes);
                }
                BuildIncluded::BuildBetaDetails { id, attributes } => {
                    index.build_beta_details.insert(id, attributes);
                }
                BuildIncluded::BetaAppReviewSubmissions { id, attributes } => {
                    index.beta_app_review_submissions.insert(id, attributes);
                }
                BuildIncluded::BetaGroups(resource) => {
                    index.beta_groups.insert(resource.id.clone(), resource);
                }
                BuildIncluded::BetaBuildLocalizations(resource) => {
                    index
                        .beta_build_localizations
                        .insert(resource.id.clone(), resource);
                }
                BuildIncluded::Other => {}
            }
        }
        index
    }
}

/// Maps a build resource into an enriched [`BuildInfo`], resolving each
/// relationship id against `included`. `app_id` is the owning app id, which may
/// be `""` when not known at the call site (the group / detail / current paths).
fn build_info_from(resource: &BuildResource, app_id: &str, included: &IncludedIndex) -> BuildInfo {
    let attributes = &resource.attributes;
    let relationships = &resource.relationships;

    let pre_release = relationships
        .pre_release_version
        .data
        .as_ref()
        .and_then(|rel| included.pre_release_versions.get(&rel.id));
    let beta_detail = relationships
        .build_beta_detail
        .data
        .as_ref()
        .and_then(|rel| included.build_beta_details.get(&rel.id));
    let review_submission = relationships
        .beta_app_review_submission
        .data
        .as_ref()
        .and_then(|rel| included.beta_app_review_submissions.get(&rel.id));

    BuildInfo {
        id: resource.id.clone(),
        app_id: app_id.to_string(),
        version: attributes.version.clone(),
        uploaded_date: attributes.uploaded_date.clone(),
        expired: attributes.expired,
        processing_state: attributes.processing_state.clone(),
        min_os_version: attributes.min_os_version.clone(),
        expiration_date: attributes.expiration_date.clone(),
        marketing_version: pre_release.and_then(|p| p.version.clone()),
        platform: pre_release.and_then(|p| p.platform.clone()),
        external_build_state: beta_detail.and_then(|d| d.external_build_state.clone()),
        internal_build_state: beta_detail.and_then(|d| d.internal_build_state.clone()),
        auto_notify_enabled: beta_detail.and_then(|d| d.auto_notify_enabled),
        beta_review_state: review_submission.and_then(|s| s.beta_review_state.clone()),
        submitted_date: review_submission.and_then(|s| s.submitted_date.clone()),
        computed_min_mac_os_version: attributes.computed_min_mac_os_version.clone(),
        computed_min_vision_os_version: attributes.computed_min_vision_os_version.clone(),
        build_audience_type: attributes.build_audience_type.clone(),
        uses_non_exempt_encryption: attributes.uses_non_exempt_encryption,
        icon_url: attributes
            .icon_asset_token
            .as_ref()
            .and_then(IconAssetToken::to_icon_url),
    }
}

// ---------------------------------------------------------------------------
// Beta groups (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `betaGroups` resources.
#[derive(Deserialize)]
struct BetaGroupsResponse {
    #[serde(default)]
    data: Vec<BetaGroupResource>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document wrapping one `betaGroups`, as returned by
/// the create (POST) and update (PATCH) endpoints: `{ "data": { ... } }`.
#[derive(Deserialize)]
struct BetaGroupDocument {
    data: BetaGroupResource,
}

#[derive(Deserialize)]
struct BetaGroupResource {
    id: String,
    #[serde(default)]
    attributes: BetaGroupAttributes,
    #[serde(default)]
    relationships: BetaGroupRelationships,
}

/// The `betaGroups` relationships we care about. Only `app` is read, and only to
/// recover the owning app id when it is not otherwise known (the update path).
#[derive(Deserialize, Default)]
struct BetaGroupRelationships {
    #[serde(default)]
    app: ToOneRelationship,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BetaGroupAttributes {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    created_date: Option<String>,
    #[serde(default)]
    is_internal_group: Option<bool>,
    #[serde(default)]
    has_access_to_all_builds: Option<bool>,
    #[serde(default)]
    public_link_enabled: Option<bool>,
    #[serde(default)]
    public_link: Option<String>,
    #[serde(default)]
    feedback_enabled: Option<bool>,
}

impl BetaGroupResource {
    fn into_beta_group_info(self, app_id: &str) -> BetaGroupInfo {
        BetaGroupInfo {
            id: self.id,
            app_id: app_id.to_string(),
            name: self.attributes.name,
            created_date: self.attributes.created_date,
            is_internal_group: self.attributes.is_internal_group,
            has_access_to_all_builds: self.attributes.has_access_to_all_builds,
            public_link_enabled: self.attributes.public_link_enabled,
            public_link: self.attributes.public_link,
            feedback_enabled: self.attributes.feedback_enabled,
        }
    }

    /// Borrowing variant of [`Self::into_beta_group_info`] for resources held in
    /// an [`IncludedIndex`] (which cannot be consumed by value).
    fn to_beta_group_info(&self, app_id: &str) -> BetaGroupInfo {
        BetaGroupInfo {
            id: self.id.clone(),
            app_id: app_id.to_string(),
            name: self.attributes.name.clone(),
            created_date: self.attributes.created_date.clone(),
            is_internal_group: self.attributes.is_internal_group,
            has_access_to_all_builds: self.attributes.has_access_to_all_builds,
            public_link_enabled: self.attributes.public_link_enabled,
            public_link: self.attributes.public_link.clone(),
            feedback_enabled: self.attributes.feedback_enabled,
        }
    }

    /// Maps a resource whose owning app id is not known from the call site (the
    /// update path), recovering `app_id` from the JSON:API `relationships.app`
    /// when present and falling back to an empty string otherwise — the ASC
    /// PATCH response carries no app relationship, so this is the simplest
    /// correct behavior without a second round-trip.
    fn into_beta_group_info_inferring_app(self) -> BetaGroupInfo {
        let app_id = self
            .relationships
            .app
            .data
            .as_ref()
            .map(|rel| rel.id.clone())
            .unwrap_or_default();
        self.into_beta_group_info(&app_id)
    }
}

// ---------------------------------------------------------------------------
// Beta testers (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `betaTesters` resources.
#[derive(Deserialize)]
struct BetaTestersResponse {
    #[serde(default)]
    data: Vec<BetaTesterResource>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document wrapping one `betaTesters`, as returned
/// by the create (POST) endpoint: `{ "data": { ... } }`.
#[derive(Deserialize)]
struct BetaTesterDocument {
    data: BetaTesterResource,
}

/// Minimal projection of a JSON:API collection used solely to read the total
/// item count from `meta.paging.total`. The full resource list is ignored, so a
/// `limit=1` request can report the count without materializing every tester.
#[derive(Deserialize, Default)]
struct BetaTestersCountResponse {
    #[serde(default)]
    meta: Option<Meta>,
}

/// JSON:API top-level `meta` object, narrowed to the paging block we read.
#[derive(Deserialize, Default)]
struct Meta {
    #[serde(default)]
    paging: Option<Paging>,
}

/// JSON:API `meta.paging` object, narrowed to the `total` count.
#[derive(Deserialize, Default)]
struct Paging {
    #[serde(default)]
    total: u32,
}

impl BetaTestersCountResponse {
    /// The reported total, defaulting to `0` when `meta`/`paging`/`total` is
    /// absent.
    fn total(&self) -> u32 {
        self.meta
            .as_ref()
            .and_then(|m| m.paging.as_ref())
            .map(|p| p.total)
            .unwrap_or(0)
    }
}

#[derive(Deserialize)]
struct BetaTesterResource {
    id: String,
    #[serde(default)]
    attributes: BetaTesterAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BetaTesterAttributes {
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    invite_type: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

impl BetaTesterResource {
    fn into_beta_tester_info(self) -> BetaTesterInfo {
        BetaTesterInfo {
            id: self.id,
            first_name: self.attributes.first_name,
            last_name: self.attributes.last_name,
            email: self.attributes.email,
            invite_type: self.attributes.invite_type,
            state: self.attributes.state,
        }
    }
}

// ---------------------------------------------------------------------------
// Beta build localizations (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `betaBuildLocalizations` resources.
#[derive(Deserialize)]
struct BetaBuildLocalizationsResponse {
    #[serde(default)]
    data: Vec<BetaBuildLocalizationResource>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document wrapping one `betaBuildLocalizations`, as
/// returned by the create (POST) and update (PATCH) endpoints: `{ "data": { ... } }`.
#[derive(Deserialize)]
struct BetaBuildLocalizationDocument {
    data: BetaBuildLocalizationResource,
}

#[derive(Deserialize)]
struct BetaBuildLocalizationResource {
    id: String,
    #[serde(default)]
    attributes: BetaBuildLocalizationAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BetaBuildLocalizationAttributes {
    #[serde(default)]
    locale: Option<String>,
    #[serde(default)]
    whats_new: Option<String>,
}

impl BetaBuildLocalizationResource {
    fn into_beta_build_localization_info(self) -> BetaBuildLocalizationInfo {
        BetaBuildLocalizationInfo {
            id: self.id,
            locale: self.attributes.locale.unwrap_or_default(),
            whats_new: self.attributes.whats_new,
        }
    }

    /// Borrowing variant of [`Self::into_beta_build_localization_info`] for
    /// resources held in an [`IncludedIndex`] (which cannot be consumed by value).
    fn to_beta_build_localization_info(&self) -> BetaBuildLocalizationInfo {
        BetaBuildLocalizationInfo {
            id: self.id.clone(),
            locale: self.attributes.locale.clone().unwrap_or_default(),
            whats_new: self.attributes.whats_new.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Beta app localizations (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `betaAppLocalizations` resources.
#[derive(Deserialize)]
struct BetaAppLocalizationsResponse {
    #[serde(default)]
    data: Vec<BetaAppLocalizationResource>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document wrapping one `betaAppLocalizations`, as
/// returned by the create (POST) and update (PATCH) endpoints: `{ "data": { ... } }`.
#[derive(Deserialize)]
struct BetaAppLocalizationDocument {
    data: BetaAppLocalizationResource,
}

#[derive(Deserialize)]
struct BetaAppLocalizationResource {
    id: String,
    #[serde(default)]
    attributes: BetaAppLocalizationAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BetaAppLocalizationAttributes {
    #[serde(default)]
    locale: Option<String>,
    #[serde(default)]
    feedback_email: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

impl BetaAppLocalizationResource {
    fn into_beta_app_localization_info(self) -> BetaAppLocalizationInfo {
        BetaAppLocalizationInfo {
            id: self.id,
            locale: self.attributes.locale.unwrap_or_default(),
            feedback_email: self.attributes.feedback_email,
            description: self.attributes.description,
        }
    }
}

// ---------------------------------------------------------------------------
// Accessibility declarations (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `accessibilityDeclarations` resources.
#[derive(Deserialize)]
struct AccessibilityDeclarationsResponse {
    #[serde(default)]
    data: Vec<AccessibilityDeclarationResource>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document wrapping one `accessibilityDeclarations`,
/// as returned by the create (POST) and update (PATCH) endpoints:
/// `{ "data": { ... } }`.
#[derive(Deserialize)]
struct AccessibilityDeclarationDocument {
    data: AccessibilityDeclarationResource,
}

#[derive(Deserialize)]
struct AccessibilityDeclarationResource {
    id: String,
    #[serde(default)]
    attributes: AccessibilityDeclarationAttributes,
}

/// App Store Connect `accessibilityDeclarations` attributes.
///
/// Note the wire key for the host's `supports_differentiate_without_color` is
/// `supportsDifferentiateWithoutColorAlone` (with an `Alone` suffix), mapped
/// explicitly below; the other eight `supports*` keys are 1:1.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AccessibilityDeclarationAttributes {
    #[serde(default)]
    device_family: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    supports_audio_descriptions: Option<bool>,
    #[serde(default)]
    supports_captions: Option<bool>,
    #[serde(default)]
    supports_dark_interface: Option<bool>,
    #[serde(default, rename = "supportsDifferentiateWithoutColorAlone")]
    supports_differentiate_without_color: Option<bool>,
    #[serde(default)]
    supports_larger_text: Option<bool>,
    #[serde(default)]
    supports_reduced_motion: Option<bool>,
    #[serde(default)]
    supports_sufficient_contrast: Option<bool>,
    #[serde(default)]
    supports_voice_control: Option<bool>,
    #[serde(default)]
    supports_voiceover: Option<bool>,
}

impl AccessibilityDeclarationResource {
    fn into_accessibility_declaration_info(self) -> AccessibilityDeclarationInfo {
        let a = self.attributes;
        AccessibilityDeclarationInfo {
            id: self.id,
            device_family: a.device_family.unwrap_or_default(),
            state: a.state,
            supports_audio_descriptions: a.supports_audio_descriptions.unwrap_or(false),
            supports_captions: a.supports_captions.unwrap_or(false),
            supports_dark_interface: a.supports_dark_interface.unwrap_or(false),
            supports_differentiate_without_color: a
                .supports_differentiate_without_color
                .unwrap_or(false),
            supports_larger_text: a.supports_larger_text.unwrap_or(false),
            supports_reduced_motion: a.supports_reduced_motion.unwrap_or(false),
            supports_sufficient_contrast: a.supports_sufficient_contrast.unwrap_or(false),
            supports_voice_control: a.supports_voice_control.unwrap_or(false),
            supports_voiceover: a.supports_voiceover.unwrap_or(false),
        }
    }
}

// ---------------------------------------------------------------------------
// App info localizations (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `appInfoLocalizations` resources.
#[derive(Deserialize)]
struct AppInfoLocalizationsResponse {
    #[serde(default)]
    data: Vec<AppInfoLocalizationResource>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document wrapping one `appInfoLocalizations`, as
/// returned by the create (POST) and update (PATCH) endpoints: `{ "data": { ... } }`.
#[derive(Deserialize)]
struct AppInfoLocalizationDocument {
    data: AppInfoLocalizationResource,
}

#[derive(Deserialize)]
struct AppInfoLocalizationResource {
    id: String,
    #[serde(default)]
    attributes: AppInfoLocalizationAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppInfoLocalizationAttributes {
    #[serde(default)]
    locale: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    subtitle: Option<String>,
    #[serde(default)]
    privacy_policy_url: Option<String>,
    #[serde(default)]
    privacy_choices_url: Option<String>,
    #[serde(default)]
    privacy_policy_text: Option<String>,
}

impl AppInfoLocalizationResource {
    fn into_app_info_localization_info(self) -> AppInfoLocalizationInfo {
        AppInfoLocalizationInfo {
            id: self.id,
            locale: self.attributes.locale.unwrap_or_default(),
            name: self.attributes.name,
            subtitle: self.attributes.subtitle,
            privacy_policy_url: self.attributes.privacy_policy_url,
            privacy_choices_url: self.attributes.privacy_choices_url,
            privacy_policy_text: self.attributes.privacy_policy_text,
        }
    }

    /// Borrowing variant of [`Self::into_app_info_localization_info`] for
    /// resources held in an `included` index (which cannot be consumed by value).
    fn to_app_info_localization_info(&self) -> AppInfoLocalizationInfo {
        AppInfoLocalizationInfo {
            id: self.id.clone(),
            locale: self.attributes.locale.clone().unwrap_or_default(),
            name: self.attributes.name.clone(),
            subtitle: self.attributes.subtitle.clone(),
            privacy_policy_url: self.attributes.privacy_policy_url.clone(),
            privacy_choices_url: self.attributes.privacy_choices_url.clone(),
            privacy_policy_text: self.attributes.privacy_policy_text.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// App Store version localizations (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `appStoreVersionLocalizations` resources.
#[derive(Deserialize)]
struct AppStoreLocalizationsResponse {
    #[serde(default)]
    data: Vec<AppStoreLocalizationResource>,
    #[serde(default)]
    links: Links,
}

#[derive(Deserialize)]
struct AppStoreLocalizationResource {
    id: String,
    #[serde(default)]
    attributes: AppStoreLocalizationAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppStoreLocalizationAttributes {
    #[serde(default)]
    locale: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    keywords: Option<String>,
    #[serde(default)]
    promotional_text: Option<String>,
    #[serde(default)]
    support_url: Option<String>,
    #[serde(default)]
    marketing_url: Option<String>,
    #[serde(default)]
    whats_new: Option<String>,
}

impl AppStoreLocalizationResource {
    fn into_app_store_localization_info(self) -> AppStoreLocalizationInfo {
        AppStoreLocalizationInfo {
            id: self.id,
            locale: self.attributes.locale,
            description: self.attributes.description,
            keywords: self.attributes.keywords,
            promotional_text: self.attributes.promotional_text,
            support_url: self.attributes.support_url,
            marketing_url: self.attributes.marketing_url,
            whats_new: self.attributes.whats_new,
        }
    }
}

// ---------------------------------------------------------------------------
// App Store screenshots (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `appScreenshotSets` resources, with the related
/// `appScreenshots` carried in `included[]`.
#[derive(Deserialize)]
struct AppScreenshotSetsResponse {
    #[serde(default)]
    data: Vec<AppScreenshotSetResource>,
    #[serde(default)]
    included: Vec<AppScreenshotIncluded>,
    #[serde(default)]
    links: Links,
}

#[derive(Deserialize)]
struct AppScreenshotSetResource {
    id: String,
    #[serde(default)]
    attributes: AppScreenshotSetAttributes,
    #[serde(default)]
    relationships: AppScreenshotSetRelationships,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppScreenshotSetAttributes {
    #[serde(default)]
    screenshot_display_type: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppScreenshotSetRelationships {
    #[serde(default)]
    app_screenshots: ToManyRelationship,
}

#[derive(Deserialize)]
struct AppScreenshotResource {
    id: String,
    #[serde(default)]
    attributes: AppScreenshotAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppScreenshotAttributes {
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    file_size: Option<i32>,
    #[serde(default)]
    image_asset: Option<ImageAsset>,
}

/// A screenshot's `imageAsset` template object. The image URL is computed by
/// substituting `{w}`/`{h}`/`{f}` placeholders in `template_url`, reusing the
/// same substitution rules as the build [`IconAssetToken`].
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ImageAsset {
    #[serde(default)]
    template_url: Option<String>,
    #[serde(default)]
    width: Option<i32>,
    #[serde(default)]
    height: Option<i32>,
}

impl ImageAsset {
    /// Computes the concrete image URL by substituting the `{w}`, `{h}`, and
    /// `{f}` placeholders in `template_url` (defaults: width/height `512`, format
    /// `png`). Returns `None` when no template URL is present. Mirrors
    /// [`IconAssetToken::to_icon_url`].
    fn to_image_url(&self) -> Option<String> {
        let template = self.template_url.as_deref()?;
        let width = self.width.unwrap_or(512);
        let height = self.height.unwrap_or(512);
        Some(
            template
                .replace("{w}", &width.to_string())
                .replace("{h}", &height.to_string())
                .replace("{f}", "png"),
        )
    }
}

/// The heterogeneous `included[]` entries of an `appScreenshotSets` document.
/// Only `appScreenshots` carry data we resolve; unknown types deserialize to
/// [`AppScreenshotIncluded::Other`] and are ignored.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum AppScreenshotIncluded {
    #[serde(rename = "appScreenshots")]
    AppScreenshots(AppScreenshotResource),
    #[serde(other)]
    Other,
}

impl AppScreenshotResource {
    /// Maps a screenshot resource into a [`ScreenshotInfo`], computing `image_url`
    /// from its `imageAsset` template and pulling `width`/`height` from the same.
    fn to_screenshot_info(&self) -> ScreenshotInfo {
        let image_asset = self.attributes.image_asset.as_ref();
        ScreenshotInfo {
            id: self.id.clone(),
            image_url: image_asset.and_then(ImageAsset::to_image_url),
            file_name: self.attributes.file_name.clone(),
            file_size: self.attributes.file_size,
            width: image_asset.and_then(|asset| asset.width),
            height: image_asset.and_then(|asset| asset.height),
        }
    }
}

// ---------------------------------------------------------------------------
// App infos (JSON:API) — full App Info detail (categories + age rating)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `appInfos` resources, with related
/// `appInfoLocalizations` and `ageRatingDeclarations` carried in the
/// heterogeneous `included[]` when requested via `include`. The category
/// relationships are read directly from each app-info resource's
/// `relationships`, so the category resources themselves need not be parsed.
#[derive(Deserialize)]
struct AppInfosResponse {
    #[serde(default)]
    data: Vec<AppInfoResource>,
    #[serde(default)]
    included: Vec<AppInfoIncluded>,
}

#[derive(Deserialize)]
struct AppInfoResource {
    id: String,
    #[serde(default)]
    attributes: AppInfoResourceAttributes,
    #[serde(default)]
    relationships: AppInfoResourceRelationships,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppInfoResourceAttributes {
    #[serde(default)]
    app_store_age_rating: Option<String>,
}

/// The `appInfos` relationships we resolve: the four category to-one links and
/// the age-rating declaration to-one link.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppInfoResourceRelationships {
    #[serde(default)]
    primary_category: ToOneRelationship,
    #[serde(default)]
    primary_subcategory_one: ToOneRelationship,
    #[serde(default)]
    secondary_category: ToOneRelationship,
    #[serde(default)]
    secondary_subcategory_one: ToOneRelationship,
    #[serde(default)]
    age_rating_declaration: ToOneRelationship,
}

/// The heterogeneous `included[]` entries of an app-info document, dispatched by
/// their JSON:API `type`. Only `appInfoLocalizations` and `ageRatingDeclarations`
/// are read; unknown types (e.g. `appCategories`) deserialize to
/// [`AppInfoIncluded::Other`] and are ignored.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum AppInfoIncluded {
    #[serde(rename = "appInfoLocalizations")]
    AppInfoLocalizations(AppInfoLocalizationResource),
    #[serde(rename = "ageRatingDeclarations")]
    AgeRatingDeclarations(AgeRatingDeclarationResource),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct AgeRatingDeclarationResource {
    id: String,
    #[serde(default)]
    attributes: AgeRatingDeclarationAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AgeRatingDeclarationAttributes {
    #[serde(default)]
    alcohol_tobacco_or_drug_use_or_references: Option<String>,
    #[serde(default)]
    contests: Option<String>,
    #[serde(default)]
    gambling_simulated: Option<String>,
    #[serde(default)]
    guns_or_other_weapons: Option<String>,
    #[serde(default)]
    medical_or_treatment_information: Option<String>,
    #[serde(default)]
    profanity_or_crude_humor: Option<String>,
    #[serde(default)]
    sexual_content_graphic_and_nudity: Option<String>,
    #[serde(default)]
    sexual_content_or_nudity: Option<String>,
    #[serde(default)]
    horror_or_fear_themes: Option<String>,
    #[serde(default)]
    mature_or_suggestive_themes: Option<String>,
    #[serde(default)]
    violence_cartoon_or_fantasy: Option<String>,
    #[serde(default)]
    violence_realistic: Option<String>,
    #[serde(default)]
    violence_realistic_prolonged_graphic_or_sadistic: Option<String>,
    #[serde(default)]
    is_advertising: Option<bool>,
    #[serde(default)]
    is_gambling: Option<bool>,
    #[serde(default)]
    is_unrestricted_web_access: Option<bool>,
    #[serde(default)]
    is_user_generated_content: Option<bool>,
    #[serde(default)]
    age_rating_override_v2: Option<String>,
}

impl AgeRatingDeclarationResource {
    fn into_age_rating_declaration_info(self) -> AgeRatingDeclarationInfo {
        let a = self.attributes;
        AgeRatingDeclarationInfo {
            id: self.id,
            alcohol_tobacco_or_drug_use_or_references: a.alcohol_tobacco_or_drug_use_or_references,
            contests: a.contests,
            gambling_simulated: a.gambling_simulated,
            guns_or_other_weapons: a.guns_or_other_weapons,
            medical_or_treatment_information: a.medical_or_treatment_information,
            profanity_or_crude_humor: a.profanity_or_crude_humor,
            sexual_content_graphic_and_nudity: a.sexual_content_graphic_and_nudity,
            sexual_content_or_nudity: a.sexual_content_or_nudity,
            horror_or_fear_themes: a.horror_or_fear_themes,
            mature_or_suggestive_themes: a.mature_or_suggestive_themes,
            violence_cartoon_or_fantasy: a.violence_cartoon_or_fantasy,
            violence_realistic: a.violence_realistic,
            violence_realistic_prolonged_graphic_or_sadistic: a
                .violence_realistic_prolonged_graphic_or_sadistic,
            is_advertising: a.is_advertising,
            is_gambling: a.is_gambling,
            is_unrestricted_web_access: a.is_unrestricted_web_access,
            is_user_generated_content: a.is_user_generated_content,
            age_rating_override_v2: a.age_rating_override_v2,
        }
    }
}

/// A JSON:API single-resource document of one `apps`, narrowed to the
/// `sku`/`primaryLocale`/`contentRightsDeclaration` attributes the App Info
/// detail merges in: `{ "data": { ... } }`.
#[derive(Deserialize)]
struct AppDetailDocument {
    data: AppDetailResource,
}

#[derive(Deserialize)]
struct AppDetailResource {
    #[serde(default)]
    attributes: AppDetailAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppDetailAttributes {
    #[serde(default)]
    sku: Option<String>,
    #[serde(default)]
    primary_locale: Option<String>,
    #[serde(default)]
    content_rights_declaration: Option<String>,
}

// ---------------------------------------------------------------------------
// App categories (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `appCategories` resources. The subcategory ids
/// are read from each top-level category's `relationships.subcategories`; the
/// subcategory resources carried in `included[]` are not parsed (their ids are
/// already present in the parent's relationships).
#[derive(Deserialize)]
struct AppCategoriesResponse {
    #[serde(default)]
    data: Vec<AppCategoryResource>,
    #[serde(default)]
    links: Links,
}

#[derive(Deserialize)]
struct AppCategoryResource {
    id: String,
    #[serde(default)]
    relationships: AppCategoryRelationships,
}

#[derive(Deserialize, Default)]
struct AppCategoryRelationships {
    #[serde(default)]
    subcategories: ToManyRelationship,
}

impl AppCategoryResource {
    fn into_app_category_info(self) -> AppCategoryInfo {
        AppCategoryInfo {
            id: self.id,
            subcategory_ids: self
                .relationships
                .subcategories
                .data
                .into_iter()
                .map(|rel| rel.id)
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Beta app review detail (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API single-resource document wrapping one `betaAppReviewDetails`, as
/// returned by the app-relationship fetch (`GET`) and the update (`PATCH`)
/// endpoints: `{ "data": { ... } }`. App Store Connect exposes exactly one beta
/// app review detail per app, so this is a single object, not a list.
#[derive(Deserialize)]
struct BetaAppReviewDetailDocument {
    data: BetaAppReviewDetailResource,
}

#[derive(Deserialize)]
struct BetaAppReviewDetailResource {
    id: String,
    #[serde(default)]
    attributes: BetaAppReviewDetailAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BetaAppReviewDetailAttributes {
    #[serde(default)]
    contact_first_name: Option<String>,
    #[serde(default)]
    contact_last_name: Option<String>,
    #[serde(default)]
    contact_email: Option<String>,
    #[serde(default)]
    contact_phone: Option<String>,
    #[serde(default)]
    demo_account_name: Option<String>,
    #[serde(default)]
    demo_account_password: Option<String>,
    #[serde(default)]
    is_demo_account_required: Option<bool>,
    #[serde(default)]
    notes: Option<String>,
}

impl BetaAppReviewDetailResource {
    fn into_beta_app_review_detail_info(self) -> BetaAppReviewDetailInfo {
        BetaAppReviewDetailInfo {
            id: self.id,
            contact_first_name: self.attributes.contact_first_name,
            contact_last_name: self.attributes.contact_last_name,
            contact_email: self.attributes.contact_email,
            contact_phone: self.attributes.contact_phone,
            demo_account_name: self.attributes.demo_account_name,
            demo_account_password: self.attributes.demo_account_password,
            is_demo_account_required: self.attributes.is_demo_account_required,
            notes: self.attributes.notes,
        }
    }
}

// ---------------------------------------------------------------------------
// App review detail (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API single-resource document wrapping one `appStoreReviewDetails`, as
/// returned by the version-relationship fetch (`GET`) and the update (`PATCH`)
/// endpoints: `{ "data": { ... } }`. App Store Connect exposes at most one app
/// review detail per version, so this is a single object, not a list; `data`
/// may be null/absent when none is attached.
#[derive(Deserialize)]
struct AppReviewDetailDocument {
    #[serde(default)]
    data: Option<AppReviewDetailResource>,
}

#[derive(Deserialize)]
struct AppReviewDetailResource {
    id: String,
    #[serde(default)]
    attributes: AppReviewDetailAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppReviewDetailAttributes {
    #[serde(default)]
    contact_first_name: Option<String>,
    #[serde(default)]
    contact_last_name: Option<String>,
    #[serde(default)]
    contact_email: Option<String>,
    #[serde(default)]
    contact_phone: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    demo_account_name: Option<String>,
    #[serde(default)]
    demo_account_password: Option<String>,
    #[serde(default)]
    is_demo_account_required: Option<bool>,
}

impl AppReviewDetailResource {
    fn into_app_review_detail_info(self) -> AppReviewDetailInfo {
        AppReviewDetailInfo {
            id: self.id,
            contact_first_name: self.attributes.contact_first_name,
            contact_last_name: self.attributes.contact_last_name,
            contact_email: self.attributes.contact_email,
            contact_phone: self.attributes.contact_phone,
            notes: self.attributes.notes,
            demo_account_name: self.attributes.demo_account_name,
            demo_account_password: self.attributes.demo_account_password,
            is_demo_account_required: self.attributes.is_demo_account_required,
        }
    }
}

// ---------------------------------------------------------------------------
// Users & user invitations (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `users` resources.
#[derive(Deserialize)]
struct UsersResponse {
    #[serde(default)]
    data: Vec<UserResource>,
    #[serde(default)]
    links: Links,
}

#[derive(Deserialize)]
struct UserResource {
    id: String,
    #[serde(default)]
    attributes: UserAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct UserAttributes {
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    /// App Store Connect stores the member's login email in `username`.
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    all_apps_visible: Option<bool>,
    #[serde(default)]
    provisioning_allowed: Option<bool>,
}

impl UserResource {
    fn into_team_member_info(self) -> TeamMemberInfo {
        TeamMemberInfo {
            id: self.id,
            first_name: self.attributes.first_name,
            last_name: self.attributes.last_name,
            username: self.attributes.username,
            roles: self.attributes.roles,
        }
    }

    /// Maps an active member into the unified [`UserInfo`]: `email` is taken from
    /// the `username` attribute, `is_pending` is `false`, and `expiration_date`
    /// is always `None`.
    fn into_active_user_info(self) -> UserInfo {
        UserInfo {
            id: self.id,
            first_name: self.attributes.first_name,
            last_name: self.attributes.last_name,
            email: self.attributes.username,
            roles: self.attributes.roles,
            all_apps_visible: self.attributes.all_apps_visible.unwrap_or(false),
            provisioning_allowed: self.attributes.provisioning_allowed.unwrap_or(false),
            is_pending: false,
            expiration_date: None,
        }
    }
}

/// A JSON:API document page of `userInvitations` resources.
#[derive(Deserialize)]
struct UserInvitationsResponse {
    #[serde(default)]
    data: Vec<UserInvitationResource>,
    #[serde(default)]
    links: Links,
}

#[derive(Deserialize)]
struct UserInvitationResource {
    id: String,
    #[serde(default)]
    attributes: UserInvitationAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct UserInvitationAttributes {
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    all_apps_visible: Option<bool>,
    #[serde(default)]
    provisioning_allowed: Option<bool>,
    #[serde(default)]
    expiration_date: Option<String>,
}

impl UserInvitationResource {
    /// Maps a pending invitation into the unified [`UserInfo`]: `email` and
    /// `expiration_date` come from the invitation's own attributes and
    /// `is_pending` is `true`.
    fn into_pending_user_info(self) -> UserInfo {
        UserInfo {
            id: self.id,
            first_name: self.attributes.first_name,
            last_name: self.attributes.last_name,
            email: self.attributes.email,
            roles: self.attributes.roles,
            all_apps_visible: self.attributes.all_apps_visible.unwrap_or(false),
            provisioning_allowed: self.attributes.provisioning_allowed.unwrap_or(false),
            is_pending: true,
            expiration_date: self.attributes.expiration_date,
        }
    }
}

// ---------------------------------------------------------------------------
// Devices (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `devices` resources.
#[derive(Deserialize)]
struct DevicesResponse {
    #[serde(default)]
    data: Vec<DeviceResource>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document of a `devices` resource (create/update
/// responses).
#[derive(Deserialize)]
struct DeviceDocument {
    data: DeviceResource,
}

#[derive(Deserialize)]
struct DeviceResource {
    id: String,
    #[serde(default)]
    attributes: DeviceAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DeviceAttributes {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    udid: Option<String>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    device_class: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    status: Option<String>,
    /// Raw ISO8601 string passed through verbatim — the core does no date parsing.
    #[serde(default)]
    added_date: Option<String>,
}

impl DeviceResource {
    /// Maps a `devices` resource into [`DeviceInfo`], applying the non-optional
    /// fallbacks: `name` → `""` and `status` → `"ENABLED"` when the attribute is
    /// absent.
    fn into_device_info(self) -> DeviceInfo {
        DeviceInfo {
            id: self.id,
            name: self.attributes.name.unwrap_or_default(),
            udid: self.attributes.udid,
            platform: self.attributes.platform,
            device_class: self.attributes.device_class,
            model: self.attributes.model,
            status: self
                .attributes
                .status
                .unwrap_or_else(|| "ENABLED".to_string()),
            added_date: self.attributes.added_date,
        }
    }
}

// ---------------------------------------------------------------------------
// Bundle IDs (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `bundleIds` resources.
#[derive(Deserialize)]
struct BundleIdsResponse {
    #[serde(default)]
    data: Vec<BundleIdResource>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document of a `bundleIds` resource (create/update
/// responses).
#[derive(Deserialize)]
struct BundleIdDocument {
    data: BundleIdResource,
}

#[derive(Deserialize)]
struct BundleIdResource {
    id: String,
    #[serde(default)]
    attributes: BundleIdAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BundleIdAttributes {
    #[serde(default)]
    identifier: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    seed_id: Option<String>,
}

impl BundleIdResource {
    /// Maps a `bundleIds` resource into [`BundleIdInfo`], applying the empty-string
    /// fallbacks for the non-optional `identifier`/`name`/`platform` attributes.
    fn into_bundle_id_info(self) -> BundleIdInfo {
        BundleIdInfo {
            id: self.id,
            identifier: self.attributes.identifier.unwrap_or_default(),
            name: self.attributes.name.unwrap_or_default(),
            platform: self.attributes.platform.unwrap_or_default(),
            seed_id: self.attributes.seed_id,
        }
    }
}

/// A JSON:API document page of `bundleIdCapabilities` resources.
#[derive(Deserialize)]
struct BundleIdCapabilitiesResponse {
    #[serde(default)]
    data: Vec<BundleIdCapabilityResource>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document of a `bundleIdCapabilities` resource
/// (enable response).
#[derive(Deserialize)]
struct BundleIdCapabilityDocument {
    data: BundleIdCapabilityResource,
}

#[derive(Deserialize)]
struct BundleIdCapabilityResource {
    id: String,
    #[serde(default)]
    attributes: BundleIdCapabilityAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BundleIdCapabilityAttributes {
    /// Read as a plain string: App Store Connect keeps adding values
    /// (e.g. `FONT_INSTALLATION`, `CARPLAY_CHARGING`), so this is never an enum.
    #[serde(default)]
    capability_type: Option<String>,
}

impl BundleIdCapabilityResource {
    /// Maps a `bundleIdCapabilities` resource into [`BundleIdCapabilityInfo`],
    /// returning `None` when `capabilityType` is missing or empty so the caller
    /// can skip it.
    fn into_capability_info(self) -> Option<BundleIdCapabilityInfo> {
        let capability_type = self.attributes.capability_type.filter(|t| !t.is_empty())?;
        Some(BundleIdCapabilityInfo {
            id: self.id,
            capability_type,
        })
    }
}

// ---------------------------------------------------------------------------
// Certificates (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `certificates` resources.
#[derive(Deserialize)]
struct CertificatesResponse {
    #[serde(default)]
    data: Vec<CertificateResource>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document of a `certificates` resource (create and
/// single-fetch responses).
#[derive(Deserialize)]
struct CertificateDocument {
    data: CertificateResource,
}

#[derive(Deserialize)]
struct CertificateResource {
    id: String,
    #[serde(default)]
    attributes: CertificateAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CertificateAttributes {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// Read as a plain string: App Store Connect keeps adding `CertificateType`
    /// values, so this is never an enum.
    #[serde(default)]
    certificate_type: Option<String>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    serial_number: Option<String>,
    /// Raw ISO8601 string passed through verbatim — the core does no date parsing.
    #[serde(default)]
    expiration_date: Option<String>,
    /// NB: the wire key is `activated`, not `isActivated`; `rename` overrides the
    /// container-level `camelCase` rule.
    #[serde(rename = "activated", default)]
    activated: bool,
    /// Base64-encoded certificate payload. Absent on list pages, present after a
    /// create or single-resource fetch.
    #[serde(default)]
    certificate_content: Option<String>,
}

impl CertificateResource {
    /// Maps a `certificates` resource into [`CertificateInfo`], applying the
    /// empty-string fallbacks for the non-optional
    /// `display_name`/`name`/`certificate_type` attributes and mapping the wire
    /// `activated` flag onto `is_activated`.
    fn into_certificate_info(self) -> CertificateInfo {
        CertificateInfo {
            id: self.id,
            display_name: self.attributes.display_name.unwrap_or_default(),
            name: self.attributes.name.unwrap_or_default(),
            certificate_type: self.attributes.certificate_type.unwrap_or_default(),
            platform: self.attributes.platform,
            serial_number: self.attributes.serial_number,
            expiration_date: self.attributes.expiration_date,
            is_activated: self.attributes.activated,
            certificate_content: self.attributes.certificate_content,
        }
    }
}

// ---------------------------------------------------------------------------
// Provisioning profiles (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `profiles` resources, with the referenced
/// `bundleIds` carried in `included[]` so each profile's bundle identifier can
/// be resolved.
#[derive(Deserialize)]
struct ProfilesResponse {
    #[serde(default)]
    data: Vec<ProfileResource>,
    #[serde(default)]
    included: Vec<ProfileIncluded>,
    #[serde(default)]
    links: Links,
}

/// A JSON:API single-resource document of a `profiles` resource (create and
/// single-content-fetch responses).
#[derive(Deserialize)]
struct ProfileDocument {
    data: ProfileResource,
}

#[derive(Deserialize)]
struct ProfileResource {
    id: String,
    #[serde(default)]
    attributes: ProfileAttributes,
    #[serde(default)]
    relationships: ProfileRelationships,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProfileAttributes {
    #[serde(default)]
    name: Option<String>,
    /// Read as a plain string: App Store Connect keeps adding `ProfileType`
    /// values, so this is never an enum.
    #[serde(default)]
    profile_type: Option<String>,
    #[serde(default)]
    profile_state: Option<String>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
    /// Raw ISO8601 string passed through verbatim — the core does no date parsing.
    #[serde(default)]
    created_date: Option<String>,
    /// Raw ISO8601 string passed through verbatim — the core does no date parsing.
    #[serde(default)]
    expiration_date: Option<String>,
    /// Base64-encoded `.mobileprovision` payload. Absent on list pages, present
    /// after a create or single-resource content fetch.
    #[serde(default)]
    profile_content: Option<String>,
}

/// The `profiles` relationships we resolve: only the to-one `bundleId`, used to
/// look up the profile's bundle identifier in `included[]`.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProfileRelationships {
    #[serde(default)]
    bundle_id: ToOneRelationship,
}

/// The heterogeneous `included[]` entries of a profiles document, dispatched by
/// their JSON:API `type`. Only `bundleIds` are resolved; unknown types
/// deserialize to [`ProfileIncluded::Other`] and are ignored.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum ProfileIncluded {
    #[serde(rename = "bundleIds")]
    BundleIds {
        id: String,
        #[serde(default)]
        attributes: ProfileBundleIdAttributes,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProfileBundleIdAttributes {
    #[serde(default)]
    identifier: Option<String>,
}

impl ProfileResource {
    /// Maps a `profiles` resource into [`ProvisioningProfileInfo`], applying the
    /// empty-string fallbacks for the non-optional `name`/`profile_type`/
    /// `profile_state` attributes. `bundle_id` is resolved from `included_bundle_ids`
    /// (a map of `bundleIds` id → `identifier`) via the profile's `bundleId`
    /// relationship; it is `None` when the relationship or the referenced bundle
    /// ID is absent.
    fn into_profile_info(
        self,
        included_bundle_ids: &HashMap<String, Option<String>>,
    ) -> ProvisioningProfileInfo {
        let bundle_id = self
            .relationships
            .bundle_id
            .data
            .as_ref()
            .and_then(|rel| included_bundle_ids.get(&rel.id))
            .cloned()
            .flatten();

        ProvisioningProfileInfo {
            id: self.id,
            name: self.attributes.name.unwrap_or_default(),
            profile_type: self.attributes.profile_type.unwrap_or_default(),
            profile_state: self.attributes.profile_state.unwrap_or_default(),
            platform: self.attributes.platform,
            uuid: self.attributes.uuid,
            bundle_id,
            created_date: self.attributes.created_date,
            expiration_date: self.attributes.expiration_date,
            profile_content: self.attributes.profile_content,
        }
    }
}

/// Minimal App Store Connect client: validate credentials and list apps.
/// `base_url` is injectable so tests can point it at a mock server.
pub(crate) struct AppStoreClient {
    base_url: String,
    http: reqwest::Client,
    auth: AppStoreAuthenticator,
    /// Optional HTTP tracing sink. `None` by default; set via
    /// [`Self::with_debug_logger`] when the host injects a logger through
    /// `connect`. When present, [`Self::send_and_read`] logs every request as a
    /// runnable cURL and every response.
    debug_logger: Option<Arc<dyn DebugLogger>>,
}

impl AppStoreClient {
    pub(crate) fn new(auth: AppStoreAuthenticator) -> Self {
        Self::with_base_url(auth, DEFAULT_BASE_URL.to_string())
    }

    pub(crate) fn with_base_url(auth: AppStoreAuthenticator, base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            auth,
            debug_logger: None,
        }
    }

    /// Attaches an optional HTTP tracing sink. Builder-style so existing callers
    /// that only need `new(auth)` stay unchanged; `None` is a no-op (no logging).
    pub(crate) fn with_debug_logger(mut self, logger: Option<Arc<dyn DebugLogger>>) -> Self {
        self.debug_logger = logger;
        self
    }

    /// Sends `builder`, returning the response status and body text. When a
    /// [`Self::debug_logger`] is present, logs the outgoing request as a runnable
    /// cURL (with pretty-printed JSON body) before sending and the response
    /// (status + pretty-printed JSON body) after.
    ///
    /// This is the single choke point every HTTP call routes through, so logging
    /// lives in one place and the send/read boilerplate is not duplicated.
    ///
    /// # Errors
    /// [`StackError::Network`] on transport failure (sending or reading the body)
    /// — the same mapping the call sites used before this method existed.
    async fn send_and_read(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<(reqwest::StatusCode, String), StackError> {
        if let Some(logger) = &self.debug_logger {
            // `try_clone` fails only for streaming bodies; the client uses
            // in-memory `.json()`/empty bodies, so this clones in practice.
            if let Some(req) = builder.try_clone().and_then(|b| b.build().ok()) {
                logger.log(render_curl(&req));
            }
        }

        let response = builder
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        if let Some(logger) = &self.debug_logger {
            logger.log(render_response(status, &body));
        }

        Ok((status, body))
    }

    /// Cheap credential check: `GET /v1/apps?limit=1`.
    ///
    /// # Errors
    /// [`StackError::Auth`] on rejection — a 403 mentioning pending agreements is
    /// surfaced with an explanatory message; otherwise the raw status/body.
    pub(crate) async fn validate(&self) -> Result<(), StackError> {
        let url = format!("{}/v1/apps?limit=1", self.base_url);
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.get(&url).bearer_auth(token))
            .await?;
        if status.is_success() {
            return Ok(());
        }

        Err(map_error_response(status.as_u16(), &body))
    }

    /// Lists every app visible to the account, following `links.next` pagination.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        let mut apps = Vec::new();
        let mut next_url = Some(format!("{}/v1/apps", self.base_url));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: AppsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("apps response: {e}")))?;
            apps.extend(page.data.into_iter().map(AppResource::into_app_info));

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(apps)
    }

    /// Lists the end-user reviews for `app_id`, newest first, attaching any
    /// developer response carried in the JSON:API `included[]` section.
    ///
    /// `GET /v1/apps/{app_id}/customerReviews?sort=-createdDate&include=response&limit=50`,
    /// following `links.next` pagination.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_customer_reviews(
        &self,
        app_id: &str,
    ) -> Result<Vec<CustomerReview>, StackError> {
        let mut reviews = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/apps/{app_id}/customerReviews?sort=-createdDate&include=response&limit=50",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: CustomerReviewsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("customer reviews response: {e}")))?;

            // Index responses from `included[]` by id, then attach by relationship.
            let responses: HashMap<String, ReviewResponse> = page
                .included
                .into_iter()
                .map(|r| (r.id.clone(), r.into_review_response()))
                .collect();

            reviews.extend(page.data.into_iter().map(|review| {
                let response = review
                    .relationships
                    .response
                    .data
                    .as_ref()
                    .and_then(|rel| responses.get(&rel.id).cloned());
                review.into_customer_review(response)
            }));

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(reviews)
    }

    /// Fetches a SINGLE page of customer reviews for incremental (load-more)
    /// paging, returning the mapped reviews plus an opaque `next_token`.
    ///
    /// When `page_token` is `Some(url)` the URL — a prior call's `next_token`,
    /// itself the JSON:API `links.next` (absolute, already encoding sort/filter/
    /// cursor) — is fetched verbatim. Otherwise the first page is built from
    /// `app_id`, `sort` (passed through as the raw ASC value — not remapped),
    /// `limit`, and, when `filter_rating` is non-empty, a comma-joined
    /// `filter[rating]`. Unlike [`Self::fetch_customer_reviews`], `links.next` is
    /// NOT followed — its value is returned as `next_token` (`None` on the last
    /// page) for the caller to pass back.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub(crate) async fn fetch_customer_reviews_page(
        &self,
        app_id: &str,
        sort: &str,
        filter_rating: &[String],
        limit: u32,
        page_token: Option<&str>,
    ) -> Result<CustomerReviewsPage, StackError> {
        let url = match page_token {
            Some(token) => token.to_string(),
            None => {
                let mut url = format!(
                    "{}/v1/apps/{app_id}/customerReviews?sort={sort}&include=response&limit={limit}",
                    self.base_url
                );
                if !filter_rating.is_empty() {
                    url.push_str("&filter[rating]=");
                    url.push_str(&filter_rating.join(","));
                }
                url
            }
        };

        let body = self.get_page(&url).await?;
        let page: CustomerReviewsResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("customer reviews response: {e}")))?;

        // Index responses from `included[]` by id, then attach by relationship.
        let responses: HashMap<String, ReviewResponse> = page
            .included
            .into_iter()
            .map(|r| (r.id.clone(), r.into_review_response()))
            .collect();

        let reviews = page
            .data
            .into_iter()
            .map(|review| {
                let response = review
                    .relationships
                    .response
                    .data
                    .as_ref()
                    .and_then(|rel| responses.get(&rel.id).cloned());
                review.into_customer_review(response)
            })
            .collect();

        // `links.next` is the opaque token for the next page; `None` when absent.
        let next_token = page.links.next.filter(|u| !u.is_empty());
        Ok(CustomerReviewsPage {
            reviews,
            next_token,
        })
    }

    /// Lists the review submissions for `app_id`, resolving the version string and
    /// submitter from the JSON:API `included[]` section.
    ///
    /// `GET /v1/reviewSubmissions?filter[app]={app_id}&include=appStoreVersionForReview,submittedByActor&limit=50`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_review_submissions(
        &self,
        app_id: &str,
    ) -> Result<Vec<ReviewSubmission>, StackError> {
        let mut submissions = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/reviewSubmissions?filter[app]={app_id}\
             &include=appStoreVersionForReview,submittedByActor&limit=50",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: ReviewSubmissionsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("review submissions response: {e}")))?;

            // Index the heterogeneous `included[]` by id, split per resource type.
            let mut versions: HashMap<String, Option<String>> = HashMap::new();
            let mut actors: HashMap<String, ActorAttributes> = HashMap::new();
            for resource in page.included {
                match resource {
                    IncludedResource::AppStoreVersions { id, attributes } => {
                        versions.insert(id, attributes.version_string);
                    }
                    IncludedResource::Actors { id, attributes } => {
                        actors.insert(id, attributes);
                    }
                    IncludedResource::Other => {}
                }
            }

            submissions.extend(
                page.data.into_iter().map(|submission| {
                    submission.into_review_submission(app_id, &versions, &actors)
                }),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(submissions)
    }

    /// Creates or replaces the developer response for `review_id` with `body`.
    ///
    /// Apple treats `POST /v1/customerReviewResponses` as an upsert keyed by the
    /// review relationship: posting again replaces the existing response. Success
    /// is `201 Created` (any 2xx is accepted); the returned single-resource
    /// document is mapped into a [`ReviewResponse`].
    ///
    /// `POST /v1/customerReviewResponses`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn reply_to_review(
        &self,
        review_id: &str,
        body: &str,
    ) -> Result<ReviewResponse, StackError> {
        let url = format!("{}/v1/customerReviewResponses", self.base_url);
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": {
                "type": "customerReviewResponses",
                "attributes": { "responseBody": body },
                "relationships": {
                    "review": {
                        "data": { "type": "customerReviews", "id": review_id }
                    }
                }
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.post(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: ReviewResponseDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("review response: {e}")))?;
        Ok(document.data.into_review_response())
    }

    /// Deletes the developer response identified by `response_id`.
    ///
    /// `DELETE /v1/customerReviewResponses/{response_id}` returns `204 No Content`
    /// (any 2xx is accepted) with an empty body.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response or [`StackError::Network`] on
    /// transport failure.
    pub(crate) async fn delete_review_response(&self, response_id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/customerReviewResponses/{response_id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Lists the App Store versions for `app_id`, mapping each into an
    /// [`AppStoreVersionInfo`] with `app_id` set from the parameter.
    ///
    /// `GET /v1/apps/{app_id}/appStoreVersions?limit={limit}`. A single page is
    /// fetched — `links.next` is not followed (mirrors the host behavior, which
    /// just passes `limit`).
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_versions(
        &self,
        app_id: &str,
        limit: u32,
    ) -> Result<Vec<AppStoreVersionInfo>, StackError> {
        let url = format!(
            "{}/v1/apps/{app_id}/appStoreVersions?limit={limit}",
            self.base_url
        );
        let body = self.get_page(&url).await?;
        let page: AppStoreVersionsResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("app store versions response: {e}")))?;
        Ok(page
            .data
            .into_iter()
            .map(|v| v.into_version_info(app_id))
            .collect())
    }

    /// Creates a new App Store version for `app_id`.
    ///
    /// `POST /v1/appStoreVersions` with a JSON:API body carrying `platform`
    /// (the raw ASC value: `IOS` / `MAC_OS` / `TV_OS` / `VISION_OS`),
    /// `versionString`, a `releaseType` of `MANUAL`, and the `app` relationship.
    /// Success is any 2xx (`201 Created`); the returned single-resource document
    /// is mapped into an [`AppStoreVersionInfo`] with `app_id` from the parameter.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn create_version(
        &self,
        app_id: &str,
        platform: &str,
        version_string: &str,
    ) -> Result<AppStoreVersionInfo, StackError> {
        let url = format!("{}/v1/appStoreVersions", self.base_url);
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": {
                "type": "appStoreVersions",
                "attributes": {
                    "platform": platform,
                    "versionString": version_string,
                    "releaseType": "MANUAL"
                },
                "relationships": {
                    "app": {
                        "data": { "type": "apps", "id": app_id }
                    }
                }
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.post(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: AppStoreVersionDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("app store version response: {e}")))?;
        Ok(document.data.into_version_info(app_id))
    }

    /// Updates the App Store version identified by `id`, sending only the
    /// attributes that are `Some`.
    ///
    /// `PATCH /v1/appStoreVersions/{id}`. `earliest_release_date` maps to the
    /// `earliestReleaseDate` attribute (a raw ISO8601 string passed through
    /// verbatim — the core does no date parsing). Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response or [`StackError::Network`] on
    /// transport failure.
    pub(crate) async fn update_version(
        &self,
        id: &str,
        version_string: Option<&str>,
        copyright: Option<&str>,
        release_type: Option<&str>,
        earliest_release_date: Option<&str>,
    ) -> Result<(), StackError> {
        let url = format!("{}/v1/appStoreVersions/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        if let Some(value) = version_string {
            attributes.insert("versionString".into(), json!(value));
        }
        if let Some(value) = copyright {
            attributes.insert("copyright".into(), json!(value));
        }
        if let Some(value) = release_type {
            attributes.insert("releaseType".into(), json!(value));
        }
        if let Some(value) = earliest_release_date {
            attributes.insert("earliestReleaseDate".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "appStoreVersions",
                "id": id,
                "attributes": attributes
            }
        });

        let (status, body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Deletes the App Store version identified by `id`.
    ///
    /// `DELETE /v1/appStoreVersions/{id}` returns `204 No Content` (any 2xx is
    /// accepted) with an empty body.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response or [`StackError::Network`] on
    /// transport failure.
    pub(crate) async fn delete_version(&self, id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/appStoreVersions/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Submits the version `version_id` of `app_id` for App Store review.
    ///
    /// This drives three sequential App Store Connect requests, each routed
    /// through the shared pending-agreements 403 guard:
    /// 1. `POST /v1/reviewSubmissions` creating a submission for the `app`
    ///    relationship. When `platform` is `Some`, an `attributes.platform`
    ///    object carries it; when `None`, no `attributes` object is sent. The
    ///    created submission's `data.id` is parsed for the follow-ups.
    /// 2. `POST /v1/reviewSubmissionItems` attaching the `appStoreVersion`
    ///    (`version_id`) to the new submission.
    /// 3. `PATCH /v1/reviewSubmissions/{id}` setting the `submitted` attribute
    ///    to `true` to finalize the submission.
    ///
    /// Any 2xx on each request advances to the next; success of the final PATCH
    /// yields `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn submit_for_review(
        &self,
        app_id: &str,
        version_id: &str,
        platform: Option<&str>,
    ) -> Result<(), StackError> {
        // 1. Create the review submission.
        let mut submission_data = serde_json::Map::new();
        submission_data.insert("type".into(), json!("reviewSubmissions"));
        if let Some(platform) = platform {
            submission_data.insert("attributes".into(), json!({ "platform": platform }));
        }
        submission_data.insert(
            "relationships".into(),
            json!({
                "app": { "data": { "type": "apps", "id": app_id } }
            }),
        );
        let create_body = json!({ "data": submission_data });

        let url = format!("{}/v1/reviewSubmissions", self.base_url);
        let response_body = self.post_json_2xx(&url, &create_body).await?;
        let document: ReviewSubmissionCreateDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("review submission response: {e}")))?;
        let submission_id = document.data.id;

        // 2. Attach the app store version as a submission item.
        let item_url = format!("{}/v1/reviewSubmissionItems", self.base_url);
        let item_body = json!({
            "data": {
                "type": "reviewSubmissionItems",
                "relationships": {
                    "reviewSubmission": {
                        "data": { "type": "reviewSubmissions", "id": submission_id }
                    },
                    "appStoreVersion": {
                        "data": { "type": "appStoreVersions", "id": version_id }
                    }
                }
            }
        });
        self.post_json_2xx(&item_url, &item_body).await?;

        // 3. Mark the submission submitted.
        let patch_url = format!("{}/v1/reviewSubmissions/{submission_id}", self.base_url);
        let patch_body = json!({
            "data": {
                "type": "reviewSubmissions",
                "id": submission_id,
                "attributes": { "submitted": true }
            }
        });
        let token = self.auth.bearer_token().await?;
        self.patch_no_content(&patch_url, &token, &patch_body).await
    }

    /// Cancels the active submission for `app_id`.
    ///
    /// 1. `GET /v1/reviewSubmissions?filter[state]=WAITING_FOR_REVIEW,IN_REVIEW\
    ///    &filter[app]={app_id}` and take the first `data` item. When the page is
    ///    empty this is a no-op that returns `Ok(())` (matching the host).
    /// 2. `PATCH /v1/reviewSubmissions/{id}` setting the `canceled` attribute to
    ///    `true`. Any 2xx → `Ok(())`.
    ///
    /// Both requests route through the shared pending-agreements 403 guard.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn cancel_review(&self, app_id: &str) -> Result<(), StackError> {
        let url = format!(
            "{}/v1/reviewSubmissions?filter[state]=WAITING_FOR_REVIEW,IN_REVIEW\
             &filter[app]={app_id}",
            self.base_url
        );
        self.cancel_first_submission(&url).await
    }

    /// Manually releases the approved version identified by `version_id`.
    ///
    /// `POST /v1/appStoreVersionReleaseRequests` with a JSON:API body carrying
    /// the `appStoreVersion` to-one relationship. Any 2xx → `Ok(())`. The call
    /// routes through the shared pending-agreements 403 guard.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn release_version(&self, version_id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/appStoreVersionReleaseRequests", self.base_url);
        let request_body = json!({
            "data": {
                "type": "appStoreVersionReleaseRequests",
                "relationships": {
                    "appStoreVersion": {
                        "data": { "type": "appStoreVersions", "id": version_id }
                    }
                }
            }
        });
        self.post_json_2xx(&url, &request_body).await.map(|_| ())
    }

    /// Discards the active submissions for `app_id`, returning the version out of
    /// review.
    ///
    /// 1. `GET /v1/reviewSubmissions?filter[app]={app_id}\
    ///    &filter[state]=READY_FOR_REVIEW,WAITING_FOR_REVIEW,IN_REVIEW,\
    ///    UNRESOLVED_ISSUES` — only *active*/cancellable submissions are
    ///    fetched, so finished/historical submissions are never targeted. An
    ///    empty page is a no-op that returns `Ok(())` (matching the host).
    /// 2. **Every** submission in the page is processed (production has been
    ///    observed with several stale `READY_FOR_REVIEW` submissions; clearing
    ///    all of them is what reliably unsticks the version). The action
    ///    branches on each submission's `state`:
    ///    - `WAITING_FOR_REVIEW` / `IN_REVIEW` / `UNRESOLVED_ISSUES` → the
    ///      submission has already been sent to review, so it is canceled via
    ///      `PATCH /v1/reviewSubmissions/{id}` with `canceled: true` (the
    ///      documented "Cancel Submission" action).
    ///    - `READY_FOR_REVIEW` → the submission was created but not yet
    ///      submitted. The `reviewSubmissions` resource does **not** allow
    ///      `DELETE` (Apple returns `403 FORBIDDEN_ERROR`) and does **not**
    ///      accept `canceled: true` in this state (Apple returns
    ///      `409 STATE_ERROR.ENTITY_STATE_INVALID`). Instead its items are
    ///      listed via `GET /v1/reviewSubmissions/{id}/items` and each one is
    ///      removed via `DELETE /v1/reviewSubmissionItems/{itemId}`. Emptying
    ///      the submission of its items returns the `appStoreVersion` to
    ///      `PREPARE_FOR_SUBMISSION`.
    ///    - any other state (or a missing `state`) → skipped, with no mutating
    ///      call. We never blindly PATCH an unknown state: a non-cancellable
    ///      submission would otherwise return `409 STATE_ERROR.ENTITY_STATE_INVALID`.
    ///
    /// All requests route through the shared pending-agreements 403 guard.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn reject_version(&self, app_id: &str) -> Result<(), StackError> {
        let url = format!(
            "{}/v1/reviewSubmissions?filter[app]={app_id}\
             &filter[state]=READY_FOR_REVIEW,WAITING_FOR_REVIEW,IN_REVIEW,UNRESOLVED_ISSUES",
            self.base_url
        );
        let body = self.get_page(&url).await?;
        let page: ReviewSubmissionsResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("review submissions response: {e}")))?;

        // Process every active submission, not just the first: production has
        // been seen with several stale READY_FOR_REVIEW submissions, and the
        // version only unsticks once they are all cleared. An empty page leaves
        // the loop untouched and returns Ok(()) (host no-op behavior).
        for submission in page.data {
            match submission.attributes.state.as_deref() {
                // Already sent to review: cancel the in-flight submission.
                Some("WAITING_FOR_REVIEW") | Some("IN_REVIEW") | Some("UNRESOLVED_ISSUES") => {
                    let patch_url =
                        format!("{}/v1/reviewSubmissions/{}", self.base_url, submission.id);
                    let patch_body = json!({
                        "data": {
                            "type": "reviewSubmissions",
                            "id": submission.id,
                            "attributes": { "canceled": true }
                        }
                    });
                    let token = self.auth.bearer_token().await?;
                    self.patch_no_content(&patch_url, &token, &patch_body)
                        .await?;
                }
                // Created but not yet submitted: `reviewSubmissions` forbids
                // DELETE and rejects `canceled: true` here, so drop the
                // submission's items instead — emptying it returns the version
                // to PREPARE_FOR_SUBMISSION.
                Some("READY_FOR_REVIEW") => {
                    self.delete_submission_items(&submission.id).await?;
                }
                // Unknown / non-cancellable state: never blindly PATCH (would 409).
                _ => {}
            }
        }
        Ok(())
    }

    /// Removes every `reviewSubmissionItem` belonging to the submission `id`,
    /// following `links.next` pagination across pages.
    ///
    /// `GET /v1/reviewSubmissions/{id}/items?include=appStoreVersion` lists the
    /// items (normally one), then each is removed via
    /// `DELETE /v1/reviewSubmissionItems/{itemId}`. Emptying a not-yet-submitted
    /// submission of its items returns the version to `PREPARE_FOR_SUBMISSION`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    async fn delete_submission_items(&self, id: &str) -> Result<(), StackError> {
        let mut next_url = Some(format!(
            "{}/v1/reviewSubmissions/{id}/items?include=appStoreVersion",
            self.base_url
        ));
        while let Some(url) = next_url {
            let page = self.fetch_submission_items_page(&url).await?;
            for item in page.data {
                self.delete_review_submission_item(&item.id).await?;
            }
            next_url = page.links.next;
        }
        Ok(())
    }

    /// Fetches and parses one page of a submission's `reviewSubmissionItems`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    async fn fetch_submission_items_page(
        &self,
        url: &str,
    ) -> Result<ReviewSubmissionItemsResponse, StackError> {
        let body = self.get_page(url).await?;
        serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("review submission items response: {e}")))
    }

    /// Deletes the review submission item identified by `item_id`.
    ///
    /// `DELETE /v1/reviewSubmissionItems/{item_id}`. Any 2xx (Apple returns
    /// `204 No Content`) → `Ok(())`. Failures route through the shared
    /// pending-agreements 403 guard.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    async fn delete_review_submission_item(&self, item_id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/reviewSubmissionItems/{item_id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Fetches the phased (staged) release for `version_id`.
    ///
    /// `GET /v1/appStoreVersions/{version_id}/appStoreVersionPhasedRelease`
    /// resolves the singular to-one relationship into a single-resource
    /// document. The document's `data` may be `null`/absent when no phased
    /// release exists → `Ok(None)`. A `404` likewise resolves to `Ok(None)`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response (other than 404),
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub(crate) async fn fetch_phased_release(
        &self,
        version_id: &str,
    ) -> Result<Option<PhasedReleaseInfo>, StackError> {
        let url = format!(
            "{}/v1/appStoreVersions/{version_id}/appStoreVersionPhasedRelease",
            self.base_url
        );
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.get(&url).bearer_auth(token))
            .await?;
        // No phased release on the version → ASC returns 404; treat as absent.
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: body,
            });
        }

        let document: PhasedReleaseDocument = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("phased release response: {e}")))?;
        Ok(document
            .data
            .map(PhasedReleaseResource::into_phased_release_info))
    }

    /// Creates a phased (staged) release for `version_id` with the initial
    /// `state`.
    ///
    /// `POST /v1/appStoreVersionPhasedReleases` with a JSON:API body carrying
    /// the `phasedReleaseState` attribute and the `appStoreVersion` to-one
    /// relationship. `state` is the raw ASC `phasedReleaseState` value
    /// (`INACTIVE` / `ACTIVE` / `PAUSED` / `COMPLETE`). The created resource
    /// (201) is mapped into a [`PhasedReleaseInfo`]. The call routes through the
    /// shared pending-agreements 403 guard.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn create_phased_release(
        &self,
        version_id: &str,
        state: &str,
    ) -> Result<PhasedReleaseInfo, StackError> {
        let url = format!("{}/v1/appStoreVersionPhasedReleases", self.base_url);
        let request_body = json!({
            "data": {
                "type": "appStoreVersionPhasedReleases",
                "attributes": { "phasedReleaseState": state },
                "relationships": {
                    "appStoreVersion": {
                        "data": { "type": "appStoreVersions", "id": version_id }
                    }
                }
            }
        });
        let body = self.post_json_2xx(&url, &request_body).await?;
        let document: PhasedReleaseDocument = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("phased release response: {e}")))?;
        document
            .data
            .map(PhasedReleaseResource::into_phased_release_info)
            .ok_or_else(|| StackError::decode("phased release response: missing data"))
    }

    /// Deletes the phased release identified by `id`.
    ///
    /// `DELETE /v1/appStoreVersionPhasedReleases/{id}`. Any 2xx → `Ok(())`. The
    /// call routes through the shared pending-agreements 403 guard.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn delete_phased_release(&self, id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/appStoreVersionPhasedReleases/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Updates the `state` of the phased release identified by `id`.
    ///
    /// `PATCH /v1/appStoreVersionPhasedReleases/{id}` with a JSON:API body
    /// setting the `phasedReleaseState` attribute. `state` is the raw ASC value
    /// (`INACTIVE` / `ACTIVE` / `PAUSED` / `COMPLETE`). The updated resource
    /// (200) is mapped into a [`PhasedReleaseInfo`]. Failures route through the
    /// shared pending-agreements 403 guard.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn update_phased_release_state(
        &self,
        id: &str,
        state: &str,
    ) -> Result<PhasedReleaseInfo, StackError> {
        let url = format!("{}/v1/appStoreVersionPhasedReleases/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": {
                "type": "appStoreVersionPhasedReleases",
                "id": id,
                "attributes": { "phasedReleaseState": state }
            }
        });

        let (status, body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: body,
            });
        }

        let document: PhasedReleaseDocument = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("phased release response: {e}")))?;
        document
            .data
            .map(PhasedReleaseResource::into_phased_release_info)
            .ok_or_else(|| StackError::decode("phased release response: missing data"))
    }

    /// Shared GET-then-conditional-PATCH used by both [`Self::cancel_review`] and
    /// [`Self::reject_version`]: fetches the submissions page at `get_url`, and
    /// if a first item exists, PATCHes it with `canceled: true`. An empty page is
    /// a no-op (`Ok(())`).
    async fn cancel_first_submission(&self, get_url: &str) -> Result<(), StackError> {
        let body = self.get_page(get_url).await?;
        let page: ReviewSubmissionsResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("review submissions response: {e}")))?;

        let Some(submission) = page.data.into_iter().next() else {
            // No active submission to cancel — matches the host's no-op behavior.
            return Ok(());
        };

        let patch_url = format!("{}/v1/reviewSubmissions/{}", self.base_url, submission.id);
        let patch_body = json!({
            "data": {
                "type": "reviewSubmissions",
                "id": submission.id,
                "attributes": { "canceled": true }
            }
        });
        let token = self.auth.bearer_token().await?;
        self.patch_no_content(&patch_url, &token, &patch_body).await
    }

    /// Authenticated `POST` of a JSON:API body, returning the raw response body on
    /// any 2xx or mapping the failure through the shared pending-agreements 403
    /// guard (then [`StackError::Http`]); transport → [`StackError::Network`].
    async fn post_json_2xx(
        &self,
        url: &str,
        request_body: &serde_json::Value,
    ) -> Result<String, StackError> {
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.post(url).bearer_auth(token).json(request_body))
            .await?;
        if status.is_success() {
            return Ok(body);
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Lists the builds for `app_id`, newest first (by upload date), mapping each
    /// into an enriched [`BuildInfo`] with `app_id` set from the parameter.
    ///
    /// `GET /v1/builds?filter[app]={app_id}&sort=-uploadedDate&limit={limit}\
    /// &include=preReleaseVersion,buildBetaDetail,betaAppReviewSubmission`,
    /// following `links.next` pagination until exhausted. The `include` resolves
    /// the marketing version / platform / build states / review state / icon
    /// enrichment from each page's `included[]`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_builds(
        &self,
        app_id: &str,
        limit: u32,
    ) -> Result<Vec<BuildInfo>, StackError> {
        let mut builds = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/builds?filter[app]={app_id}&sort=-uploadedDate&limit={limit}\
             &include=preReleaseVersion,buildBetaDetail,betaAppReviewSubmission",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: BuildsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("builds response: {e}")))?;
            let index = IncludedIndex::from_included(page.included);
            builds.extend(page.data.iter().map(|b| build_info_from(b, app_id, &index)));

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(builds)
    }

    /// Fetches a SINGLE page of builds for incremental (load-more) paging,
    /// returning the enriched builds plus an opaque `next_token`.
    ///
    /// When `page_token` is `Some(url)` the URL — a prior call's `next_token`,
    /// itself the JSON:API `links.next` (absolute, already encoding sort / filter /
    /// cursor / include) — is fetched verbatim. Otherwise the first page is built
    /// from `app_id`, the newest-first sort, `limit`, the enrichment `include`,
    /// and, when present, `filter[preReleaseVersion.platform]` (`platform`) and a
    /// comma-joined `filter[processingState]` (`processing_states`). Unlike
    /// [`Self::fetch_builds`], `links.next` is NOT followed — its value is returned
    /// as `next_token` (`None` on the last page) for the caller to pass back.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub(crate) async fn fetch_builds_page(
        &self,
        app_id: &str,
        platform: Option<&str>,
        processing_states: &[String],
        limit: u32,
        page_token: Option<&str>,
    ) -> Result<BuildsPage, StackError> {
        let url = match page_token {
            Some(token) => token.to_string(),
            None => {
                let mut url = format!(
                    "{}/v1/builds?filter[app]={app_id}&sort=-uploadedDate&limit={limit}\
                     &include=preReleaseVersion,buildBetaDetail,betaAppReviewSubmission",
                    self.base_url
                );
                if let Some(platform) = platform {
                    url.push_str("&filter[preReleaseVersion.platform]=");
                    url.push_str(platform);
                }
                if !processing_states.is_empty() {
                    url.push_str("&filter[processingState]=");
                    url.push_str(&processing_states.join(","));
                }
                url
            }
        };

        let body = self.get_page(&url).await?;
        let page: BuildsResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("builds response: {e}")))?;
        let index = IncludedIndex::from_included(page.included);
        let builds = page
            .data
            .iter()
            .map(|b| build_info_from(b, app_id, &index))
            .collect();

        // `links.next` is the opaque token for the next page; `None` when absent.
        let next_token = page.links.next.filter(|u| !u.is_empty());
        Ok(BuildsPage { builds, next_token })
    }

    /// Lists the builds belonging to the beta group `group_id`, newest first,
    /// mapping each into an enriched [`BuildInfo`]. The owning app id is not known
    /// from this call site, so `BuildInfo::app_id` is left empty.
    ///
    /// `GET /v1/builds?filter[betaGroups]={group_id}&sort=-uploadedDate&limit={limit}\
    /// &include=preReleaseVersion,buildBetaDetail,betaAppReviewSubmission`,
    /// following `links.next` pagination until exhausted.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_builds_for_group(
        &self,
        group_id: &str,
        limit: u32,
    ) -> Result<Vec<BuildInfo>, StackError> {
        let mut builds = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/builds?filter[betaGroups]={group_id}&sort=-uploadedDate&limit={limit}\
             &include=preReleaseVersion,buildBetaDetail,betaAppReviewSubmission",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: BuildsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("builds response: {e}")))?;
            let index = IncludedIndex::from_included(page.included);
            builds.extend(page.data.iter().map(|b| build_info_from(b, "", &index)));

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(builds)
    }

    /// Fetches the full detail of the build `build_id`: the enriched build plus
    /// its associated beta groups and "What to Test" localizations resolved from
    /// the single-resource document's `included[]`. The owning app id is not known
    /// from this call site, so `BuildInfo::app_id` is left empty.
    ///
    /// `GET /v1/builds/{build_id}?include=preReleaseVersion,buildBetaDetail,\
    /// betaAppReviewSubmission,betaGroups,betaBuildLocalizations\
    /// &limit[betaBuildLocalizations]=50&limit[betaGroups]=50`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_build_detail(
        &self,
        build_id: &str,
    ) -> Result<BuildDetailInfo, StackError> {
        let url = format!(
            "{}/v1/builds/{build_id}?include=preReleaseVersion,buildBetaDetail,\
             betaAppReviewSubmission,betaGroups,betaBuildLocalizations\
             &limit[betaBuildLocalizations]=50&limit[betaGroups]=50",
            self.base_url
        );
        let body = self.get_page(&url).await?;
        let document: BuildDocument = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("build detail response: {e}")))?;

        let resource = document
            .data
            .ok_or_else(|| StackError::decode("build detail response: missing data".to_string()))?;
        let index = IncludedIndex::from_included(document.included);

        let build = build_info_from(&resource, "", &index);

        let beta_groups = resource
            .relationships
            .beta_groups
            .data
            .iter()
            .filter_map(|rel| index.beta_groups.get(&rel.id))
            .map(|group| group.to_beta_group_info(""))
            .collect();

        let localizations = resource
            .relationships
            .beta_build_localizations
            .data
            .iter()
            .filter_map(|rel| index.beta_build_localizations.get(&rel.id))
            .map(BetaBuildLocalizationResource::to_beta_build_localization_info)
            .collect();

        Ok(BuildDetailInfo {
            build,
            beta_groups,
            localizations,
        })
    }

    /// Fetches the build currently attached to the App Store version
    /// `version_id`, via its singular `build` to-one relationship document. The
    /// build carries no enrichment (no `include`), and the owning app id is not
    /// known from this call site, so `BuildInfo::app_id` is left empty.
    ///
    /// `GET /v1/appStoreVersions/{version_id}/build`. The document's `data` may be
    /// `null`/absent when no build is attached → `Ok(None)`. A `404` likewise
    /// resolves to `Ok(None)`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page other than 404, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_current_build(
        &self,
        version_id: &str,
    ) -> Result<Option<BuildInfo>, StackError> {
        let url = format!("{}/v1/appStoreVersions/{version_id}/build", self.base_url);
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.get(&url).bearer_auth(token))
            .await?;
        // No build attached to the version → ASC returns 404; treat as absent.
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: body,
            });
        }

        let document: BuildDocument = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("current build response: {e}")))?;
        let index = IncludedIndex::default();
        Ok(document
            .data
            .map(|resource| build_info_from(&resource, "", &index)))
    }

    /// Marks the build identified by `build_id` as expired.
    ///
    /// `PATCH /v1/builds/{build_id}` with a JSON:API body setting the `expired`
    /// attribute to `true` (the ASC attribute key is `expired`, not `isExpired`).
    /// Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn expire_build(&self, build_id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/builds/{build_id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": {
                "type": "builds",
                "id": build_id,
                "attributes": { "expired": true }
            }
        });

        let (status, body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Attaches the build `build_id` to the App Store version `version_id`.
    ///
    /// `PATCH /v1/appStoreVersions/{version_id}/relationships/build` with a
    /// JSON:API to-one linkage body `{ "data": { "type": "builds", "id": ... } }`
    /// (a single relationship object, not an array). Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn attach_build(
        &self,
        version_id: &str,
        build_id: &str,
    ) -> Result<(), StackError> {
        let url = format!(
            "{}/v1/appStoreVersions/{version_id}/relationships/build",
            self.base_url
        );
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": { "type": "builds", "id": build_id }
        });

        let (status, body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Submits the build `build_id` for beta (TestFlight) review.
    ///
    /// `POST /v1/betaAppReviewSubmissions` with a JSON:API body carrying the
    /// `build` to-one relationship. Any 2xx (`201 Created`) → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn submit_build_for_beta_review(
        &self,
        build_id: &str,
    ) -> Result<(), StackError> {
        let url = format!("{}/v1/betaAppReviewSubmissions", self.base_url);
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": {
                "type": "betaAppReviewSubmissions",
                "relationships": {
                    "build": {
                        "data": { "type": "builds", "id": build_id }
                    }
                }
            }
        });

        let (status, body) = self
            .send_and_read(self.http.post(&url).bearer_auth(token).json(&request_body))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Adds the build `build_id` to each beta group in `group_ids`.
    ///
    /// `POST /v1/builds/{build_id}/relationships/betaGroups` with a JSON:API
    /// to-many linkage body `{ "data": [{ "type": "betaGroups", "id": ... }, ...] }`
    /// — one entry per group id (an empty `group_ids` sends an empty array). Any
    /// 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn add_build_to_groups(
        &self,
        build_id: &str,
        group_ids: &[String],
    ) -> Result<(), StackError> {
        let url = format!(
            "{}/v1/builds/{build_id}/relationships/betaGroups",
            self.base_url
        );
        let token = self.auth.bearer_token().await?;
        let data: Vec<_> = group_ids
            .iter()
            .map(|id| json!({ "type": "betaGroups", "id": id }))
            .collect();
        let request_body = json!({ "data": data });

        let (status, body) = self
            .send_and_read(self.http.post(&url).bearer_auth(token).json(&request_body))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Removes the build `build_id` from the beta group `group_id`.
    ///
    /// `DELETE /v1/betaGroups/{group_id}/relationships/builds` with a JSON:API
    /// to-many linkage body `{ "data": [{ "type": "builds", "id": ... }] }` (an
    /// array carrying the single build). Any 2xx (`204 No Content`) → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn remove_build_from_group(
        &self,
        build_id: &str,
        group_id: &str,
    ) -> Result<(), StackError> {
        let url = format!(
            "{}/v1/betaGroups/{group_id}/relationships/builds",
            self.base_url
        );
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": [{ "type": "builds", "id": build_id }]
        });

        let (status, body) = self
            .send_and_read(
                self.http
                    .delete(&url)
                    .bearer_auth(token)
                    .json(&request_body),
            )
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Lists the beta groups for `app_id`, mapping each into a [`BetaGroupInfo`]
    /// with `app_id` set from the parameter.
    ///
    /// `GET /v1/betaGroups?filter[app]={app_id}&limit={limit}`, following
    /// `links.next` pagination until exhausted (`limit` is the page size).
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_beta_groups(
        &self,
        app_id: &str,
        limit: u32,
    ) -> Result<Vec<BetaGroupInfo>, StackError> {
        let mut groups = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/betaGroups?filter[app]={app_id}&limit={limit}",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: BetaGroupsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("beta groups response: {e}")))?;
            groups.extend(
                page.data
                    .into_iter()
                    .map(|g| g.into_beta_group_info(app_id)),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(groups)
    }

    /// Lists the beta testers belonging to `group_id`, mapping each into a
    /// [`BetaTesterInfo`].
    ///
    /// `GET /v1/betaTesters?filter[betaGroups]={group_id}&limit={limit}`,
    /// following `links.next` pagination until exhausted (`limit` is the page
    /// size).
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_beta_testers(
        &self,
        group_id: &str,
        limit: u32,
    ) -> Result<Vec<BetaTesterInfo>, StackError> {
        let mut testers = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/betaTesters?filter[betaGroups]={group_id}&limit={limit}",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: BetaTestersResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("beta testers response: {e}")))?;
            testers.extend(
                page.data
                    .into_iter()
                    .map(BetaTesterResource::into_beta_tester_info),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(testers)
    }

    /// Creates a new beta group for `app_id`.
    ///
    /// `POST /v1/betaGroups` with a JSON:API body carrying the `name`,
    /// `isInternalGroup`, `hasAccessToAllBuilds`, `isPublicLinkEnabled` and a
    /// `isFeedbackEnabled` of `true`, plus the `app` relationship. Success is any
    /// 2xx (`201 Created`); the returned single-resource document is mapped into a
    /// [`BetaGroupInfo`] with `app_id` set from the parameter (same as the read
    /// path).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn create_beta_group(
        &self,
        app_id: &str,
        name: &str,
        is_internal: bool,
        public_link_enabled: bool,
        has_access_to_all_builds: bool,
    ) -> Result<BetaGroupInfo, StackError> {
        let url = format!("{}/v1/betaGroups", self.base_url);
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": {
                "type": "betaGroups",
                "attributes": {
                    "name": name,
                    "isInternalGroup": is_internal,
                    "hasAccessToAllBuilds": has_access_to_all_builds,
                    "isPublicLinkEnabled": public_link_enabled,
                    "isFeedbackEnabled": true
                },
                "relationships": {
                    "app": {
                        "data": { "type": "apps", "id": app_id }
                    }
                }
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.post(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: BetaGroupDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("beta group response: {e}")))?;
        Ok(document.data.into_beta_group_info(app_id))
    }

    /// Updates the beta group identified by `group_id`, sending only the
    /// attributes that are `Some`.
    ///
    /// `PATCH /v1/betaGroups/{group_id}`. `name` → `name`, `public_link_enabled`
    /// → `isPublicLinkEnabled`, `public_link_limit` → `publicLinkLimit`, and
    /// `feedback_enabled` → `isFeedbackEnabled`; `None` fields are omitted from
    /// the request entirely. Success is any 2xx; the returned single-resource
    /// document is mapped into a [`BetaGroupInfo`]. The PATCH response carries no
    /// `app` relationship, so `app_id` is recovered from it when present and is an
    /// empty string otherwise (see
    /// [`BetaGroupResource::into_beta_group_info_inferring_app`]).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn update_beta_group(
        &self,
        group_id: &str,
        name: Option<&str>,
        public_link_enabled: Option<bool>,
        public_link_limit: Option<i32>,
        feedback_enabled: Option<bool>,
    ) -> Result<BetaGroupInfo, StackError> {
        let url = format!("{}/v1/betaGroups/{group_id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        if let Some(value) = name {
            attributes.insert("name".into(), json!(value));
        }
        if let Some(value) = public_link_enabled {
            attributes.insert("isPublicLinkEnabled".into(), json!(value));
        }
        if let Some(value) = public_link_limit {
            attributes.insert("publicLinkLimit".into(), json!(value));
        }
        if let Some(value) = feedback_enabled {
            attributes.insert("isFeedbackEnabled".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "betaGroups",
                "id": group_id,
                "attributes": attributes
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: BetaGroupDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("beta group response: {e}")))?;
        Ok(document.data.into_beta_group_info_inferring_app())
    }

    /// Deletes the beta group identified by `group_id`.
    ///
    /// `DELETE /v1/betaGroups/{group_id}` returns `204 No Content` (any 2xx is
    /// accepted) with an empty body.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn delete_beta_group(&self, group_id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/betaGroups/{group_id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Adds a beta tester to `group_id`, creating the tester from `email` (and
    /// optional name parts).
    ///
    /// `POST /v1/betaTesters` with a JSON:API body carrying the `email`, the
    /// `firstName`/`lastName` attributes (omitted when `None`), and the
    /// `betaGroups` to-many relationship. Success is any 2xx (`201 Created`); the
    /// returned single-resource document is mapped into a [`BetaTesterInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn add_beta_tester(
        &self,
        group_id: &str,
        email: &str,
        first_name: Option<&str>,
        last_name: Option<&str>,
    ) -> Result<BetaTesterInfo, StackError> {
        let url = format!("{}/v1/betaTesters", self.base_url);
        let token = self.auth.bearer_token().await?;

        // Build attributes dynamically so absent name parts are omitted entirely,
        // mirroring how the read DTO treats optional names.
        let mut attributes = serde_json::Map::new();
        if let Some(value) = first_name {
            attributes.insert("firstName".into(), json!(value));
        }
        if let Some(value) = last_name {
            attributes.insert("lastName".into(), json!(value));
        }
        attributes.insert("email".into(), json!(email));

        let request_body = json!({
            "data": {
                "type": "betaTesters",
                "attributes": attributes,
                "relationships": {
                    "betaGroups": {
                        "data": [{ "type": "betaGroups", "id": group_id }]
                    }
                }
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.post(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: BetaTesterDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("beta tester response: {e}")))?;
        Ok(document.data.into_beta_tester_info())
    }

    /// Removes the beta tester `tester_id` from `group_id` (the tester is not
    /// deleted, only unlinked from the group).
    ///
    /// `DELETE /v1/betaGroups/{group_id}/relationships/betaTesters` with a
    /// JSON:API to-many linkage body `{ "data": [{ "type": "betaTesters", "id":
    /// ... }] }`. Returns `204 No Content` (any 2xx is accepted).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn remove_beta_tester(
        &self,
        group_id: &str,
        tester_id: &str,
    ) -> Result<(), StackError> {
        let url = format!(
            "{}/v1/betaGroups/{group_id}/relationships/betaTesters",
            self.base_url
        );
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": [{ "type": "betaTesters", "id": tester_id }]
        });

        let (status, body) = self
            .send_and_read(
                self.http
                    .delete(&url)
                    .bearer_auth(token)
                    .json(&request_body),
            )
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Returns the number of beta testers belonging to `group_id`.
    ///
    /// `GET /v1/betaGroups/{group_id}/betaTesters?limit=1`. The full list is
    /// intentionally not fetched and `links.next` is not followed; the count is
    /// read from the JSON:API `meta.paging.total` field and defaults to `0` when
    /// that field is absent.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_tester_count(&self, group_id: &str) -> Result<u32, StackError> {
        let url = format!(
            "{}/v1/betaGroups/{group_id}/betaTesters?limit=1",
            self.base_url
        );
        // `get_page` already routes non-2xx (incl. the pending-agreements 403)
        // through `pending_agreements_error`.
        let body = self.get_page(&url).await?;
        let response: BetaTestersCountResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("beta testers count response: {e}")))?;
        Ok(response.total())
    }

    /// Resends the TestFlight invite for beta tester `tester_id` on `app_id`.
    ///
    /// `POST /v1/betaTesterInvitations` with a JSON:API body carrying the
    /// `betaTester` and `app` relationships. Any 2xx is treated as success; the
    /// response body (if any) is ignored.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn resend_invite(
        &self,
        tester_id: &str,
        app_id: &str,
    ) -> Result<(), StackError> {
        let url = format!("{}/v1/betaTesterInvitations", self.base_url);
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": {
                "type": "betaTesterInvitations",
                "relationships": {
                    "betaTester": {
                        "data": { "type": "betaTesters", "id": tester_id }
                    },
                    "app": {
                        "data": { "type": "apps", "id": app_id }
                    }
                }
            }
        });

        let (status, body) = self
            .send_and_read(self.http.post(&url).bearer_auth(token).json(&request_body))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Lists the beta build localizations for `build_id`, mapping each into a
    /// [`BetaBuildLocalizationInfo`].
    ///
    /// `GET /v1/betaBuildLocalizations?filter[build]={build_id}&limit={limit}`,
    /// following `links.next` pagination until exhausted (`limit` is the page
    /// size).
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_beta_build_localizations(
        &self,
        build_id: &str,
        limit: u32,
    ) -> Result<Vec<BetaBuildLocalizationInfo>, StackError> {
        let mut localizations = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/betaBuildLocalizations?filter[build]={build_id}&limit={limit}",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: BetaBuildLocalizationsResponse =
                serde_json::from_str(&body).map_err(|e| {
                    StackError::decode(format!("beta build localizations response: {e}"))
                })?;
            localizations.extend(
                page.data
                    .into_iter()
                    .map(BetaBuildLocalizationResource::into_beta_build_localization_info),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(localizations)
    }

    /// Creates a beta build localization for `build_id` in `locale`.
    ///
    /// `POST /v1/betaBuildLocalizations` with a JSON:API body carrying the
    /// `whatsNew` and `locale` attributes plus the `build` relationship. Success
    /// is any 2xx (`201 Created`); the returned single-resource document is mapped
    /// into a [`BetaBuildLocalizationInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn create_beta_build_localization(
        &self,
        build_id: &str,
        locale: &str,
        whats_new: &str,
    ) -> Result<BetaBuildLocalizationInfo, StackError> {
        let url = format!("{}/v1/betaBuildLocalizations", self.base_url);
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": {
                "type": "betaBuildLocalizations",
                "attributes": {
                    "whatsNew": whats_new,
                    "locale": locale
                },
                "relationships": {
                    "build": {
                        "data": { "type": "builds", "id": build_id }
                    }
                }
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.post(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: BetaBuildLocalizationDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("beta build localization response: {e}")))?;
        Ok(document.data.into_beta_build_localization_info())
    }

    /// Updates the beta build localization identified by `id`, replacing its
    /// `whatsNew` testing notes.
    ///
    /// `PATCH /v1/betaBuildLocalizations/{id}` with a JSON:API body carrying the
    /// `whatsNew` attribute. Success is any 2xx (`200 OK`); the returned
    /// single-resource document is mapped into a [`BetaBuildLocalizationInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn update_beta_build_localization(
        &self,
        id: &str,
        whats_new: &str,
    ) -> Result<BetaBuildLocalizationInfo, StackError> {
        let url = format!("{}/v1/betaBuildLocalizations/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": {
                "type": "betaBuildLocalizations",
                "id": id,
                "attributes": {
                    "whatsNew": whats_new
                }
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: BetaBuildLocalizationDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("beta build localization response: {e}")))?;
        Ok(document.data.into_beta_build_localization_info())
    }

    /// Lists the beta app localizations for `app_id`, mapping each into a
    /// [`BetaAppLocalizationInfo`].
    ///
    /// `GET /v1/apps/{app_id}/betaAppLocalizations?limit={limit}` — the app's
    /// relationship list endpoint (note this is under `/apps/{id}/`, not a
    /// `filter[app]` query) — following `links.next` pagination until exhausted
    /// (`limit` is the page size).
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_beta_app_localizations(
        &self,
        app_id: &str,
        limit: u32,
    ) -> Result<Vec<BetaAppLocalizationInfo>, StackError> {
        let mut localizations = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/apps/{app_id}/betaAppLocalizations?limit={limit}",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: BetaAppLocalizationsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("beta app localizations response: {e}")))?;
            localizations.extend(
                page.data
                    .into_iter()
                    .map(BetaAppLocalizationResource::into_beta_app_localization_info),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(localizations)
    }

    /// Creates a beta app localization for `app_id` in `locale`.
    ///
    /// `POST /v1/betaAppLocalizations` with a JSON:API body that always carries
    /// the `locale` attribute and includes `feedbackEmail`/`description` only when
    /// `Some`, plus the `app` relationship. Success is any 2xx (`201 Created`); the
    /// returned single-resource document is mapped into a
    /// [`BetaAppLocalizationInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn create_beta_app_localization(
        &self,
        app_id: &str,
        locale: &str,
        feedback_email: Option<&str>,
        description: Option<&str>,
    ) -> Result<BetaAppLocalizationInfo, StackError> {
        let url = format!("{}/v1/betaAppLocalizations", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        attributes.insert("locale".into(), json!(locale));
        if let Some(value) = feedback_email {
            attributes.insert("feedbackEmail".into(), json!(value));
        }
        if let Some(value) = description {
            attributes.insert("description".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "betaAppLocalizations",
                "attributes": attributes,
                "relationships": {
                    "app": {
                        "data": { "type": "apps", "id": app_id }
                    }
                }
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.post(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: BetaAppLocalizationDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("beta app localization response: {e}")))?;
        Ok(document.data.into_beta_app_localization_info())
    }

    /// Updates the beta app localization identified by `id`, replacing only the
    /// provided `feedbackEmail` and/or `description` attributes.
    ///
    /// `PATCH /v1/betaAppLocalizations/{id}` with a JSON:API body that includes
    /// only the `Some` attributes and no relationships. Success is any 2xx
    /// (`200 OK`); the returned single-resource document is mapped into a
    /// [`BetaAppLocalizationInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn update_beta_app_localization(
        &self,
        id: &str,
        feedback_email: Option<&str>,
        description: Option<&str>,
    ) -> Result<BetaAppLocalizationInfo, StackError> {
        let url = format!("{}/v1/betaAppLocalizations/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        if let Some(value) = feedback_email {
            attributes.insert("feedbackEmail".into(), json!(value));
        }
        if let Some(value) = description {
            attributes.insert("description".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "betaAppLocalizations",
                "id": id,
                "attributes": attributes
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: BetaAppLocalizationDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("beta app localization response: {e}")))?;
        Ok(document.data.into_beta_app_localization_info())
    }

    /// Lists the accessibility declarations for `app_id`, mapping each into an
    /// [`AccessibilityDeclarationInfo`].
    ///
    /// `GET /v1/apps/{app_id}/accessibilityDeclarations?limit={limit}` — the app's
    /// relationship list endpoint (note this is under `/apps/{id}/`, not a
    /// `filter[app]` query) — following `links.next` pagination until exhausted
    /// (`limit` is the page size).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_accessibility_declarations(
        &self,
        app_id: &str,
        limit: i64,
    ) -> Result<Vec<AccessibilityDeclarationInfo>, StackError> {
        let mut declarations = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/apps/{app_id}/accessibilityDeclarations?limit={limit}",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: AccessibilityDeclarationsResponse =
                serde_json::from_str(&body).map_err(|e| {
                    StackError::decode(format!("accessibility declarations response: {e}"))
                })?;
            declarations.extend(
                page.data
                    .into_iter()
                    .map(AccessibilityDeclarationResource::into_accessibility_declaration_info),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(declarations)
    }

    /// Creates an accessibility declaration for `app_id` targeting
    /// `device_family`.
    ///
    /// `POST /v1/accessibilityDeclarations` with a JSON:API body carrying the
    /// `deviceFamily` attribute plus the `app` relationship. Success is any 2xx
    /// (`201 Created`); the returned single-resource document is mapped into an
    /// [`AccessibilityDeclarationInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn create_accessibility_declaration(
        &self,
        app_id: &str,
        device_family: &str,
    ) -> Result<AccessibilityDeclarationInfo, StackError> {
        let url = format!("{}/v1/accessibilityDeclarations", self.base_url);
        let token = self.auth.bearer_token().await?;

        let request_body = json!({
            "data": {
                "type": "accessibilityDeclarations",
                "attributes": { "deviceFamily": device_family },
                "relationships": {
                    "app": {
                        "data": { "type": "apps", "id": app_id }
                    }
                }
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.post(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: AccessibilityDeclarationDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("accessibility declaration response: {e}")))?;
        Ok(document.data.into_accessibility_declaration_info())
    }

    /// Updates the accessibility declaration identified by `id`, sending all nine
    /// `supports*` feature flags and, only when `publish` is `true`, a
    /// `publish: true` attribute (the key is omitted entirely otherwise).
    ///
    /// `PATCH /v1/accessibilityDeclarations/{id}` with a JSON:API body. Note the
    /// `supports_differentiate_without_color` flag is sent under the wire key
    /// `supportsDifferentiateWithoutColorAlone`. Success is any 2xx (`200 OK`);
    /// the returned single-resource document is mapped into an
    /// [`AccessibilityDeclarationInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn update_accessibility_declaration(
        &self,
        id: &str,
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
        let url = format!("{}/v1/accessibilityDeclarations/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        // `publish` is only sent when requesting publication; omitting it leaves
        // the declaration in draft.
        if publish {
            attributes.insert("publish".into(), json!(true));
        }
        attributes.insert(
            "supportsAudioDescriptions".into(),
            json!(supports_audio_descriptions),
        );
        attributes.insert("supportsCaptions".into(), json!(supports_captions));
        attributes.insert(
            "supportsDarkInterface".into(),
            json!(supports_dark_interface),
        );
        attributes.insert(
            "supportsDifferentiateWithoutColorAlone".into(),
            json!(supports_differentiate_without_color),
        );
        attributes.insert("supportsLargerText".into(), json!(supports_larger_text));
        attributes.insert(
            "supportsReducedMotion".into(),
            json!(supports_reduced_motion),
        );
        attributes.insert(
            "supportsSufficientContrast".into(),
            json!(supports_sufficient_contrast),
        );
        attributes.insert("supportsVoiceControl".into(), json!(supports_voice_control));
        attributes.insert("supportsVoiceover".into(), json!(supports_voiceover));

        let request_body = json!({
            "data": {
                "type": "accessibilityDeclarations",
                "id": id,
                "attributes": attributes
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: AccessibilityDeclarationDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("accessibility declaration response: {e}")))?;
        Ok(document.data.into_accessibility_declaration_info())
    }

    /// Deletes the accessibility declaration identified by `id`.
    ///
    /// `DELETE /v1/accessibilityDeclarations/{id}` — any 2xx response is success.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn delete_accessibility_declaration(
        &self,
        id: &str,
    ) -> Result<(), StackError> {
        let url = format!("{}/v1/accessibilityDeclarations/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let (status, response_body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        Ok(())
    }

    /// Lists the app-info localizations for `app_info_id`, mapping each into an
    /// [`AppInfoLocalizationInfo`].
    ///
    /// `GET /v1/appInfos/{app_info_id}/appInfoLocalizations` — the appInfo's
    /// relationship list endpoint (note this is under `/appInfos/{id}/`, not a
    /// `filter[appInfo]` query) — following `links.next` pagination until
    /// exhausted.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_app_info_localizations(
        &self,
        app_info_id: &str,
    ) -> Result<Vec<AppInfoLocalizationInfo>, StackError> {
        let mut localizations = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/appInfos/{app_info_id}/appInfoLocalizations",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: AppInfoLocalizationsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("app info localizations response: {e}")))?;
            localizations.extend(
                page.data
                    .into_iter()
                    .map(AppInfoLocalizationResource::into_app_info_localization_info),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(localizations)
    }

    /// Updates the app-info localization identified by `id`, always sending the
    /// `name` attribute and sending `subtitle` only when `Some`.
    ///
    /// `PATCH /v1/appInfoLocalizations/{id}` with a JSON:API body that always
    /// carries `name` and includes `subtitle` only when provided, and no
    /// relationships. Success is any 2xx (`200 OK`); the returned single-resource
    /// document is mapped into an [`AppInfoLocalizationInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn update_app_info_localization(
        &self,
        id: &str,
        name: &str,
        subtitle: Option<&str>,
    ) -> Result<AppInfoLocalizationInfo, StackError> {
        let url = format!("{}/v1/appInfoLocalizations/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        attributes.insert("name".into(), json!(name));
        if let Some(value) = subtitle {
            attributes.insert("subtitle".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "appInfoLocalizations",
                "id": id,
                "attributes": attributes
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: AppInfoLocalizationDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("app info localization response: {e}")))?;
        Ok(document.data.into_app_info_localization_info())
    }

    /// Updates the privacy attributes of the app-info localization identified by
    /// `id`, replacing only the provided privacy URL/text attributes.
    ///
    /// `PATCH /v1/appInfoLocalizations/{id}` with a JSON:API body that includes
    /// only the `Some` privacy attributes
    /// (`privacyPolicyUrl`/`privacyChoicesUrl`/`privacyPolicyText`) and no
    /// relationships. Success is any 2xx (`200 OK`); the returned single-resource
    /// document is mapped into an [`AppInfoLocalizationInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn update_app_info_localization_privacy(
        &self,
        id: &str,
        privacy_policy_url: Option<&str>,
        privacy_choices_url: Option<&str>,
        privacy_policy_text: Option<&str>,
    ) -> Result<AppInfoLocalizationInfo, StackError> {
        let url = format!("{}/v1/appInfoLocalizations/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        if let Some(value) = privacy_policy_url {
            attributes.insert("privacyPolicyUrl".into(), json!(value));
        }
        if let Some(value) = privacy_choices_url {
            attributes.insert("privacyChoicesUrl".into(), json!(value));
        }
        if let Some(value) = privacy_policy_text {
            attributes.insert("privacyPolicyText".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "appInfoLocalizations",
                "id": id,
                "attributes": attributes
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: AppInfoLocalizationDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("app info localization response: {e}")))?;
        Ok(document.data.into_app_info_localization_info())
    }

    /// Creates an app-info localization for `app_info_id` in `locale`.
    ///
    /// `POST /v1/appInfoLocalizations` with a JSON:API body that always carries
    /// the `locale` and `name` attributes and includes `subtitle` only when
    /// `Some`, plus the `appInfo` relationship. Success is any 2xx
    /// (`201 Created`); the returned single-resource document is mapped into an
    /// [`AppInfoLocalizationInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn create_app_info_localization(
        &self,
        app_info_id: &str,
        locale: &str,
        name: &str,
        subtitle: Option<&str>,
    ) -> Result<AppInfoLocalizationInfo, StackError> {
        let url = format!("{}/v1/appInfoLocalizations", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        attributes.insert("locale".into(), json!(locale));
        attributes.insert("name".into(), json!(name));
        if let Some(value) = subtitle {
            attributes.insert("subtitle".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "appInfoLocalizations",
                "attributes": attributes,
                "relationships": {
                    "appInfo": {
                        "data": { "type": "appInfos", "id": app_info_id }
                    }
                }
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.post(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: AppInfoLocalizationDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("app info localization response: {e}")))?;
        Ok(document.data.into_app_info_localization_info())
    }

    /// Deletes the app-info localization identified by `id`.
    ///
    /// `DELETE /v1/appInfoLocalizations/{id}` — any 2xx response is success.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn delete_app_info_localization(&self, id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/appInfoLocalizations/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let (status, response_body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        Ok(())
    }

    /// Lists the App Store version localizations for `version_id`, mapping each
    /// into an [`AppStoreLocalizationInfo`].
    ///
    /// `GET /v1/appStoreVersions/{version_id}/appStoreVersionLocalizations` — the
    /// version's relationship list endpoint — following `links.next` pagination
    /// until exhausted.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_localizations(
        &self,
        version_id: &str,
    ) -> Result<Vec<AppStoreLocalizationInfo>, StackError> {
        let mut localizations = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/appStoreVersions/{version_id}/appStoreVersionLocalizations",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: AppStoreLocalizationsResponse = serde_json::from_str(&body).map_err(|e| {
                StackError::decode(format!("app store version localizations response: {e}"))
            })?;
            localizations.extend(
                page.data
                    .into_iter()
                    .map(AppStoreLocalizationResource::into_app_store_localization_info),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(localizations)
    }

    /// Updates the App Store version localization identified by `id`, sending
    /// only the provided attributes.
    ///
    /// `PATCH /v1/appStoreVersionLocalizations/{id}` with a JSON:API body that
    /// includes only the `Some` attributes
    /// (`description`/`keywords`/`promotionalText`/`supportUrl`/`marketingUrl`/`whatsNew`)
    /// and no relationships. Any 2xx is treated as success.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn update_localization(
        &self,
        id: &str,
        description: Option<&str>,
        keywords: Option<&str>,
        promotional_text: Option<&str>,
        support_url: Option<&str>,
        marketing_url: Option<&str>,
        whats_new: Option<&str>,
    ) -> Result<(), StackError> {
        let url = format!("{}/v1/appStoreVersionLocalizations/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        if let Some(value) = description {
            attributes.insert("description".into(), json!(value));
        }
        if let Some(value) = keywords {
            attributes.insert("keywords".into(), json!(value));
        }
        if let Some(value) = promotional_text {
            attributes.insert("promotionalText".into(), json!(value));
        }
        if let Some(value) = support_url {
            attributes.insert("supportUrl".into(), json!(value));
        }
        if let Some(value) = marketing_url {
            attributes.insert("marketingUrl".into(), json!(value));
        }
        if let Some(value) = whats_new {
            attributes.insert("whatsNew".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "appStoreVersionLocalizations",
                "id": id,
                "attributes": attributes
            }
        });

        self.patch_no_content(&url, &token, &request_body).await
    }

    /// Lists the screenshot sets (with their screenshots) for the version
    /// localization identified by `localization_id`, mapping each into a
    /// [`ScreenshotSetInfo`].
    ///
    /// `GET /v1/appStoreVersionLocalizations/{localization_id}/appScreenshotSets?include=appScreenshots`
    /// — the localization's relationship list endpoint — following `links.next`
    /// pagination until exhausted. Each set's screenshots are resolved from the
    /// document's `included[]` (`appScreenshots`), preserving the relationship
    /// order, and each screenshot's `image_url` is computed from its `imageAsset`
    /// template exactly as the build icon URL is.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_screenshot_sets(
        &self,
        localization_id: &str,
    ) -> Result<Vec<ScreenshotSetInfo>, StackError> {
        let mut sets = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/appStoreVersionLocalizations/{localization_id}/appScreenshotSets?include=appScreenshots",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: AppScreenshotSetsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("app screenshot sets response: {e}")))?;

            // Index the included `appScreenshots` by id so each set can resolve
            // its relationship ids in order.
            let screenshots: HashMap<String, AppScreenshotResource> = page
                .included
                .into_iter()
                .filter_map(|included| match included {
                    AppScreenshotIncluded::AppScreenshots(resource) => {
                        Some((resource.id.clone(), resource))
                    }
                    AppScreenshotIncluded::Other => None,
                })
                .collect();

            for set in page.data {
                let resolved = set
                    .relationships
                    .app_screenshots
                    .data
                    .iter()
                    .filter_map(|rel| screenshots.get(&rel.id))
                    .map(AppScreenshotResource::to_screenshot_info)
                    .collect();
                sets.push(ScreenshotSetInfo {
                    id: set.id,
                    display_type: set.attributes.screenshot_display_type,
                    screenshots: resolved,
                });
            }

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(sets)
    }

    /// Fetches the full App Info detail for `app_id` via two requests, merged
    /// into one [`AppInfoDetails`].
    ///
    /// 1. `GET /v1/apps/{app_id}/appInfos?include=ageRatingDeclaration,appInfoLocalizations,primaryCategory,primarySubcategoryOne,secondaryCategory,secondarySubcategoryOne&limit=1&limit[appInfoLocalizations]=50`
    ///    — the FIRST `data` app-info resource supplies the `app_info_id`, the
    ///    `appStoreAgeRating` attribute, the four category ids and the
    ///    age-rating-declaration id from its RELATIONSHIPS, the localizations
    ///    (from `included` `appInfoLocalizations`), and the age rating (from
    ///    `included` `ageRatingDeclarations`).
    /// 2. `GET /v1/apps/{app_id}?fields[apps]=sku,primaryLocale,contentRightsDeclaration`
    ///    — supplies `sku`, `primary_locale`, and `content_rights_declaration`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response (including a 404-style
    /// error when the app has no app-info resource), [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_app_info(&self, app_id: &str) -> Result<AppInfoDetails, StackError> {
        // Request 1: the app-info resource with category/age-rating/localization
        // includes.
        let app_infos_url = format!(
            "{}/v1/apps/{app_id}/appInfos?include=ageRatingDeclaration,appInfoLocalizations,\
             primaryCategory,primarySubcategoryOne,secondaryCategory,secondarySubcategoryOne\
             &limit=1&limit[appInfoLocalizations]=50",
            self.base_url
        );
        let body = self.get_page(&app_infos_url).await?;
        let page: AppInfosResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("app infos response: {e}")))?;

        let app_info = page
            .data
            .into_iter()
            .next()
            .ok_or_else(|| StackError::Http {
                status: 404,
                message: format!("no app info found for app {app_id}"),
            })?;

        // Resolve the `included[]` localizations and age-rating declarations.
        let mut localizations = Vec::new();
        let mut age_ratings: HashMap<String, AgeRatingDeclarationInfo> = HashMap::new();
        for resource in page.included {
            match resource {
                AppInfoIncluded::AppInfoLocalizations(loc) => {
                    localizations.push(loc.to_app_info_localization_info());
                }
                AppInfoIncluded::AgeRatingDeclarations(decl) => {
                    age_ratings.insert(decl.id.clone(), decl.into_age_rating_declaration_info());
                }
                AppInfoIncluded::Other => {}
            }
        }

        let relationships = &app_info.relationships;
        let age_rating_declaration_id = relationships
            .age_rating_declaration
            .data
            .as_ref()
            .map(|rel| rel.id.clone());
        let age_rating = age_rating_declaration_id
            .as_ref()
            .and_then(|id| age_ratings.get(id).cloned());

        // Request 2: the owning app's sku / primaryLocale / contentRightsDeclaration.
        let app_url = format!(
            "{}/v1/apps/{app_id}?fields[apps]=sku,primaryLocale,contentRightsDeclaration",
            self.base_url
        );
        let app_body = self.get_page(&app_url).await?;
        let app_document: AppDetailDocument = serde_json::from_str(&app_body)
            .map_err(|e| StackError::decode(format!("app detail response: {e}")))?;
        let app_attributes = app_document.data.attributes;

        Ok(AppInfoDetails {
            app_info_id: app_info.id,
            app_id: app_id.to_string(),
            sku: app_attributes.sku,
            primary_locale: app_attributes.primary_locale,
            content_rights_declaration: app_attributes.content_rights_declaration,
            primary_category_id: relationships
                .primary_category
                .data
                .as_ref()
                .map(|rel| rel.id.clone()),
            primary_subcategory_one_id: relationships
                .primary_subcategory_one
                .data
                .as_ref()
                .map(|rel| rel.id.clone()),
            secondary_category_id: relationships
                .secondary_category
                .data
                .as_ref()
                .map(|rel| rel.id.clone()),
            secondary_subcategory_one_id: relationships
                .secondary_subcategory_one
                .data
                .as_ref()
                .map(|rel| rel.id.clone()),
            age_rating_declaration_id,
            app_store_age_rating: app_info.attributes.app_store_age_rating,
            localizations,
            age_rating,
        })
    }

    /// Lists the top-level App Store categories (iOS), each with its subcategory
    /// ids.
    ///
    /// `GET /v1/appCategories?filter[platforms]=IOS&exists[parent]=false&include=subcategories`,
    /// following `links.next` pagination. Each top-level category's
    /// subcategory ids are read from its `relationships.subcategories`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_app_categories(&self) -> Result<Vec<AppCategoryInfo>, StackError> {
        let mut categories = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/appCategories?filter[platforms]=IOS&exists[parent]=false&include=subcategories",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: AppCategoriesResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("app categories response: {e}")))?;
            categories.extend(
                page.data
                    .into_iter()
                    .map(AppCategoryResource::into_app_category_info),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(categories)
    }

    /// Updates the category relationships of the app-info `app_info_id`.
    ///
    /// `PATCH /v1/appInfos/{app_info_id}` with a JSON:API body that wires a
    /// relationship ONLY for each id that is `Some`
    /// (`primaryCategory`/`primarySubcategoryOne`/`secondaryCategory`/
    /// `secondarySubcategoryOne`, each `{ "data": { "type": "appCategories",
    /// "id": ... } }`). Relationships whose id is `None` are omitted (not sent as
    /// `null`). Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn update_app_info_category(
        &self,
        app_info_id: &str,
        primary_category_id: Option<&str>,
        subcategory_one_id: Option<&str>,
        secondary_category_id: Option<&str>,
        secondary_subcategory_one_id: Option<&str>,
    ) -> Result<(), StackError> {
        let url = format!("{}/v1/appInfos/{app_info_id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut relationships = serde_json::Map::new();
        let mut insert_category = |key: &str, id: Option<&str>| {
            if let Some(id) = id {
                relationships.insert(
                    key.into(),
                    json!({ "data": { "type": "appCategories", "id": id } }),
                );
            }
        };
        insert_category("primaryCategory", primary_category_id);
        insert_category("primarySubcategoryOne", subcategory_one_id);
        insert_category("secondaryCategory", secondary_category_id);
        insert_category("secondarySubcategoryOne", secondary_subcategory_one_id);

        let request_body = json!({
            "data": {
                "type": "appInfos",
                "id": app_info_id,
                "relationships": relationships
            }
        });

        self.patch_no_content(&url, &token, &request_body).await
    }

    /// Updates the app `id`, sending `contentRightsDeclaration` and/or
    /// `primaryLocale` only when `Some`.
    ///
    /// `PATCH /v1/apps/{id}`. Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn update_app(
        &self,
        id: &str,
        content_rights_declaration: Option<&str>,
        primary_locale: Option<&str>,
    ) -> Result<(), StackError> {
        let url = format!("{}/v1/apps/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        if let Some(value) = content_rights_declaration {
            attributes.insert("contentRightsDeclaration".into(), json!(value));
        }
        if let Some(value) = primary_locale {
            attributes.insert("primaryLocale".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "apps",
                "id": id,
                "attributes": attributes
            }
        });

        self.patch_no_content(&url, &token, &request_body).await
    }

    /// Updates the age-rating declaration `id`, sending all 18 attributes (all
    /// required from the host).
    ///
    /// `PATCH /v1/ageRatingDeclarations/{id}`. Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn update_age_rating(
        &self,
        id: &str,
        alcohol_tobacco: &str,
        contests: &str,
        gambling_simulated: &str,
        guns_or_other_weapons: &str,
        medical_information: &str,
        profanity: &str,
        sexual_content_graphic: &str,
        sexual_content_or_nudity: &str,
        horror_or_fear: &str,
        mature_or_suggestive: &str,
        violence_cartoon: &str,
        violence_realistic: &str,
        violence_graphic: &str,
        is_advertising: bool,
        is_gambling: bool,
        is_unrestricted_web_access: bool,
        is_user_generated_content: bool,
        age_rating_override: &str,
    ) -> Result<(), StackError> {
        let url = format!("{}/v1/ageRatingDeclarations/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let request_body = json!({
            "data": {
                "type": "ageRatingDeclarations",
                "id": id,
                "attributes": {
                    "alcoholTobaccoOrDrugUseOrReferences": alcohol_tobacco,
                    "contests": contests,
                    "gamblingSimulated": gambling_simulated,
                    "gunsOrOtherWeapons": guns_or_other_weapons,
                    "medicalOrTreatmentInformation": medical_information,
                    "profanityOrCrudeHumor": profanity,
                    "sexualContentGraphicAndNudity": sexual_content_graphic,
                    "sexualContentOrNudity": sexual_content_or_nudity,
                    "horrorOrFearThemes": horror_or_fear,
                    "matureOrSuggestiveThemes": mature_or_suggestive,
                    "violenceCartoonOrFantasy": violence_cartoon,
                    "violenceRealistic": violence_realistic,
                    "violenceRealisticProlongedGraphicOrSadistic": violence_graphic,
                    "isAdvertising": is_advertising,
                    "isGambling": is_gambling,
                    "isUnrestrictedWebAccess": is_unrestricted_web_access,
                    "isUserGeneratedContent": is_user_generated_content,
                    "ageRatingOverrideV2": age_rating_override
                }
            }
        });

        self.patch_no_content(&url, &token, &request_body).await
    }

    /// Resolves the icon URL for `app_id` from its most recent build.
    ///
    /// `GET /v1/builds?filter[app]={app_id}&sort=-uploadedDate&limit=1` — the
    /// first build's `iconAssetToken` is substituted via [`IconAssetToken::to_icon_url`]
    /// (the same computation the build enrichment uses). Returns `Ok(None)` when
    /// there is no build or no token.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_icon_url(&self, app_id: &str) -> Result<Option<String>, StackError> {
        let url = format!(
            "{}/v1/builds?filter[app]={app_id}&sort=-uploadedDate&limit=1",
            self.base_url
        );
        let body = self.get_page(&url).await?;
        let page: BuildsResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("builds response: {e}")))?;
        Ok(page
            .data
            .into_iter()
            .next()
            .and_then(|build| build.attributes.icon_asset_token)
            .and_then(|token| token.to_icon_url()))
    }

    /// Fetches the single beta app review detail for `app_id`, mapping it into a
    /// [`BetaAppReviewDetailInfo`].
    ///
    /// `GET /v1/apps/{app_id}/betaAppReviewDetail` — the app's singular
    /// relationship endpoint, which returns a single-resource document
    /// (`{ "data": { ... } }`), not a list. There is no pagination.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_beta_app_review_detail(
        &self,
        app_id: &str,
    ) -> Result<BetaAppReviewDetailInfo, StackError> {
        let url = format!("{}/v1/apps/{app_id}/betaAppReviewDetail", self.base_url);
        let token = self.auth.bearer_token().await?;

        let (status, response_body) = self
            .send_and_read(self.http.get(&url).bearer_auth(token))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: BetaAppReviewDetailDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("beta app review detail response: {e}")))?;
        Ok(document.data.into_beta_app_review_detail_info())
    }

    /// Updates the beta app review detail identified by `detail_id`, replacing
    /// only the provided attributes.
    ///
    /// `PATCH /v1/betaAppReviewDetails/{detail_id}` (note the plural path
    /// segment) with a JSON:API body that includes only the `Some` attributes
    /// and no relationships. Success is any 2xx (`200 OK`); the returned
    /// single-resource document is mapped into a [`BetaAppReviewDetailInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn update_beta_app_review_detail(
        &self,
        detail_id: &str,
        contact_first_name: Option<&str>,
        contact_last_name: Option<&str>,
        contact_email: Option<&str>,
        contact_phone: Option<&str>,
        demo_account_name: Option<&str>,
        demo_account_password: Option<&str>,
        is_demo_account_required: Option<bool>,
        notes: Option<&str>,
    ) -> Result<BetaAppReviewDetailInfo, StackError> {
        let url = format!("{}/v1/betaAppReviewDetails/{detail_id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        if let Some(value) = contact_first_name {
            attributes.insert("contactFirstName".into(), json!(value));
        }
        if let Some(value) = contact_last_name {
            attributes.insert("contactLastName".into(), json!(value));
        }
        if let Some(value) = contact_email {
            attributes.insert("contactEmail".into(), json!(value));
        }
        if let Some(value) = contact_phone {
            attributes.insert("contactPhone".into(), json!(value));
        }
        if let Some(value) = demo_account_name {
            attributes.insert("demoAccountName".into(), json!(value));
        }
        if let Some(value) = demo_account_password {
            attributes.insert("demoAccountPassword".into(), json!(value));
        }
        if let Some(value) = is_demo_account_required {
            attributes.insert("isDemoAccountRequired".into(), json!(value));
        }
        if let Some(value) = notes {
            attributes.insert("notes".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "betaAppReviewDetails",
                "id": detail_id,
                "attributes": attributes
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: BetaAppReviewDetailDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("beta app review detail response: {e}")))?;
        Ok(document.data.into_beta_app_review_detail_info())
    }

    /// Fetches the single app review detail for `version_id`, or `None` when
    /// there is no app review detail.
    ///
    /// `GET /v1/appStoreVersions/{version_id}/appStoreReviewDetail` — the
    /// version's singular relationship endpoint, which returns a single-resource
    /// document (`{ "data": { ... } }`), not a list. There is no pagination.
    /// Returns `Ok(None)` when the document's `data` is null/absent or the
    /// relationship endpoint answers 404 (no detail attached).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_app_review_detail(
        &self,
        version_id: &str,
    ) -> Result<Option<AppReviewDetailInfo>, StackError> {
        let url = format!(
            "{}/v1/appStoreVersions/{version_id}/appStoreReviewDetail",
            self.base_url
        );
        let token = self.auth.bearer_token().await?;

        let (status, response_body) = self
            .send_and_read(self.http.get(&url).bearer_auth(token))
            .await?;
        // No app review detail on the version → ASC returns 404; treat as absent.
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: AppReviewDetailDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("app review detail response: {e}")))?;
        Ok(document
            .data
            .map(AppReviewDetailResource::into_app_review_detail_info))
    }

    /// Updates the app review detail identified by `detail_id`, replacing only
    /// the provided attributes.
    ///
    /// `PATCH /v1/appStoreReviewDetails/{detail_id}` (note the plural path
    /// segment, in contrast to the singular `appStoreReviewDetail` relationship
    /// used for the fetch) with a JSON:API body that includes only the `Some`
    /// attributes and no relationships. Success is any 2xx (`200 OK`); the
    /// returned single-resource document is mapped into an [`AppReviewDetailInfo`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn update_app_review_detail(
        &self,
        detail_id: &str,
        contact_first_name: Option<&str>,
        contact_last_name: Option<&str>,
        contact_email: Option<&str>,
        contact_phone: Option<&str>,
        notes: Option<&str>,
        demo_account_name: Option<&str>,
        demo_account_password: Option<&str>,
        is_demo_account_required: Option<bool>,
    ) -> Result<AppReviewDetailInfo, StackError> {
        let url = format!("{}/v1/appStoreReviewDetails/{detail_id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        if let Some(value) = contact_first_name {
            attributes.insert("contactFirstName".into(), json!(value));
        }
        if let Some(value) = contact_last_name {
            attributes.insert("contactLastName".into(), json!(value));
        }
        if let Some(value) = contact_email {
            attributes.insert("contactEmail".into(), json!(value));
        }
        if let Some(value) = contact_phone {
            attributes.insert("contactPhone".into(), json!(value));
        }
        if let Some(value) = notes {
            attributes.insert("notes".into(), json!(value));
        }
        if let Some(value) = demo_account_name {
            attributes.insert("demoAccountName".into(), json!(value));
        }
        if let Some(value) = demo_account_password {
            attributes.insert("demoAccountPassword".into(), json!(value));
        }
        if let Some(value) = is_demo_account_required {
            attributes.insert("isDemoAccountRequired".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "appStoreReviewDetails",
                "id": detail_id,
                "attributes": attributes
            }
        });

        let (status, response_body) = self
            .send_and_read(self.http.patch(&url).bearer_auth(token).json(&request_body))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &response_body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: response_body,
            });
        }

        let document: AppReviewDetailDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("app review detail response: {e}")))?;
        document
            .data
            .map(AppReviewDetailResource::into_app_review_detail_info)
            .ok_or_else(|| {
                StackError::decode("app review detail response: missing data".to_string())
            })
    }

    /// Authenticated `PATCH` of `request_body` to `url`, succeeding on any 2xx
    /// (the response body is ignored). Mirrors the failure mapping of the other
    /// write paths: pending-agreements 403 → [`StackError::PendingAgreements`],
    /// any other non-2xx → [`StackError::Http`], transport → [`StackError::Network`].
    /// Lists the team members of the connected account, mapping each active
    /// `users` resource into a [`TeamMemberInfo`].
    ///
    /// `GET /v1/users?fields[users]=firstName,lastName,username,roles&limit=200`,
    /// following `links.next` pagination until exhausted.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_team_members(&self) -> Result<Vec<TeamMemberInfo>, StackError> {
        let mut members = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/users?fields[users]=firstName,lastName,username,roles&limit=200",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: UsersResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("users response: {e}")))?;
            members.extend(
                page.data
                    .into_iter()
                    .map(UserResource::into_team_member_info),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(members)
    }

    /// Lists every user of the connected account: the active members (`users`)
    /// followed by the outstanding invitations (`userInvitations`), unified into
    /// one [`UserInfo`] list discriminated by `is_pending`.
    ///
    /// Two requests are issued (mirroring the host): `GET /v1/users` with the
    /// extended `fields[users]` projection and `GET /v1/userInvitations` with the
    /// `fields[userInvitations]` projection, both at `limit=200` and following
    /// `links.next` pagination. Active members' `email` is read from the
    /// `username` attribute; the two lists are concatenated (active first).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_users(&self) -> Result<Vec<UserInfo>, StackError> {
        // Request 1: active members.
        let mut users = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/users?fields[users]=firstName,lastName,username,roles,allAppsVisible,\
             provisioningAllowed&limit=200",
            self.base_url
        ));
        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: UsersResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("users response: {e}")))?;
            users.extend(
                page.data
                    .into_iter()
                    .map(UserResource::into_active_user_info),
            );
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        // Request 2: pending invitations.
        let mut next_url = Some(format!(
            "{}/v1/userInvitations?fields[userInvitations]=firstName,lastName,email,roles,\
             allAppsVisible,provisioningAllowed,expirationDate&limit=200",
            self.base_url
        ));
        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: UserInvitationsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("user invitations response: {e}")))?;
            users.extend(
                page.data
                    .into_iter()
                    .map(UserInvitationResource::into_pending_user_info),
            );
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(users)
    }

    /// Invites a new user to the connected account.
    ///
    /// `POST /v1/userInvitations` with a JSON:API body whose `attributes` carry
    /// `email`/`firstName`/`lastName`, the raw ASC `roles` strings verbatim, and
    /// the `allAppsVisible`/`provisioningAllowed` flags. The response is
    /// discarded (any 2xx is success).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn invite_user(
        &self,
        email: &str,
        first_name: &str,
        last_name: &str,
        roles: &[String],
        all_apps_visible: bool,
        provisioning_allowed: bool,
    ) -> Result<(), StackError> {
        let url = format!("{}/v1/userInvitations", self.base_url);
        let request_body = json!({
            "data": {
                "type": "userInvitations",
                "attributes": {
                    "email": email,
                    "firstName": first_name,
                    "lastName": last_name,
                    "roles": roles,
                    "allAppsVisible": all_apps_visible,
                    "provisioningAllowed": provisioning_allowed,
                }
            }
        });

        self.post_json_2xx(&url, &request_body).await.map(|_| ())
    }

    /// Deletes the user `id`: cancels the invitation
    /// (`DELETE /v1/userInvitations/{id}`) when `is_pending`, otherwise removes
    /// the active member (`DELETE /v1/users/{id}`).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn delete_user(&self, id: &str, is_pending: bool) -> Result<(), StackError> {
        let url = if is_pending {
            format!("{}/v1/userInvitations/{id}", self.base_url)
        } else {
            format!("{}/v1/users/{id}", self.base_url)
        };
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Lists every registered device of the connected account, sorted by name.
    ///
    /// `GET /v1/devices?sort=name&limit=200`, following `links.next` pagination
    /// until exhausted (`limit` is the page size, not a cap on the total).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_devices(&self) -> Result<Vec<DeviceInfo>, StackError> {
        let mut devices = Vec::new();
        let mut next_url = Some(format!("{}/v1/devices?sort=name&limit=200", self.base_url));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: DevicesResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("devices response: {e}")))?;
            devices.extend(page.data.into_iter().map(DeviceResource::into_device_info));

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(devices)
    }

    /// Registers a new device with `name`, ASC `platform`, and `udid`.
    ///
    /// `POST /v1/devices` with a JSON:API body whose `attributes` carry
    /// `name`/`platform`/`udid`. `platform` is forwarded verbatim (App Store
    /// Connect validates the `BundleIdPlatform` enum). Returns the created device.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn create_device(
        &self,
        name: &str,
        platform: &str,
        udid: &str,
    ) -> Result<DeviceInfo, StackError> {
        let url = format!("{}/v1/devices", self.base_url);
        let request_body = json!({
            "data": {
                "type": "devices",
                "attributes": {
                    "name": name,
                    "platform": platform,
                    "udid": udid,
                }
            }
        });

        let response_body = self.post_json_2xx(&url, &request_body).await?;
        let document: DeviceDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("device response: {e}")))?;
        Ok(document.data.into_device_info())
    }

    /// Updates the device `id`, sending only the attributes that are `Some`.
    ///
    /// `PATCH /v1/devices/{id}`. `name` renames the device; `status`
    /// (`"DISABLED"` removes it from the account, `"ENABLED"` re-enables it) is
    /// forwarded verbatim. Attributes left `None` are omitted entirely. Any 2xx →
    /// `Ok(())` (the response is discarded).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn update_device(
        &self,
        id: &str,
        name: Option<&str>,
        status: Option<&str>,
    ) -> Result<(), StackError> {
        let url = format!("{}/v1/devices/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;

        let mut attributes = serde_json::Map::new();
        if let Some(value) = name {
            attributes.insert("name".into(), json!(value));
        }
        if let Some(value) = status {
            attributes.insert("status".into(), json!(value));
        }

        let request_body = json!({
            "data": {
                "type": "devices",
                "id": id,
                "attributes": attributes
            }
        });

        self.patch_no_content(&url, &token, &request_body).await
    }

    /// Lists every bundle ID of the connected account, sorted by name.
    ///
    /// `GET /v1/bundleIds?sort=name&limit=200`, following `links.next` pagination
    /// until exhausted (`limit` is the page size, not a cap on the total).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_bundle_ids(&self) -> Result<Vec<BundleIdInfo>, StackError> {
        let mut bundle_ids = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/bundleIds?sort=name&limit=200",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: BundleIdsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("bundleIds response: {e}")))?;
            bundle_ids.extend(
                page.data
                    .into_iter()
                    .map(BundleIdResource::into_bundle_id_info),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(bundle_ids)
    }

    /// Registers a new bundle ID with `identifier`, `name`, and ASC `platform`.
    ///
    /// `POST /v1/bundleIds` with a JSON:API body whose `attributes` carry
    /// `name`/`platform`/`identifier`. `platform` is forwarded verbatim (App Store
    /// Connect validates the `BundleIdPlatform` enum). Returns the created bundle
    /// ID.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn create_bundle_id(
        &self,
        identifier: &str,
        name: &str,
        platform: &str,
    ) -> Result<BundleIdInfo, StackError> {
        let url = format!("{}/v1/bundleIds", self.base_url);
        let request_body = json!({
            "data": {
                "type": "bundleIds",
                "attributes": {
                    "name": name,
                    "platform": platform,
                    "identifier": identifier,
                }
            }
        });

        let response_body = self.post_json_2xx(&url, &request_body).await?;
        let document: BundleIdDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("bundleId response: {e}")))?;
        Ok(document.data.into_bundle_id_info())
    }

    /// Renames the bundle ID `id` (only `name` is mutable).
    ///
    /// `PATCH /v1/bundleIds/{id}` with a JSON:API body carrying only the `name`
    /// attribute. Any 2xx → `Ok(())` (the response is discarded).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn update_bundle_id(&self, id: &str, name: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/bundleIds/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let request_body = json!({
            "data": {
                "type": "bundleIds",
                "id": id,
                "attributes": { "name": name }
            }
        });

        self.patch_no_content(&url, &token, &request_body).await
    }

    /// Deletes the bundle ID `id`.
    ///
    /// `DELETE /v1/bundleIds/{id}`. Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn delete_bundle_id(&self, id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/bundleIds/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Lists the capabilities enabled on `bundle_id`.
    ///
    /// `GET /v1/bundleIds/{bundleId}/bundleIdCapabilities`, following `links.next`
    /// pagination until exhausted. No `limit` query parameter is sent: the
    /// relationship endpoint rejects it with `PARAMETER_ERROR.ILLEGAL`. Resources
    /// whose `capabilityType` is missing or empty are skipped.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_bundle_id_capabilities(
        &self,
        bundle_id: &str,
    ) -> Result<Vec<BundleIdCapabilityInfo>, StackError> {
        let mut capabilities = Vec::new();
        // NB: no `limit` param — the relationship endpoint rejects it.
        let mut next_url = Some(format!(
            "{}/v1/bundleIds/{bundle_id}/bundleIdCapabilities",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: BundleIdCapabilitiesResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("bundleIdCapabilities response: {e}")))?;
            capabilities.extend(
                page.data
                    .into_iter()
                    .filter_map(BundleIdCapabilityResource::into_capability_info),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(capabilities)
    }

    /// Enables `capability_type` on `bundle_id`, returning the created capability.
    ///
    /// `POST /v1/bundleIdCapabilities` with a JSON:API body whose `attributes`
    /// carry the raw `capabilityType` string and whose `relationships.bundleId`
    /// points at `bundle_id`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn enable_capability(
        &self,
        bundle_id: &str,
        capability_type: &str,
    ) -> Result<BundleIdCapabilityInfo, StackError> {
        let url = format!("{}/v1/bundleIdCapabilities", self.base_url);
        let request_body = json!({
            "data": {
                "type": "bundleIdCapabilities",
                "attributes": { "capabilityType": capability_type },
                "relationships": {
                    "bundleId": {
                        "data": { "type": "bundleIds", "id": bundle_id }
                    }
                }
            }
        });

        let response_body = self.post_json_2xx(&url, &request_body).await?;
        let document: BundleIdCapabilityDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("bundleIdCapability response: {e}")))?;
        document.data.into_capability_info().ok_or_else(|| {
            StackError::decode("bundleIdCapability response: missing capabilityType".to_string())
        })
    }

    /// Disables the capability `capability_id`.
    ///
    /// `DELETE /v1/bundleIdCapabilities/{capabilityId}`. Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn disable_capability(&self, capability_id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/bundleIdCapabilities/{capability_id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Lists every certificate of the connected account, sorted by display name.
    ///
    /// `GET /v1/certificates?sort=displayName&limit=200`, following `links.next`
    /// pagination until exhausted (`limit` is the page size, not a cap on the
    /// total). The list omits certificate content, so every entry's
    /// `certificate_content` is `None`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_certificates(&self) -> Result<Vec<CertificateInfo>, StackError> {
        let mut certificates = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/certificates?sort=displayName&limit=200",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: CertificatesResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("certificates response: {e}")))?;
            certificates.extend(
                page.data
                    .into_iter()
                    .map(CertificateResource::into_certificate_info),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(certificates)
    }

    /// Fetches the base64 `certificateContent` of the certificate `id`.
    ///
    /// `GET /v1/certificates/{id}?fields[certificates]=certificateContent,...`,
    /// requesting the content field explicitly (the list omits it). Returns
    /// `None` when the attribute is absent.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_certificate_content(
        &self,
        id: &str,
    ) -> Result<Option<String>, StackError> {
        let url = format!(
            "{}/v1/certificates/{id}?fields[certificates]=certificateContent,displayName,name,certificateType,platform,serialNumber,expirationDate,activated",
            self.base_url
        );
        let body = self.get_page(&url).await?;
        let document: CertificateDocument = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("certificate response: {e}")))?;
        Ok(document.data.attributes.certificate_content)
    }

    /// Creates a certificate from `csr_content` of `certificate_type`, optionally
    /// related to a Pass Type ID or an Apple Pay merchant ID.
    ///
    /// `POST /v1/certificates` with a JSON:API body whose `attributes` carry the
    /// raw `csrContent`/`certificateType`. When `pass_type_id` is `Some` and
    /// non-empty it is attached as the `passTypeId` relationship; otherwise when
    /// `merchant_id` is `Some` and non-empty it is attached as the `merchantId`
    /// relationship; otherwise no `relationships` object is sent. The response
    /// carries the created certificate including its `certificateContent`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn create_certificate(
        &self,
        csr_content: &str,
        certificate_type: &str,
        pass_type_id: Option<&str>,
        merchant_id: Option<&str>,
    ) -> Result<CertificateInfo, StackError> {
        let url = format!("{}/v1/certificates", self.base_url);

        let mut data = json!({
            "type": "certificates",
            "attributes": {
                "csrContent": csr_content,
                "certificateType": certificate_type,
            }
        });

        // passTypeId wins over merchantId; empty strings are treated as absent.
        if let Some(id) = pass_type_id.filter(|v| !v.is_empty()) {
            data["relationships"] = json!({
                "passTypeId": { "data": { "type": "passTypeIds", "id": id } }
            });
        } else if let Some(id) = merchant_id.filter(|v| !v.is_empty()) {
            data["relationships"] = json!({
                "merchantId": { "data": { "type": "merchantIds", "id": id } }
            });
        }

        let request_body = json!({ "data": data });

        let response_body = self.post_json_2xx(&url, &request_body).await?;
        let document: CertificateDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("certificate response: {e}")))?;
        Ok(document.data.into_certificate_info())
    }

    /// Revokes (deletes) the certificate `id`.
    ///
    /// `DELETE /v1/certificates/{id}`. Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn revoke_certificate(&self, id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/certificates/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Lists every provisioning profile of the connected account, sorted by
    /// name.
    ///
    /// `GET /v1/profiles?sort=name&limit=200&include=bundleId`, following
    /// `links.next` pagination until exhausted (`limit` is the page size, not a
    /// cap on the total). Each page's `included[]` carries the referenced
    /// `bundleIds`; a profile's `bundle_id` is resolved to the referenced bundle
    /// ID's `identifier` (per-page — each page's `included[]` covers that page's
    /// profiles). The list omits profile content, so every entry's
    /// `profile_content` is `None`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx page, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_profiles(&self) -> Result<Vec<ProvisioningProfileInfo>, StackError> {
        let mut profiles = Vec::new();
        let mut next_url = Some(format!(
            "{}/v1/profiles?sort=name&limit=200&include=bundleId",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: ProfilesResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("profiles response: {e}")))?;

            // Index this page's included bundleIds by id → identifier, then map
            // each profile resolving its bundle identifier via the relationship.
            let bundle_ids: HashMap<String, Option<String>> = page
                .included
                .into_iter()
                .filter_map(|inc| match inc {
                    ProfileIncluded::BundleIds { id, attributes } => {
                        Some((id, attributes.identifier))
                    }
                    ProfileIncluded::Other => None,
                })
                .collect();

            profiles.extend(
                page.data
                    .into_iter()
                    .map(|p| p.into_profile_info(&bundle_ids)),
            );

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(profiles)
    }

    /// Creates a provisioning profile from `name`, `profile_type`, the bundle ID
    /// `bundle_id_id`, the signing `certificate_ids`, and the `device_ids`.
    ///
    /// `POST /v1/profiles` with a JSON:API body whose `attributes` carry
    /// `name`/`profileType` (the raw `profileType` is forwarded verbatim) and
    /// whose `relationships` always carry `bundleId` (to-one) and `certificates`
    /// (to-many, even when empty). The `devices` relationship is attached only
    /// when `device_ids` is non-empty; it is omitted entirely otherwise (App
    /// Store Connect rejects an empty `devices` array for non-development
    /// profiles). The response carries the created profile including its
    /// `profileContent`. The created profile's `bundle_id` is left `None` (the
    /// host does not resolve it on create).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn create_profile(
        &self,
        name: &str,
        profile_type: &str,
        bundle_id_id: &str,
        certificate_ids: &[String],
        device_ids: &[String],
    ) -> Result<ProvisioningProfileInfo, StackError> {
        let url = format!("{}/v1/profiles", self.base_url);

        let certificates: Vec<serde_json::Value> = certificate_ids
            .iter()
            .map(|id| json!({ "type": "certificates", "id": id }))
            .collect();

        let mut relationships = json!({
            "bundleId": {
                "data": { "type": "bundleIds", "id": bundle_id_id }
            },
            // certificates is ALWAYS sent, even when the list is empty.
            "certificates": { "data": certificates }
        });

        // devices is OMITTED entirely when there are no device ids.
        if !device_ids.is_empty() {
            let devices: Vec<serde_json::Value> = device_ids
                .iter()
                .map(|id| json!({ "type": "devices", "id": id }))
                .collect();
            relationships["devices"] = json!({ "data": devices });
        }

        let request_body = json!({
            "data": {
                "type": "profiles",
                "attributes": {
                    "name": name,
                    "profileType": profile_type,
                },
                "relationships": relationships
            }
        });

        let response_body = self.post_json_2xx(&url, &request_body).await?;
        let document: ProfileDocument = serde_json::from_str(&response_body)
            .map_err(|e| StackError::decode(format!("profile response: {e}")))?;
        // The create path does not resolve the bundle identifier; pass an empty
        // index so `bundle_id` is `None`.
        Ok(document.data.into_profile_info(&HashMap::new()))
    }

    /// Deletes the profile `id`.
    ///
    /// `DELETE /v1/profiles/{id}`. Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn delete_profile(&self, id: &str) -> Result<(), StackError> {
        let url = format!("{}/v1/profiles/{id}", self.base_url);
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.delete(&url).bearer_auth(token))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Fetches the base64 `profileContent` of the profile `id`.
    ///
    /// `GET /v1/profiles/{id}?fields[profiles]=profileContent,...`, requesting the
    /// content field explicitly (the list omits it). Returns `None` when the
    /// attribute is absent.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// on malformed JSON, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_profile_content(
        &self,
        id: &str,
    ) -> Result<Option<String>, StackError> {
        let url = format!(
            "{}/v1/profiles/{id}?fields[profiles]=profileContent,name,profileType,platform,profileState,uuid,createdDate,expirationDate",
            self.base_url
        );
        let body = self.get_page(&url).await?;
        let document: ProfileDocument = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("profile response: {e}")))?;
        Ok(document.data.attributes.profile_content)
    }

    async fn patch_no_content(
        &self,
        url: &str,
        token: &str,
        request_body: &serde_json::Value,
    ) -> Result<(), StackError> {
        let (status, body) = self
            .send_and_read(self.http.patch(url).bearer_auth(token).json(request_body))
            .await?;
        if status.is_success() {
            return Ok(());
        }

        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Authenticated `GET` of one JSON:API page, returning the raw body or mapping
    /// the failure: non-2xx → [`StackError::Http`], transport → [`StackError::Network`].
    async fn get_page(&self, url: &str) -> Result<String, StackError> {
        let token = self.auth.bearer_token().await?;
        let (status, body) = self
            .send_and_read(self.http.get(url).bearer_auth(token))
            .await?;
        if !status.is_success() {
            if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
                return Err(err);
            }
            return Err(StackError::Http {
                status: status.as_u16(),
                message: body,
            });
        }
        Ok(body)
    }
}

/// Renders a `reqwest::Request` as a runnable, multi-line cURL command for the
/// debug logger: the method/URL, one `-H` line per header (in reqwest's stored
/// order, Authorization included verbatim — this is an opt-in, debug-only sink),
/// and a `-d` line carrying the pretty-printed JSON body when one is present.
fn render_curl(req: &reqwest::Request) -> String {
    let mut out = format!(
        "[RustCore] → request\n→ curl -X {} '{}'",
        req.method(),
        req.url()
    );

    for (name, value) in req.headers() {
        let value = value.to_str().unwrap_or("<non-utf8>");
        out.push_str(&format!(" \\\n  -H '{name}: {value}'"));
    }

    if let Some(bytes) = req.body().and_then(reqwest::Body::as_bytes) {
        if !bytes.is_empty() {
            out.push_str(&format!(" \\\n  -d '{}'", pretty_json(bytes)));
        }
    }

    out
}

/// Renders a response status line plus its body (pretty-printed when the body is
/// JSON, raw otherwise) for the debug logger.
fn render_response(status: reqwest::StatusCode, body: &str) -> String {
    format!("[RustCore] ← {status}\n{}", pretty_json(body.as_bytes()))
}

/// Pretty-prints `bytes` as JSON; on any parse/serialize failure falls back to
/// the lossy UTF-8 of the raw bytes. Never panics.
fn pretty_json(bytes: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned())
}

/// Detects an App Store Connect "pending agreements" 403 from a non-2xx
/// response. Returns `Some(StackError::PendingAgreements)` when `status` is 403
/// and `body` mentions an agreement/pending; otherwise `None` so the caller
/// applies its normal mapping.
fn pending_agreements_error(status: u16, body: &str) -> Option<StackError> {
    if status == 403 {
        let lowered = body.to_lowercase();
        if lowered.contains("agreement") || lowered.contains("pending") {
            return Some(StackError::pending_agreements(
                "App Store Connect has pending agreements; accept them in the \
                 developer portal before connecting",
            ));
        }
    }
    None
}

/// Maps a non-success `validate` response. A 403 whose body mentions pending
/// agreements becomes a typed [`StackError::PendingAgreements`]; any other
/// failure becomes [`StackError::Auth`].
fn map_error_response(status: u16, body: &str) -> StackError {
    if let Some(err) = pending_agreements_error(status, body) {
        return err;
    }
    StackError::auth(format!("validation failed ({status}): {body}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PRIVATE_P8: &[u8] = include_bytes!("../../../tests/fixtures/test_ec_private.p8");

    fn client(base_url: String) -> AppStoreClient {
        let auth = AppStoreAuthenticator::new("issuer".into(), "kid".into(), PRIVATE_P8.to_vec());
        AppStoreClient::with_base_url(auth, base_url)
    }

    /// A [`DebugLogger`] that captures every emitted message so a test can assert
    /// on what the client logged.
    #[derive(Default)]
    struct CapturingLogger {
        messages: Mutex<Vec<String>>,
    }

    impl CapturingLogger {
        fn messages(&self) -> Vec<String> {
            self.messages.lock().unwrap().clone()
        }
    }

    impl DebugLogger for CapturingLogger {
        fn log(&self, message: String) {
            self.messages.lock().unwrap().push(message);
        }
    }

    /// Builds a client pointed at `base_url` with `logger` attached as its debug
    /// sink.
    fn client_with_logger(base_url: String, logger: Arc<dyn DebugLogger>) -> AppStoreClient {
        client(base_url).with_debug_logger(Some(logger))
    }

    #[tokio::test]
    async fn fetch_apps_maps_and_paginates() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/apps?cursor=PAGE2", server.uri());

        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "apps",
                    "id": "111",
                    "attributes": { "name": "Foo", "bundleId": "com.foo" }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "apps",
                    "id": "222",
                    "attributes": { "name": "Bar", "bundleId": "com.bar" }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let apps = client(server.uri()).fetch_apps().await.unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(
            apps[0],
            AppInfo {
                id: "111".into(),
                name: "Foo".into(),
                bundle_id: "com.foo".into(),
                platform: None,
            }
        );
        assert_eq!(apps[1].id, "222");
        assert_eq!(apps[1].bundle_id, "com.bar");
    }

    #[tokio::test]
    async fn fetch_apps_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_apps().await.unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn validate_ok_on_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .and(query_param("limit", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [], "links": {}
            })))
            .mount(&server)
            .await;

        assert!(client(server.uri()).validate().await.is_ok());
    }

    #[tokio::test]
    async fn validate_errors_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let err = client(server.uri()).validate().await.unwrap_err();
        assert!(matches!(err, StackError::Auth { .. }));
    }

    #[tokio::test]
    async fn validate_explains_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri()).validate().await.unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_page_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending acceptance." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_apps().await.unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_page_403_without_agreement_wording_stays_http() {
        let server = MockServer::start().await;
        // A 403 that is NOT about agreements must keep the generic HTTP mapping,
        // proving the pending-agreements guard is specific to that wording.
        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_apps().await.unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn fetch_customer_reviews_maps_responses_and_paginates() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/apps/APP1/customerReviews?cursor=PAGE2", server.uri());

        // Page 1: a review WITH a developer response (resolved from `included`).
        Mock::given(method("GET"))
            .and(path("/v1/apps/APP1/customerReviews"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "customerReviews",
                    "id": "rev-1",
                    "attributes": {
                        "rating": 5,
                        "title": "Great app",
                        "body": "Love it",
                        "reviewerNickname": "alice",
                        "createdDate": "2026-01-02T03:04:05Z",
                        "territory": "USA"
                    },
                    "relationships": {
                        "response": { "data": { "type": "customerReviewResponses", "id": "resp-1" } }
                    }
                }],
                "included": [{
                    "type": "customerReviewResponses",
                    "id": "resp-1",
                    "attributes": {
                        "responseBody": "Thank you!",
                        "state": "PUBLISHED",
                        "lastModifiedDate": "2026-01-03T00:00:00Z"
                    }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a review WITHOUT a response.
        Mock::given(method("GET"))
            .and(path("/v1/apps/APP1/customerReviews"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "customerReviews",
                    "id": "rev-2",
                    "attributes": {
                        "rating": 2,
                        "title": "Meh",
                        "body": null,
                        "reviewerNickname": "bob",
                        "createdDate": "2026-01-01T00:00:00Z",
                        "territory": "GBR"
                    },
                    "relationships": { "response": { "data": null } }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let reviews = client(server.uri())
            .fetch_customer_reviews("APP1")
            .await
            .unwrap();
        assert_eq!(reviews.len(), 2);

        let with_response = &reviews[0];
        assert_eq!(with_response.id, "rev-1");
        assert_eq!(with_response.rating, 5);
        assert_eq!(with_response.title.as_deref(), Some("Great app"));
        assert_eq!(with_response.territory.as_deref(), Some("USA"));
        assert_eq!(
            with_response.created_date.as_deref(),
            Some("2026-01-02T03:04:05Z")
        );
        assert_eq!(with_response.reviewer_nickname.as_deref(), Some("alice"));
        let response = with_response.response.as_ref().expect("response attached");
        assert_eq!(response.id, "resp-1");
        assert_eq!(response.body.as_deref(), Some("Thank you!"));
        assert_eq!(response.state.as_deref(), Some("PUBLISHED"));
        assert_eq!(
            response.last_modified_date.as_deref(),
            Some("2026-01-03T00:00:00Z")
        );

        let without_response = &reviews[1];
        assert_eq!(without_response.id, "rev-2");
        assert_eq!(without_response.rating, 2);
        assert!(without_response.body.is_none());
        assert!(without_response.response.is_none());
    }

    #[tokio::test]
    async fn fetch_customer_reviews_page_first_page_returns_token() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/apps/APP1/customerReviews?cursor=PAGE2", server.uri());

        Mock::given(method("GET"))
            .and(path("/v1/apps/APP1/customerReviews"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "customerReviews",
                    "id": "rev-1",
                    "attributes": {
                        "rating": 5,
                        "title": "Great app",
                        "body": "Love it",
                        "reviewerNickname": "alice",
                        "createdDate": "2026-01-02T03:04:05Z",
                        "territory": "USA"
                    },
                    "relationships": {
                        "response": { "data": { "type": "customerReviewResponses", "id": "resp-1" } }
                    }
                }],
                "included": [{
                    "type": "customerReviewResponses",
                    "id": "resp-1",
                    "attributes": {
                        "responseBody": "Thank you!",
                        "state": "PUBLISHED",
                        "lastModifiedDate": "2026-01-03T00:00:00Z"
                    }
                }],
                "links": { "next": next.clone() }
            })))
            .mount(&server)
            .await;

        let page = client(server.uri())
            .fetch_customer_reviews_page("APP1", "-createdDate", &[], 50, None)
            .await
            .unwrap();

        assert_eq!(page.reviews.len(), 1);
        let review = &page.reviews[0];
        assert_eq!(review.id, "rev-1");
        assert_eq!(review.rating, 5);
        let response = review.response.as_ref().expect("response attached");
        assert_eq!(response.id, "resp-1");
        assert_eq!(response.body.as_deref(), Some("Thank you!"));
        assert_eq!(page.next_token, Some(next));
    }

    #[tokio::test]
    async fn fetch_customer_reviews_page_follows_token() {
        let server = MockServer::start().await;
        let token = format!("{}/v1/apps/APP1/customerReviews?cursor=PAGE2", server.uri());

        // The exact path/cursor encoded in the token must be fetched verbatim.
        Mock::given(method("GET"))
            .and(path("/v1/apps/APP1/customerReviews"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "customerReviews",
                    "id": "rev-2",
                    "attributes": {
                        "rating": 2,
                        "title": "Meh",
                        "body": null,
                        "reviewerNickname": "bob",
                        "createdDate": "2026-01-01T00:00:00Z",
                        "territory": "GBR"
                    },
                    "relationships": { "response": { "data": null } }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let page = client(server.uri())
            .fetch_customer_reviews_page("APP1", "-createdDate", &[], 50, Some(&token))
            .await
            .unwrap();

        assert_eq!(page.reviews.len(), 1);
        assert_eq!(page.reviews[0].id, "rev-2");
        assert!(page.reviews[0].response.is_none());
        assert_eq!(page.next_token, None);
    }

    #[tokio::test]
    async fn fetch_customer_reviews_page_applies_filter_and_sort() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/apps/APP1/customerReviews"))
            .and(query_param("sort", "-rating"))
            .and(query_param("filter[rating]", "4,5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [],
                "links": {}
            })))
            .mount(&server)
            .await;

        let page = client(server.uri())
            .fetch_customer_reviews_page(
                "APP1",
                "-rating",
                &["4".to_string(), "5".to_string()],
                50,
                None,
            )
            .await
            .unwrap();

        assert!(page.reviews.is_empty());
        assert_eq!(page.next_token, None);
    }

    #[tokio::test]
    async fn fetch_review_submissions_resolves_version_and_actor() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions"))
            .and(query_param("filter[app]", "APP1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "type": "reviewSubmissions",
                        "id": "sub-1",
                        "attributes": {
                            "platform": "IOS",
                            "submittedDate": "2026-02-01T00:00:00Z",
                            "state": "WAITING_FOR_REVIEW"
                        },
                        "relationships": {
                            "appStoreVersionForReview": { "data": { "type": "appStoreVersions", "id": "ver-1" } },
                            "submittedByActor": { "data": { "type": "actors", "id": "actor-1" } }
                        }
                    },
                    {
                        "type": "reviewSubmissions",
                        "id": "sub-2",
                        "attributes": {
                            "platform": "IOS",
                            "submittedDate": "2026-02-02T00:00:00Z",
                            "state": "IN_REVIEW"
                        },
                        "relationships": {
                            "appStoreVersionForReview": { "data": { "type": "appStoreVersions", "id": "ver-1" } },
                            "submittedByActor": { "data": { "type": "actors", "id": "actor-2" } }
                        }
                    }
                ],
                "included": [
                    {
                        "type": "appStoreVersions",
                        "id": "ver-1",
                        "attributes": { "versionString": "1.4.0" }
                    },
                    {
                        "type": "actors",
                        "id": "actor-1",
                        "attributes": {
                            "userFirstName": "Jane",
                            "userLastName": "Doe",
                            "userEmail": "jane@example.com",
                            "actorType": "USER"
                        }
                    },
                    {
                        "type": "actors",
                        "id": "actor-2",
                        "attributes": {
                            "apiKeyId": "ABC123",
                            "actorType": "API_KEY"
                        }
                    }
                ],
                "links": {}
            })))
            .mount(&server)
            .await;

        let submissions = client(server.uri())
            .fetch_review_submissions("APP1")
            .await
            .unwrap();
        assert_eq!(submissions.len(), 2);

        // "first last" actor case.
        let by_user = &submissions[0];
        assert_eq!(by_user.id, "sub-1");
        assert_eq!(by_user.app_id, "APP1");
        assert_eq!(by_user.platform.as_deref(), Some("IOS"));
        assert_eq!(by_user.state.as_deref(), Some("WAITING_FOR_REVIEW"));
        assert_eq!(by_user.version_string.as_deref(), Some("1.4.0"));
        assert_eq!(by_user.version_id.as_deref(), Some("ver-1"));
        assert_eq!(by_user.submitted_by_name.as_deref(), Some("Jane Doe"));
        assert_eq!(
            by_user.submitted_by_email.as_deref(),
            Some("jane@example.com")
        );

        // API-key actor case.
        let by_api_key = &submissions[1];
        assert_eq!(by_api_key.id, "sub-2");
        assert_eq!(by_api_key.version_string.as_deref(), Some("1.4.0"));
        assert_eq!(
            by_api_key.submitted_by_name.as_deref(),
            Some("API Key (ABC123)")
        );
        assert!(by_api_key.submitted_by_email.is_none());
    }

    #[tokio::test]
    async fn reply_to_review_posts_and_maps_response() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/customerReviewResponses"))
            // Assert the request carries the body text and the review relationship id,
            // without over-constraining the rest of the JSON:API envelope.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "customerReviewResponses",
                    "attributes": { "responseBody": "Thanks for the feedback!" },
                    "relationships": {
                        "review": { "data": { "type": "customerReviews", "id": "rev-1" } }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "customerReviewResponses",
                    "id": "resp-1",
                    "attributes": {
                        "responseBody": "Thanks for the feedback!",
                        "state": "PENDING_PUBLISH",
                        "lastModifiedDate": "2026-03-01T12:00:00Z"
                    }
                }
            })))
            .mount(&server)
            .await;

        let response = client(server.uri())
            .reply_to_review("rev-1", "Thanks for the feedback!")
            .await
            .unwrap();

        assert_eq!(response.id, "resp-1");
        assert_eq!(response.body.as_deref(), Some("Thanks for the feedback!"));
        assert_eq!(response.state.as_deref(), Some("PENDING_PUBLISH"));
        assert_eq!(
            response.last_modified_date.as_deref(),
            Some("2026-03-01T12:00:00Z")
        );
    }

    #[tokio::test]
    async fn reply_to_review_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/customerReviewResponses"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .reply_to_review("rev-1", "hi")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn delete_review_response_succeeds_on_204() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/customerReviewResponses/resp-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .delete_review_response("resp-1")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn delete_review_response_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/customerReviewResponses/resp-1"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .delete_review_response("resp-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn fetch_versions_maps_fields() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/apps/APP1/appStoreVersions"))
            .and(query_param("limit", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "type": "appStoreVersions",
                        "id": "ver-1",
                        "attributes": {
                            "platform": "IOS",
                            "appStoreState": "READY_FOR_SALE",
                            "appVersionState": "ACCEPTED",
                            "versionString": "1.4.0",
                            "copyright": "2026 Acme",
                            "releaseType": "MANUAL",
                            "createdDate": "2026-01-02T03:04:05Z"
                        }
                    },
                    {
                        "type": "appStoreVersions",
                        "id": "ver-2",
                        "attributes": {
                            "platform": "IOS",
                            "appStoreState": "PREPARE_FOR_SUBMISSION",
                            "versionString": "1.5.0",
                            "releaseType": "AFTER_APPROVAL",
                            "createdDate": "2026-02-02T00:00:00Z"
                        }
                    }
                ]
            })))
            .mount(&server)
            .await;

        let versions = client(server.uri())
            .fetch_versions("APP1", 20)
            .await
            .unwrap();
        assert_eq!(versions.len(), 2);

        let first = &versions[0];
        assert_eq!(first.id, "ver-1");
        assert_eq!(first.app_id, "APP1");
        assert_eq!(first.platform.as_deref(), Some("IOS"));
        assert_eq!(first.app_store_state.as_deref(), Some("READY_FOR_SALE"));
        assert_eq!(first.app_version_state.as_deref(), Some("ACCEPTED"));
        assert_eq!(first.version_string.as_deref(), Some("1.4.0"));
        assert_eq!(first.copyright.as_deref(), Some("2026 Acme"));
        assert_eq!(first.release_type.as_deref(), Some("MANUAL"));
        assert_eq!(first.created_date.as_deref(), Some("2026-01-02T03:04:05Z"));

        let second = &versions[1];
        assert_eq!(second.id, "ver-2");
        assert_eq!(second.app_id, "APP1");
        assert_eq!(second.version_string.as_deref(), Some("1.5.0"));
        assert_eq!(second.release_type.as_deref(), Some("AFTER_APPROVAL"));
        assert!(second.app_version_state.is_none());
        assert!(second.copyright.is_none());
    }

    #[tokio::test]
    async fn fetch_versions_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps/APP1/appStoreVersions"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_versions("APP1", 20)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn create_version_posts_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/appStoreVersions"))
            // Assert the request carries platform/versionString/releaseType and the
            // app relationship id, without over-constraining the envelope.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "appStoreVersions",
                    "attributes": {
                        "platform": "IOS",
                        "versionString": "2.0.0",
                        "releaseType": "MANUAL"
                    },
                    "relationships": {
                        "app": { "data": { "type": "apps", "id": "APP1" } }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "appStoreVersions",
                    "id": "ver-new",
                    "attributes": {
                        "platform": "IOS",
                        "appStoreState": "PREPARE_FOR_SUBMISSION",
                        "versionString": "2.0.0",
                        "releaseType": "MANUAL",
                        "createdDate": "2026-03-01T12:00:00Z"
                    }
                }
            })))
            .mount(&server)
            .await;

        let version = client(server.uri())
            .create_version("APP1", "IOS", "2.0.0")
            .await
            .unwrap();

        assert_eq!(version.id, "ver-new");
        assert_eq!(version.app_id, "APP1");
        assert_eq!(version.platform.as_deref(), Some("IOS"));
        assert_eq!(
            version.app_store_state.as_deref(),
            Some("PREPARE_FOR_SUBMISSION")
        );
        assert_eq!(version.version_string.as_deref(), Some("2.0.0"));
        assert_eq!(version.release_type.as_deref(), Some("MANUAL"));
        assert_eq!(
            version.created_date.as_deref(),
            Some("2026-03-01T12:00:00Z")
        );
    }

    #[tokio::test]
    async fn create_version_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/appStoreVersions"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_version("APP1", "IOS", "2.0.0")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn update_version_sends_only_provided_attributes() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/appStoreVersions/V1"))
            // The provided attributes (versionString, releaseType) must be present.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "appStoreVersions",
                    "id": "V1",
                    "attributes": {
                        "versionString": "3.1.0",
                        "releaseType": "MANUAL"
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "appStoreVersions",
                    "id": "V1",
                    "attributes": {}
                }
            })))
            .mount(&server)
            .await;

        let result = client(server.uri())
            .update_version("V1", Some("3.1.0"), None, Some("MANUAL"), None)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn update_version_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/appStoreVersions/V1"))
            .respond_with(ResponseTemplate::new(422).set_body_string("unprocessable"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .update_version("V1", Some("3.1.0"), None, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 422, .. }));
    }

    #[tokio::test]
    async fn delete_version_succeeds_on_204() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/appStoreVersions/V1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri()).delete_version("V1").await.is_ok());
    }

    #[tokio::test]
    async fn delete_version_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/appStoreVersions/V1"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri()).delete_version("V1").await.unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn fetch_phased_release_maps_all_fields() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path(
                "/v1/appStoreVersions/ver-1/appStoreVersionPhasedRelease",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "appStoreVersionPhasedReleases",
                    "id": "phr-1",
                    "attributes": {
                        "phasedReleaseState": "ACTIVE",
                        "startDate": "2026-03-01T12:00:00Z",
                        "totalPauseDuration": 2,
                        "currentDayNumber": 3
                    }
                }
            })))
            .mount(&server)
            .await;

        let release = client(server.uri())
            .fetch_phased_release("ver-1")
            .await
            .unwrap()
            .expect("expected a phased release");

        assert_eq!(release.id, "phr-1");
        // The ASC `phasedReleaseState` attribute maps onto the record's `state`.
        assert_eq!(release.state.as_deref(), Some("ACTIVE"));
        assert_eq!(release.start_date.as_deref(), Some("2026-03-01T12:00:00Z"));
        assert_eq!(release.total_pause_duration, Some(2));
        assert_eq!(release.current_day_number, Some(3));
    }

    #[tokio::test]
    async fn fetch_phased_release_returns_none_when_data_null() {
        let server = MockServer::start().await;

        // ASC may return a document with a null `data` when no phased release
        // is attached to the version.
        Mock::given(method("GET"))
            .and(path(
                "/v1/appStoreVersions/ver-1/appStoreVersionPhasedRelease",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": null
            })))
            .mount(&server)
            .await;

        let release = client(server.uri())
            .fetch_phased_release("ver-1")
            .await
            .unwrap();
        assert!(release.is_none());
    }

    #[tokio::test]
    async fn fetch_phased_release_returns_none_on_404() {
        let server = MockServer::start().await;

        // A 404 (no phased-release relationship) resolves to `Ok(None)`.
        Mock::given(method("GET"))
            .and(path(
                "/v1/appStoreVersions/ver-1/appStoreVersionPhasedRelease",
            ))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let release = client(server.uri())
            .fetch_phased_release("ver-1")
            .await
            .unwrap();
        assert!(release.is_none());
    }

    #[tokio::test]
    async fn create_phased_release_posts_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/appStoreVersionPhasedReleases"))
            // Assert the request carries the phasedReleaseState attribute and the
            // appStoreVersion relationship id.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "appStoreVersionPhasedReleases",
                    "attributes": { "phasedReleaseState": "ACTIVE" },
                    "relationships": {
                        "appStoreVersion": {
                            "data": { "type": "appStoreVersions", "id": "ver-1" }
                        }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "appStoreVersionPhasedReleases",
                    "id": "phr-new",
                    "attributes": {
                        "phasedReleaseState": "ACTIVE",
                        "currentDayNumber": 1
                    }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let release = client(server.uri())
            .create_phased_release("ver-1", "ACTIVE")
            .await
            .unwrap();

        assert_eq!(release.id, "phr-new");
        assert_eq!(release.state.as_deref(), Some("ACTIVE"));
        assert_eq!(release.current_day_number, Some(1));
    }

    #[tokio::test]
    async fn delete_phased_release_succeeds_on_204() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/appStoreVersionPhasedReleases/phr-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .delete_phased_release("phr-1")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn update_phased_release_state_patches_state() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/appStoreVersionPhasedReleases/phr-1"))
            // The phasedReleaseState attribute must be patched.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "appStoreVersionPhasedReleases",
                    "id": "phr-1",
                    "attributes": { "phasedReleaseState": "PAUSED" }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "appStoreVersionPhasedReleases",
                    "id": "phr-1",
                    "attributes": { "phasedReleaseState": "PAUSED" }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let release = client(server.uri())
            .update_phased_release_state("phr-1", "PAUSED")
            .await
            .unwrap();

        assert_eq!(release.id, "phr-1");
        assert_eq!(release.state.as_deref(), Some("PAUSED"));
    }

    #[tokio::test]
    async fn create_phased_release_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/appStoreVersionPhasedReleases"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string("There are pending agreements that must be accepted"),
            )
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_phased_release("ver-1", "ACTIVE")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::PendingAgreements { .. }));
    }

    #[tokio::test]
    async fn submit_for_review_chains_three_calls_with_platform() {
        let server = MockServer::start().await;

        // 1. Create submission — platform present, app relationship set.
        Mock::given(method("POST"))
            .and(path("/v1/reviewSubmissions"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "reviewSubmissions",
                    "attributes": { "platform": "IOS" },
                    "relationships": {
                        "app": { "data": { "type": "apps", "id": "APP1" } }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": { "type": "reviewSubmissions", "id": "sub-1" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        // 2. Attach the version as a submission item.
        Mock::given(method("POST"))
            .and(path("/v1/reviewSubmissionItems"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "reviewSubmissionItems",
                    "relationships": {
                        "reviewSubmission": {
                            "data": { "type": "reviewSubmissions", "id": "sub-1" }
                        },
                        "appStoreVersion": {
                            "data": { "type": "appStoreVersions", "id": "VER1" }
                        }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": { "type": "reviewSubmissionItems", "id": "item-1" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        // 3. Mark the submission submitted (ASC attribute key is `submitted`).
        Mock::given(method("PATCH"))
            .and(path("/v1/reviewSubmissions/sub-1"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "reviewSubmissions",
                    "id": "sub-1",
                    "attributes": { "submitted": true }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "type": "reviewSubmissions", "id": "sub-1" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let result = client(server.uri())
            .submit_for_review("APP1", "VER1", Some("IOS"))
            .await;
        assert!(result.is_ok());
        // Each `.expect(1)` is verified on server drop.
    }

    #[tokio::test]
    async fn submit_for_review_omits_attributes_when_platform_absent() {
        let server = MockServer::start().await;

        // The create body must omit `attributes` entirely when platform is None.
        Mock::given(method("POST"))
            .and(path("/v1/reviewSubmissions"))
            .and(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                body["data"].get("attributes").is_none()
            })
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": { "type": "reviewSubmissions", "id": "sub-2" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/reviewSubmissionItems"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": { "type": "reviewSubmissionItems", "id": "item-2" }
            })))
            .mount(&server)
            .await;

        Mock::given(method("PATCH"))
            .and(path("/v1/reviewSubmissions/sub-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "type": "reviewSubmissions", "id": "sub-2" }
            })))
            .mount(&server)
            .await;

        let result = client(server.uri())
            .submit_for_review("APP1", "VER1", None)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn submit_for_review_surfaces_pending_agreements_on_create() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/reviewSubmissions"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .submit_for_review("APP1", "VER1", Some("IOS"))
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::PendingAgreements { .. }));
    }

    #[tokio::test]
    async fn cancel_review_patches_active_submission() {
        let server = MockServer::start().await;

        // GET filtered by the active states returns one submission.
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions"))
            .and(query_param("filter[state]", "WAITING_FOR_REVIEW,IN_REVIEW"))
            .and(query_param("filter[app]", "APP1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "type": "reviewSubmissions", "id": "sub-9" }]
            })))
            .mount(&server)
            .await;

        // PATCH sets `canceled: true`.
        Mock::given(method("PATCH"))
            .and(path("/v1/reviewSubmissions/sub-9"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "reviewSubmissions",
                    "id": "sub-9",
                    "attributes": { "canceled": true }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "type": "reviewSubmissions", "id": "sub-9" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client(server.uri()).cancel_review("APP1").await.is_ok());
    }

    #[tokio::test]
    async fn cancel_review_is_noop_when_no_active_submission() {
        let server = MockServer::start().await;

        // Empty page → no PATCH issued. The absence of a PATCH mock proves no
        // PATCH is attempted (any unmatched request would 404 and fail).
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": []
            })))
            .mount(&server)
            .await;

        assert!(client(server.uri()).cancel_review("APP1").await.is_ok());
    }

    #[tokio::test]
    async fn release_version_posts_release_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/appStoreVersionReleaseRequests"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "appStoreVersionReleaseRequests",
                    "relationships": {
                        "appStoreVersion": {
                            "data": { "type": "appStoreVersions", "id": "VER1" }
                        }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": { "type": "appStoreVersionReleaseRequests", "id": "rel-1" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client(server.uri()).release_version("VER1").await.is_ok());
    }

    #[tokio::test]
    async fn release_version_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/appStoreVersionReleaseRequests"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .release_version("VER1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn reject_version_patches_in_review_submission() {
        let server = MockServer::start().await;

        // GET is filtered to active/cancellable states.
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions"))
            .and(query_param("filter[app]", "APP1"))
            .and(query_param(
                "filter[state]",
                "READY_FOR_REVIEW,WAITING_FOR_REVIEW,IN_REVIEW,UNRESOLVED_ISSUES",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "reviewSubmissions",
                    "id": "sub-r",
                    "attributes": { "state": "IN_REVIEW" }
                }]
            })))
            .mount(&server)
            .await;

        // An IN_REVIEW submission is canceled via PATCH { canceled: true }.
        Mock::given(method("PATCH"))
            .and(path("/v1/reviewSubmissions/sub-r"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "reviewSubmissions",
                    "id": "sub-r",
                    "attributes": { "canceled": true }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "type": "reviewSubmissions", "id": "sub-r" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client(server.uri()).reject_version("APP1").await.is_ok());
    }

    #[tokio::test]
    async fn reject_version_deletes_items_of_ready_for_review_submission() {
        let server = MockServer::start().await;

        // One READY_FOR_REVIEW submission.
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions"))
            .and(query_param("filter[app]", "APP1"))
            .and(query_param(
                "filter[state]",
                "READY_FOR_REVIEW,WAITING_FOR_REVIEW,IN_REVIEW,UNRESOLVED_ISSUES",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "reviewSubmissions",
                    "id": "sub-ready",
                    "attributes": { "state": "READY_FOR_REVIEW" }
                }]
            })))
            .mount(&server)
            .await;

        // Its items list returns two items.
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions/sub-ready/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    { "type": "reviewSubmissionItems", "id": "item-1" },
                    { "type": "reviewSubmissionItems", "id": "item-2" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        // Each item is removed via DELETE /v1/reviewSubmissionItems/{itemId}.
        // Exactly two DELETEs are expected (one per item).
        Mock::given(method("DELETE"))
            .and(path("/v1/reviewSubmissionItems/item-1"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/reviewSubmissionItems/item-2"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        // A DELETE on /v1/reviewSubmissions/* (forbidden by Apple) or a PATCH
        // would hit no mock, 404, and fail the `is_ok()` assertion below.
        assert!(client(server.uri()).reject_version("APP1").await.is_ok());
        // `.expect(1)` on each mock is verified on server drop: two item DELETEs,
        // one items GET, and no other mutating request occurred.
    }

    #[tokio::test]
    async fn reject_version_deletes_items_of_all_ready_for_review_submissions() {
        let server = MockServer::start().await;

        // The page contains TWO stale READY_FOR_REVIEW submissions; both must be
        // cleared (production has been seen with several).
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions"))
            .and(query_param("filter[app]", "APP1"))
            .and(query_param(
                "filter[state]",
                "READY_FOR_REVIEW,WAITING_FOR_REVIEW,IN_REVIEW,UNRESOLVED_ISSUES",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "type": "reviewSubmissions",
                        "id": "sub-a",
                        "attributes": { "state": "READY_FOR_REVIEW" }
                    },
                    {
                        "type": "reviewSubmissions",
                        "id": "sub-b",
                        "attributes": { "state": "READY_FOR_REVIEW" }
                    }
                ]
            })))
            .mount(&server)
            .await;

        // Each submission's items are listed and its single item deleted.
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions/sub-a/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "type": "reviewSubmissionItems", "id": "item-a" }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions/sub-b/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "type": "reviewSubmissionItems", "id": "item-b" }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("DELETE"))
            .and(path("/v1/reviewSubmissionItems/item-a"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/reviewSubmissionItems/item-b"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client(server.uri()).reject_version("APP1").await.is_ok());
    }

    #[tokio::test]
    async fn reject_version_surfaces_pending_agreements_on_item_delete() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions"))
            .and(query_param("filter[app]", "APP1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "reviewSubmissions",
                    "id": "sub-ready",
                    "attributes": { "state": "READY_FOR_REVIEW" }
                }]
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions/sub-ready/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "type": "reviewSubmissionItems", "id": "item-1" }]
            })))
            .mount(&server)
            .await;

        // The item DELETE comes back as a pending-agreements 403.
        Mock::given(method("DELETE"))
            .and(path("/v1/reviewSubmissionItems/item-1"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending acceptance." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .reject_version("APP1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::PendingAgreements { .. }));
    }

    #[tokio::test]
    async fn reject_version_is_noop_when_no_submission() {
        let server = MockServer::start().await;
        // GET returns an empty page; no PATCH/DELETE mock is mounted, so any
        // mutating request would fail the test.
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions"))
            .and(query_param("filter[app]", "APP1"))
            .and(query_param(
                "filter[state]",
                "READY_FOR_REVIEW,WAITING_FOR_REVIEW,IN_REVIEW,UNRESOLVED_ISSUES",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client(server.uri()).reject_version("APP1").await.is_ok());
    }

    #[tokio::test]
    async fn reject_version_is_noop_for_non_cancellable_state() {
        let server = MockServer::start().await;
        // A submission in a state we don't act on (e.g. COMPLETING) must not be
        // PATCHed or DELETEd — that is what previously caused a 409. No mutating
        // mock is mounted, so any such request would fail the test.
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions"))
            .and(query_param("filter[app]", "APP1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "reviewSubmissions",
                    "id": "sub-x",
                    "attributes": { "state": "COMPLETING" }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client(server.uri()).reject_version("APP1").await.is_ok());
    }

    #[tokio::test]
    async fn reject_version_is_noop_when_state_missing() {
        let server = MockServer::start().await;
        // A submission with no `state` attribute is treated as non-cancellable.
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions"))
            .and(query_param("filter[app]", "APP1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "type": "reviewSubmissions", "id": "sub-n" }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client(server.uri()).reject_version("APP1").await.is_ok());
    }

    #[tokio::test]
    async fn cancel_review_surfaces_pending_agreements_on_get() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/reviewSubmissions"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending acceptance." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .cancel_review("APP1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::PendingAgreements { .. }));
    }

    #[tokio::test]
    async fn fetch_builds_maps_and_paginates() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/builds?cursor=PAGE2", server.uri());

        // Page 1: a fully-populated build with enrichment relationships resolved
        // from `included[]`, plus the `links.next` cursor. The first request must
        // carry the app filter, the newest-first sort, the limit, and the
        // enrichment `include`.
        Mock::given(method("GET"))
            .and(path("/v1/builds"))
            .and(query_param("filter[app]", "APP1"))
            .and(query_param("sort", "-uploadedDate"))
            .and(query_param("limit", "20"))
            .and(query_param(
                "include",
                "preReleaseVersion,buildBetaDetail,betaAppReviewSubmission",
            ))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "builds",
                    "id": "build-1",
                    "attributes": {
                        "version": "42",
                        "uploadedDate": "2026-03-01T12:00:00Z",
                        "expired": false,
                        "processingState": "VALID",
                        "minOsVersion": "17.0",
                        "expirationDate": "2026-06-01T12:00:00Z",
                        "buildAudienceType": "APP_STORE_ELIGIBLE",
                        "usesNonExemptEncryption": false,
                        "iconAssetToken": {
                            "templateUrl": "https://cdn.example.com/icon/{w}x{h}.{f}",
                            "width": 1024,
                            "height": 1024
                        }
                    },
                    "relationships": {
                        "preReleaseVersion": { "data": { "type": "preReleaseVersions", "id": "prv-1" } },
                        "buildBetaDetail": { "data": { "type": "buildBetaDetails", "id": "bbd-1" } },
                        "betaAppReviewSubmission": { "data": { "type": "betaAppReviewSubmissions", "id": "bars-1" } }
                    }
                }],
                "included": [
                    {
                        "type": "preReleaseVersions",
                        "id": "prv-1",
                        "attributes": { "version": "1.2.3", "platform": "IOS" }
                    },
                    {
                        "type": "buildBetaDetails",
                        "id": "bbd-1",
                        "attributes": {
                            "externalBuildState": "READY_FOR_BETA_TESTING",
                            "internalBuildState": "IN_BETA_TESTING",
                            "autoNotifyEnabled": true
                        }
                    },
                    {
                        "type": "betaAppReviewSubmissions",
                        "id": "bars-1",
                        "attributes": {
                            "betaReviewState": "APPROVED",
                            "submittedDate": "2026-03-02T09:00:00Z"
                        }
                    }
                ],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a sparse build (only the upload date present) and no further page.
        Mock::given(method("GET"))
            .and(path("/v1/builds"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "builds",
                    "id": "build-2",
                    "attributes": {
                        "uploadedDate": "2026-02-01T00:00:00Z"
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let builds = client(server.uri()).fetch_builds("APP1", 20).await.unwrap();
        assert_eq!(builds.len(), 2);

        let first = &builds[0];
        assert_eq!(first.id, "build-1");
        assert_eq!(first.app_id, "APP1");
        assert_eq!(first.version.as_deref(), Some("42"));
        assert_eq!(first.uploaded_date.as_deref(), Some("2026-03-01T12:00:00Z"));
        assert_eq!(first.expired, Some(false));
        assert_eq!(first.processing_state.as_deref(), Some("VALID"));
        assert_eq!(first.min_os_version.as_deref(), Some("17.0"));
        assert_eq!(
            first.expiration_date.as_deref(),
            Some("2026-06-01T12:00:00Z")
        );
        // Enrichment resolved from `included[]` and the icon-token attribute.
        assert_eq!(first.marketing_version.as_deref(), Some("1.2.3"));
        assert_eq!(first.platform.as_deref(), Some("IOS"));
        assert_eq!(
            first.external_build_state.as_deref(),
            Some("READY_FOR_BETA_TESTING")
        );
        assert_eq!(
            first.internal_build_state.as_deref(),
            Some("IN_BETA_TESTING")
        );
        assert_eq!(first.auto_notify_enabled, Some(true));
        assert_eq!(first.beta_review_state.as_deref(), Some("APPROVED"));
        assert_eq!(
            first.submitted_date.as_deref(),
            Some("2026-03-02T09:00:00Z")
        );
        assert_eq!(
            first.build_audience_type.as_deref(),
            Some("APP_STORE_ELIGIBLE")
        );
        assert_eq!(first.uses_non_exempt_encryption, Some(false));
        assert_eq!(
            first.icon_url.as_deref(),
            Some("https://cdn.example.com/icon/1024x1024.png")
        );

        let second = &builds[1];
        assert_eq!(second.id, "build-2");
        assert_eq!(second.app_id, "APP1");
        assert_eq!(
            second.uploaded_date.as_deref(),
            Some("2026-02-01T00:00:00Z")
        );
        assert!(second.version.is_none());
        assert!(second.expired.is_none());
        assert!(second.processing_state.is_none());
        assert!(second.min_os_version.is_none());
        assert!(second.expiration_date.is_none());
        // No relationships / included on the sparse build → enrichment is absent.
        assert!(second.marketing_version.is_none());
        assert!(second.platform.is_none());
        assert!(second.external_build_state.is_none());
        assert!(second.beta_review_state.is_none());
        assert!(second.icon_url.is_none());
    }

    #[tokio::test]
    async fn fetch_builds_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/builds"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_builds("APP1", 20)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn fetch_builds_page_applies_filters_and_returns_next_token() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/builds?cursor=PAGE2", server.uri());

        // First page: must carry the app filter, sort, limit, enrichment include,
        // the platform filter, and the comma-joined processing-state filter.
        Mock::given(method("GET"))
            .and(path("/v1/builds"))
            .and(query_param("filter[app]", "APP1"))
            .and(query_param("sort", "-uploadedDate"))
            .and(query_param("limit", "10"))
            .and(query_param(
                "include",
                "preReleaseVersion,buildBetaDetail,betaAppReviewSubmission",
            ))
            .and(query_param("filter[preReleaseVersion.platform]", "IOS"))
            .and(query_param("filter[processingState]", "VALID,PROCESSING"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "builds",
                    "id": "build-1",
                    "attributes": { "version": "7" },
                    "relationships": {
                        "preReleaseVersion": { "data": { "type": "preReleaseVersions", "id": "prv-1" } }
                    }
                }],
                "included": [{
                    "type": "preReleaseVersions",
                    "id": "prv-1",
                    "attributes": { "version": "3.0.0", "platform": "IOS" }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        let page = client(server.uri())
            .fetch_builds_page(
                "APP1",
                Some("IOS"),
                &["VALID".to_string(), "PROCESSING".to_string()],
                10,
                None,
            )
            .await
            .unwrap();

        assert_eq!(page.builds.len(), 1);
        assert_eq!(page.builds[0].id, "build-1");
        assert_eq!(page.builds[0].app_id, "APP1");
        assert_eq!(page.builds[0].marketing_version.as_deref(), Some("3.0.0"));
        assert_eq!(page.builds[0].platform.as_deref(), Some("IOS"));
        // `next_token` is the opaque `links.next` cursor URL, returned verbatim.
        assert_eq!(
            page.next_token.as_deref(),
            Some(format!("{}/v1/builds?cursor=PAGE2", server.uri()).as_str())
        );
    }

    #[tokio::test]
    async fn fetch_builds_page_follows_opaque_cursor_token() {
        let server = MockServer::start().await;

        // The page_token path fetches the cursor URL verbatim — no filters are
        // re-applied, and the last page reports no `next_token`.
        Mock::given(method("GET"))
            .and(path("/v1/builds"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "builds",
                    "id": "build-2",
                    "attributes": { "version": "8" }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let token = format!("{}/v1/builds?cursor=PAGE2", server.uri());
        let page = client(server.uri())
            .fetch_builds_page("APP1", None, &[], 10, Some(&token))
            .await
            .unwrap();

        assert_eq!(page.builds.len(), 1);
        assert_eq!(page.builds[0].id, "build-2");
        assert!(page.next_token.is_none());
    }

    #[tokio::test]
    async fn fetch_builds_page_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/builds"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_builds_page("APP1", None, &[], 10, None)
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_builds_for_group_maps_and_paginates() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/builds?cursor=GPAGE2", server.uri());

        // First page: filtered by beta group, newest first, with enrichment.
        Mock::given(method("GET"))
            .and(path("/v1/builds"))
            .and(query_param("filter[betaGroups]", "group-1"))
            .and(query_param("sort", "-uploadedDate"))
            .and(query_param("limit", "25"))
            .and(query_param(
                "include",
                "preReleaseVersion,buildBetaDetail,betaAppReviewSubmission",
            ))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "builds",
                    "id": "build-1",
                    "attributes": { "version": "10" },
                    "relationships": {
                        "buildBetaDetail": { "data": { "type": "buildBetaDetails", "id": "bbd-1" } }
                    }
                }],
                "included": [{
                    "type": "buildBetaDetails",
                    "id": "bbd-1",
                    "attributes": { "externalBuildState": "IN_BETA_TESTING" }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Second page: another build and no further page.
        Mock::given(method("GET"))
            .and(path("/v1/builds"))
            .and(query_param("cursor", "GPAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "builds",
                    "id": "build-2",
                    "attributes": { "version": "11" }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let builds = client(server.uri())
            .fetch_builds_for_group("group-1", 25)
            .await
            .unwrap();

        assert_eq!(builds.len(), 2);
        assert_eq!(builds[0].id, "build-1");
        // The owning app id is unknown from this call site → empty.
        assert_eq!(builds[0].app_id, "");
        assert_eq!(
            builds[0].external_build_state.as_deref(),
            Some("IN_BETA_TESTING")
        );
        assert_eq!(builds[1].id, "build-2");
    }

    #[tokio::test]
    async fn fetch_build_detail_maps_build_groups_and_localizations() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/builds/build-1"))
            .and(query_param(
                "include",
                "preReleaseVersion,buildBetaDetail,betaAppReviewSubmission,betaGroups,betaBuildLocalizations",
            ))
            .and(query_param("limit[betaBuildLocalizations]", "50"))
            .and(query_param("limit[betaGroups]", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "builds",
                    "id": "build-1",
                    "attributes": { "version": "42", "processingState": "VALID" },
                    "relationships": {
                        "preReleaseVersion": { "data": { "type": "preReleaseVersions", "id": "prv-1" } },
                        "betaGroups": { "data": [
                            { "type": "betaGroups", "id": "group-1" },
                            { "type": "betaGroups", "id": "group-2" }
                        ] },
                        "betaBuildLocalizations": { "data": [
                            { "type": "betaBuildLocalizations", "id": "loc-1" }
                        ] }
                    }
                },
                "included": [
                    {
                        "type": "preReleaseVersions",
                        "id": "prv-1",
                        "attributes": { "version": "4.5.6", "platform": "IOS" }
                    },
                    {
                        "type": "betaGroups",
                        "id": "group-1",
                        "attributes": { "name": "Internal", "isInternalGroup": true }
                    },
                    {
                        "type": "betaGroups",
                        "id": "group-2",
                        "attributes": { "name": "External", "isInternalGroup": false }
                    },
                    {
                        "type": "betaBuildLocalizations",
                        "id": "loc-1",
                        "attributes": { "locale": "en-US", "whatsNew": "Bug fixes" }
                    }
                ]
            })))
            .mount(&server)
            .await;

        let detail = client(server.uri())
            .fetch_build_detail("build-1")
            .await
            .unwrap();

        assert_eq!(detail.build.id, "build-1");
        assert_eq!(detail.build.app_id, "");
        assert_eq!(detail.build.marketing_version.as_deref(), Some("4.5.6"));
        assert_eq!(detail.build.platform.as_deref(), Some("IOS"));

        assert_eq!(detail.beta_groups.len(), 2);
        assert_eq!(detail.beta_groups[0].id, "group-1");
        assert_eq!(detail.beta_groups[0].name.as_deref(), Some("Internal"));
        assert_eq!(detail.beta_groups[0].is_internal_group, Some(true));
        assert_eq!(detail.beta_groups[1].id, "group-2");

        assert_eq!(detail.localizations.len(), 1);
        assert_eq!(detail.localizations[0].id, "loc-1");
        assert_eq!(detail.localizations[0].locale, "en-US");
        assert_eq!(
            detail.localizations[0].whats_new.as_deref(),
            Some("Bug fixes")
        );
    }

    #[tokio::test]
    async fn fetch_current_build_maps_attached_build() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/appStoreVersions/ver-1/build"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "builds",
                    "id": "build-99",
                    "attributes": {
                        "version": "99",
                        "processingState": "VALID"
                    }
                }
            })))
            .mount(&server)
            .await;

        let build = client(server.uri())
            .fetch_current_build("ver-1")
            .await
            .unwrap();

        let build = build.expect("expected an attached build");
        assert_eq!(build.id, "build-99");
        assert_eq!(build.app_id, "");
        assert_eq!(build.version.as_deref(), Some("99"));
        assert_eq!(build.processing_state.as_deref(), Some("VALID"));
        // No `include` on this path → enrichment is absent.
        assert!(build.marketing_version.is_none());
        assert!(build.icon_url.is_none());
    }

    #[tokio::test]
    async fn fetch_current_build_returns_none_when_data_null() {
        let server = MockServer::start().await;

        // ASC may return a document with a null `data` when no build is attached.
        Mock::given(method("GET"))
            .and(path("/v1/appStoreVersions/ver-1/build"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": null
            })))
            .mount(&server)
            .await;

        let build = client(server.uri())
            .fetch_current_build("ver-1")
            .await
            .unwrap();
        assert!(build.is_none());
    }

    #[tokio::test]
    async fn fetch_current_build_returns_none_on_404() {
        let server = MockServer::start().await;

        // A 404 (no build relationship) resolves to `Ok(None)`, not an error.
        Mock::given(method("GET"))
            .and(path("/v1/appStoreVersions/ver-1/build"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let build = client(server.uri())
            .fetch_current_build("ver-1")
            .await
            .unwrap();
        assert!(build.is_none());
    }

    #[tokio::test]
    async fn expire_build_patches_expired_attribute() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/builds/build-1"))
            // The ASC attribute key is `expired` (boolean), not `isExpired`.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "builds",
                    "id": "build-1",
                    "attributes": { "expired": true }
                }
            })))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        assert!(client(server.uri()).expire_build("build-1").await.is_ok());
    }

    #[tokio::test]
    async fn expire_build_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/builds/build-1"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .expire_build("build-1")
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attach_build_patches_single_relationship_object() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/appStoreVersions/version-1/relationships/build"))
            // The linkage is a single to-one relationship object, NOT an array.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": { "type": "builds", "id": "build-1" }
            })))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .attach_build("version-1", "build-1")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn attach_build_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/appStoreVersions/version-1/relationships/build"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .attach_build("version-1", "build-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn submit_build_for_beta_review_posts_relationship() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/betaAppReviewSubmissions"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "betaAppReviewSubmissions",
                    "relationships": {
                        "build": {
                            "data": { "type": "builds", "id": "build-1" }
                        }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .submit_build_for_beta_review("build-1")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn add_build_to_groups_posts_to_many_linkage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/builds/build-1/relationships/betaGroups"))
            // One linkage entry per group id.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": [
                    { "type": "betaGroups", "id": "group-1" },
                    { "type": "betaGroups", "id": "group-2" }
                ]
            })))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .add_build_to_groups("build-1", &["group-1".to_string(), "group-2".to_string()])
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn remove_build_from_group_deletes_with_body() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/betaGroups/group-1/relationships/builds"))
            // Array carrying the single build to unlink.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": [{ "type": "builds", "id": "build-1" }]
            })))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .remove_build_from_group("build-1", "group-1")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn fetch_beta_groups_maps_and_paginates() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/betaGroups?cursor=PAGE2", server.uri());

        // Page 1: a fully-populated group, plus the `links.next` cursor. The first
        // request must carry the app filter and the limit.
        Mock::given(method("GET"))
            .and(path("/v1/betaGroups"))
            .and(query_param("filter[app]", "APP1"))
            .and(query_param("limit", "20"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "betaGroups",
                    "id": "group-1",
                    "attributes": {
                        "name": "External Testers",
                        "createdDate": "2026-01-02T03:04:05Z",
                        "isInternalGroup": false,
                        "hasAccessToAllBuilds": true,
                        "publicLinkEnabled": true,
                        "publicLink": "https://testflight.apple.com/join/ABC123",
                        "feedbackEnabled": true
                    }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a sparse group (only the name present) and no further page.
        Mock::given(method("GET"))
            .and(path("/v1/betaGroups"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "betaGroups",
                    "id": "group-2",
                    "attributes": {
                        "name": "Internal"
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let groups = client(server.uri())
            .fetch_beta_groups("APP1", 20)
            .await
            .unwrap();
        assert_eq!(groups.len(), 2);

        let first = &groups[0];
        assert_eq!(first.id, "group-1");
        assert_eq!(first.app_id, "APP1");
        assert_eq!(first.name.as_deref(), Some("External Testers"));
        assert_eq!(first.created_date.as_deref(), Some("2026-01-02T03:04:05Z"));
        assert_eq!(first.is_internal_group, Some(false));
        assert_eq!(first.has_access_to_all_builds, Some(true));
        assert_eq!(first.public_link_enabled, Some(true));
        assert_eq!(
            first.public_link.as_deref(),
            Some("https://testflight.apple.com/join/ABC123")
        );
        assert_eq!(first.feedback_enabled, Some(true));

        let second = &groups[1];
        assert_eq!(second.id, "group-2");
        assert_eq!(second.app_id, "APP1");
        assert_eq!(second.name.as_deref(), Some("Internal"));
        assert!(second.created_date.is_none());
        assert!(second.is_internal_group.is_none());
        assert!(second.has_access_to_all_builds.is_none());
        assert!(second.public_link_enabled.is_none());
        assert!(second.public_link.is_none());
        assert!(second.feedback_enabled.is_none());
    }

    #[tokio::test]
    async fn fetch_beta_groups_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/betaGroups"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_beta_groups("APP1", 20)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn fetch_beta_testers_maps_fields() {
        let server = MockServer::start().await;

        // The request must carry the group filter and the limit.
        Mock::given(method("GET"))
            .and(path("/v1/betaTesters"))
            .and(query_param("filter[betaGroups]", "group-1"))
            .and(query_param("limit", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "type": "betaTesters",
                        "id": "tester-1",
                        "attributes": {
                            "firstName": "Jane",
                            "lastName": "Doe",
                            "email": "jane@example.com",
                            "inviteType": "EMAIL",
                            "state": "ACCEPTED"
                        }
                    },
                    {
                        "type": "betaTesters",
                        "id": "tester-2",
                        "attributes": {
                            "email": "bob@example.com"
                        }
                    }
                ],
                "links": {}
            })))
            .mount(&server)
            .await;

        let testers = client(server.uri())
            .fetch_beta_testers("group-1", 20)
            .await
            .unwrap();
        assert_eq!(testers.len(), 2);

        let first = &testers[0];
        assert_eq!(first.id, "tester-1");
        assert_eq!(first.first_name.as_deref(), Some("Jane"));
        assert_eq!(first.last_name.as_deref(), Some("Doe"));
        assert_eq!(first.email.as_deref(), Some("jane@example.com"));
        assert_eq!(first.invite_type.as_deref(), Some("EMAIL"));
        assert_eq!(first.state.as_deref(), Some("ACCEPTED"));

        let second = &testers[1];
        assert_eq!(second.id, "tester-2");
        assert_eq!(second.email.as_deref(), Some("bob@example.com"));
        assert!(second.first_name.is_none());
        assert!(second.last_name.is_none());
        assert!(second.invite_type.is_none());
        assert!(second.state.is_none());
    }

    #[tokio::test]
    async fn fetch_beta_testers_follows_next() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/betaTesters?cursor=PAGE2", server.uri());

        // Page 1: one tester plus the `links.next` cursor.
        Mock::given(method("GET"))
            .and(path("/v1/betaTesters"))
            .and(query_param("filter[betaGroups]", "group-1"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "betaTesters",
                    "id": "tester-1",
                    "attributes": { "email": "a@example.com" }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: the cursor encoded in `links.next` must be fetched verbatim.
        Mock::given(method("GET"))
            .and(path("/v1/betaTesters"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "betaTesters",
                    "id": "tester-2",
                    "attributes": { "email": "b@example.com" }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let testers = client(server.uri())
            .fetch_beta_testers("group-1", 50)
            .await
            .unwrap();
        assert_eq!(testers.len(), 2);
        assert_eq!(testers[0].id, "tester-1");
        assert_eq!(testers[1].id, "tester-2");
        assert_eq!(testers[1].email.as_deref(), Some("b@example.com"));
    }

    #[tokio::test]
    async fn fetch_beta_testers_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/betaTesters"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_beta_testers("group-1", 20)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn create_beta_group_posts_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/betaGroups"))
            // Assert the request carries the attributes and the app relationship,
            // without over-constraining the rest of the JSON:API envelope.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "betaGroups",
                    "attributes": {
                        "name": "External Testers",
                        "isInternalGroup": false,
                        "hasAccessToAllBuilds": true,
                        "isPublicLinkEnabled": true,
                        "isFeedbackEnabled": true
                    },
                    "relationships": {
                        "app": { "data": { "type": "apps", "id": "APP1" } }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaGroups",
                    "id": "group-1",
                    "attributes": {
                        "name": "External Testers",
                        "createdDate": "2026-03-01T12:00:00Z",
                        "isInternalGroup": false,
                        "hasAccessToAllBuilds": true,
                        "publicLinkEnabled": true,
                        "publicLink": "https://testflight.apple.com/join/ABC123",
                        "feedbackEnabled": true
                    }
                }
            })))
            .mount(&server)
            .await;

        let group = client(server.uri())
            .create_beta_group("APP1", "External Testers", false, true, true)
            .await
            .unwrap();

        assert_eq!(group.id, "group-1");
        assert_eq!(group.app_id, "APP1");
        assert_eq!(group.name.as_deref(), Some("External Testers"));
        assert_eq!(group.created_date.as_deref(), Some("2026-03-01T12:00:00Z"));
        assert_eq!(group.is_internal_group, Some(false));
        assert_eq!(group.has_access_to_all_builds, Some(true));
        assert_eq!(group.public_link_enabled, Some(true));
        assert_eq!(
            group.public_link.as_deref(),
            Some("https://testflight.apple.com/join/ABC123")
        );
        assert_eq!(group.feedback_enabled, Some(true));
    }

    #[tokio::test]
    async fn create_beta_group_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/betaGroups"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_beta_group("APP1", "Dupe", false, false, false)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn create_beta_group_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/betaGroups"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_beta_group("APP1", "Group", false, false, false)
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_beta_group_sends_only_provided_attributes() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/betaGroups/group-1"))
            // Only `name` and `publicLinkLimit` were provided; assert they are
            // present. (Absent attrs are simply omitted from the request map.)
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "betaGroups",
                    "id": "group-1",
                    "attributes": {
                        "name": "Renamed",
                        "publicLinkLimit": 250
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaGroups",
                    "id": "group-1",
                    "attributes": {
                        "name": "Renamed",
                        "publicLinkEnabled": true,
                        "feedbackEnabled": true
                    },
                    "relationships": {
                        "app": { "data": { "type": "apps", "id": "APP1" } }
                    }
                }
            })))
            .mount(&server)
            .await;

        let group = client(server.uri())
            .update_beta_group("group-1", Some("Renamed"), None, Some(250), None)
            .await
            .unwrap();

        assert_eq!(group.id, "group-1");
        // `app_id` is recovered from the PATCH response's app relationship.
        assert_eq!(group.app_id, "APP1");
        assert_eq!(group.name.as_deref(), Some("Renamed"));
        assert_eq!(group.public_link_enabled, Some(true));
        assert_eq!(group.feedback_enabled, Some(true));
    }

    #[tokio::test]
    async fn update_beta_group_app_id_empty_when_no_relationship() {
        let server = MockServer::start().await;

        // A PATCH response with no `app` relationship → `app_id` falls back to "".
        Mock::given(method("PATCH"))
            .and(path("/v1/betaGroups/group-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaGroups",
                    "id": "group-2",
                    "attributes": { "feedbackEnabled": false }
                }
            })))
            .mount(&server)
            .await;

        let group = client(server.uri())
            .update_beta_group("group-2", None, None, None, Some(false))
            .await
            .unwrap();

        assert_eq!(group.id, "group-2");
        assert_eq!(group.app_id, "");
        assert_eq!(group.feedback_enabled, Some(false));
    }

    #[tokio::test]
    async fn update_beta_group_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/betaGroups/group-1"))
            .respond_with(ResponseTemplate::new(422).set_body_string("unprocessable"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .update_beta_group("group-1", Some("x"), None, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 422, .. }));
    }

    #[tokio::test]
    async fn delete_beta_group_succeeds_on_204() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/betaGroups/group-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .delete_beta_group("group-1")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn delete_beta_group_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/betaGroups/group-1"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .delete_beta_group("group-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn add_beta_tester_posts_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/betaTesters"))
            // Assert the email + name attrs and the betaGroups linkage are carried.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "betaTesters",
                    "attributes": {
                        "firstName": "Jane",
                        "lastName": "Doe",
                        "email": "jane@example.com"
                    },
                    "relationships": {
                        "betaGroups": {
                            "data": [{ "type": "betaGroups", "id": "group-1" }]
                        }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaTesters",
                    "id": "tester-1",
                    "attributes": {
                        "firstName": "Jane",
                        "lastName": "Doe",
                        "email": "jane@example.com",
                        "inviteType": "EMAIL",
                        "state": "INVITED"
                    }
                }
            })))
            .mount(&server)
            .await;

        let tester = client(server.uri())
            .add_beta_tester("group-1", "jane@example.com", Some("Jane"), Some("Doe"))
            .await
            .unwrap();

        assert_eq!(tester.id, "tester-1");
        assert_eq!(tester.first_name.as_deref(), Some("Jane"));
        assert_eq!(tester.last_name.as_deref(), Some("Doe"));
        assert_eq!(tester.email.as_deref(), Some("jane@example.com"));
        assert_eq!(tester.invite_type.as_deref(), Some("EMAIL"));
        assert_eq!(tester.state.as_deref(), Some("INVITED"));
    }

    #[tokio::test]
    async fn add_beta_tester_omits_absent_name_parts() {
        let server = MockServer::start().await;

        // With no name parts, only `email` is sent in attributes.
        Mock::given(method("POST"))
            .and(path("/v1/betaTesters"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "betaTesters",
                    "attributes": { "email": "bob@example.com" }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaTesters",
                    "id": "tester-2",
                    "attributes": { "email": "bob@example.com" }
                }
            })))
            .mount(&server)
            .await;

        let tester = client(server.uri())
            .add_beta_tester("group-1", "bob@example.com", None, None)
            .await
            .unwrap();

        assert_eq!(tester.id, "tester-2");
        assert_eq!(tester.email.as_deref(), Some("bob@example.com"));
        assert!(tester.first_name.is_none());
        assert!(tester.last_name.is_none());
    }

    #[tokio::test]
    async fn add_beta_tester_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/betaTesters"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .add_beta_tester("group-1", "x@example.com", None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn remove_beta_tester_succeeds_on_204() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/betaGroups/group-1/relationships/betaTesters"))
            // Assert the to-many linkage body carries the tester id.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": [{ "type": "betaTesters", "id": "tester-1" }]
            })))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .remove_beta_tester("group-1", "tester-1")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn remove_beta_tester_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/betaGroups/group-1/relationships/betaTesters"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .remove_beta_tester("group-1", "tester-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn fetch_tester_count_reads_meta_paging_total() {
        let server = MockServer::start().await;

        // The request must cap the page at one item and not fetch the list; the
        // count comes from `meta.paging.total` (42) even though `data` carries a
        // single tester.
        Mock::given(method("GET"))
            .and(path("/v1/betaGroups/group-1/betaTesters"))
            .and(query_param("limit", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    { "type": "betaTesters", "id": "tester-1", "attributes": {} }
                ],
                "meta": { "paging": { "total": 42, "limit": 1 } }
            })))
            .mount(&server)
            .await;

        let count = client(server.uri())
            .fetch_tester_count("group-1")
            .await
            .unwrap();
        assert_eq!(count, 42);
    }

    #[tokio::test]
    async fn fetch_tester_count_defaults_to_zero_without_meta() {
        let server = MockServer::start().await;

        // No `meta` block at all → the count falls back to 0.
        Mock::given(method("GET"))
            .and(path("/v1/betaGroups/group-1/betaTesters"))
            .and(query_param("limit", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": []
            })))
            .mount(&server)
            .await;

        let count = client(server.uri())
            .fetch_tester_count("group-1")
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn fetch_tester_count_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/betaGroups/group-1/betaTesters"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_tester_count("group-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 500, .. }));
    }

    #[tokio::test]
    async fn resend_invite_posts_invitation() {
        let server = MockServer::start().await;

        // Assert the betaTesterInvitations POST body shape: the betaTester and
        // app relationships are both carried.
        Mock::given(method("POST"))
            .and(path("/v1/betaTesterInvitations"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "betaTesterInvitations",
                    "relationships": {
                        "betaTester": {
                            "data": { "type": "betaTesters", "id": "tester-1" }
                        },
                        "app": {
                            "data": { "type": "apps", "id": "APP1" }
                        }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .resend_invite("tester-1", "APP1")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn resend_invite_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/betaTesterInvitations"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .resend_invite("tester-1", "APP1")
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resend_invite_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/betaTesterInvitations"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .resend_invite("tester-1", "APP1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn fetch_beta_build_localizations_maps_and_paginates() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/betaBuildLocalizations?cursor=PAGE2", server.uri());

        // Page 1: a fully-populated localization, plus the `links.next` cursor.
        // The first request must carry the build filter and the limit.
        Mock::given(method("GET"))
            .and(path("/v1/betaBuildLocalizations"))
            .and(query_param("filter[build]", "build-1"))
            .and(query_param("limit", "20"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "betaBuildLocalizations",
                    "id": "loc-1",
                    "attributes": {
                        "locale": "en-US",
                        "whatsNew": "Bug fixes and improvements."
                    }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a sparse localization (only the locale present) and no further
        // page, exercising the `whatsNew`-absent path.
        Mock::given(method("GET"))
            .and(path("/v1/betaBuildLocalizations"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "betaBuildLocalizations",
                    "id": "loc-2",
                    "attributes": {
                        "locale": "pt-BR"
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let localizations = client(server.uri())
            .fetch_beta_build_localizations("build-1", 20)
            .await
            .unwrap();
        assert_eq!(localizations.len(), 2);

        let first = &localizations[0];
        assert_eq!(first.id, "loc-1");
        assert_eq!(first.locale, "en-US");
        assert_eq!(
            first.whats_new.as_deref(),
            Some("Bug fixes and improvements.")
        );

        let second = &localizations[1];
        assert_eq!(second.id, "loc-2");
        assert_eq!(second.locale, "pt-BR");
        assert!(second.whats_new.is_none());
    }

    #[tokio::test]
    async fn fetch_beta_build_localizations_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/betaBuildLocalizations"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_beta_build_localizations("build-1", 20)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn create_beta_build_localization_posts_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/betaBuildLocalizations"))
            // Assert the request carries the attributes and the build
            // relationship, without over-constraining the rest of the envelope.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "betaBuildLocalizations",
                    "attributes": {
                        "whatsNew": "First beta!",
                        "locale": "en-US"
                    },
                    "relationships": {
                        "build": { "data": { "type": "builds", "id": "build-1" } }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaBuildLocalizations",
                    "id": "loc-1",
                    "attributes": {
                        "locale": "en-US",
                        "whatsNew": "First beta!"
                    }
                }
            })))
            .mount(&server)
            .await;

        let localization = client(server.uri())
            .create_beta_build_localization("build-1", "en-US", "First beta!")
            .await
            .unwrap();

        assert_eq!(localization.id, "loc-1");
        assert_eq!(localization.locale, "en-US");
        assert_eq!(localization.whats_new.as_deref(), Some("First beta!"));
    }

    #[tokio::test]
    async fn create_beta_build_localization_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/betaBuildLocalizations"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_beta_build_localization("build-1", "en-US", "Notes")
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_beta_build_localization_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/betaBuildLocalizations"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_beta_build_localization("build-1", "en-US", "Notes")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn update_beta_build_localization_patches_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/betaBuildLocalizations/loc-1"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "betaBuildLocalizations",
                    "id": "loc-1",
                    "attributes": {
                        "whatsNew": "Updated notes."
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaBuildLocalizations",
                    "id": "loc-1",
                    "attributes": {
                        "locale": "en-US",
                        "whatsNew": "Updated notes."
                    }
                }
            })))
            .mount(&server)
            .await;

        let localization = client(server.uri())
            .update_beta_build_localization("loc-1", "Updated notes.")
            .await
            .unwrap();

        assert_eq!(localization.id, "loc-1");
        assert_eq!(localization.locale, "en-US");
        assert_eq!(localization.whats_new.as_deref(), Some("Updated notes."));
    }

    #[tokio::test]
    async fn update_beta_build_localization_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/betaBuildLocalizations/loc-1"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .update_beta_build_localization("loc-1", "Notes")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    #[tokio::test]
    async fn fetch_beta_app_localizations_maps_and_paginates() {
        let server = MockServer::start().await;
        let next = format!(
            "{}/v1/apps/app-1/betaAppLocalizations?cursor=PAGE2",
            server.uri()
        );

        // Page 1: a fully-populated localization, plus the `links.next` cursor.
        // The first request hits the app-relationship endpoint and carries the
        // limit.
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1/betaAppLocalizations"))
            .and(query_param("limit", "20"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "betaAppLocalizations",
                    "id": "loc-1",
                    "attributes": {
                        "locale": "en-US",
                        "feedbackEmail": "beta@example.com",
                        "description": "Try the new flows."
                    }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a sparse localization (only the locale present) and no further
        // page, exercising the `feedbackEmail`/`description`-absent path.
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1/betaAppLocalizations"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "betaAppLocalizations",
                    "id": "loc-2",
                    "attributes": {
                        "locale": "pt-BR"
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let localizations = client(server.uri())
            .fetch_beta_app_localizations("app-1", 20)
            .await
            .unwrap();
        assert_eq!(localizations.len(), 2);

        let first = &localizations[0];
        assert_eq!(first.id, "loc-1");
        assert_eq!(first.locale, "en-US");
        assert_eq!(first.feedback_email.as_deref(), Some("beta@example.com"));
        assert_eq!(first.description.as_deref(), Some("Try the new flows."));

        let second = &localizations[1];
        assert_eq!(second.id, "loc-2");
        assert_eq!(second.locale, "pt-BR");
        assert!(second.feedback_email.is_none());
        assert!(second.description.is_none());
    }

    #[tokio::test]
    async fn fetch_beta_app_localizations_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1/betaAppLocalizations"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_beta_app_localizations("app-1", 20)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }

    #[tokio::test]
    async fn create_beta_app_localization_posts_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/betaAppLocalizations"))
            // Assert the request always carries `locale`, includes the provided
            // optional attributes, and wires the app relationship, without
            // over-constraining the rest of the envelope.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "betaAppLocalizations",
                    "attributes": {
                        "locale": "en-US",
                        "feedbackEmail": "beta@example.com",
                        "description": "Try the new flows."
                    },
                    "relationships": {
                        "app": { "data": { "type": "apps", "id": "app-1" } }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaAppLocalizations",
                    "id": "loc-1",
                    "attributes": {
                        "locale": "en-US",
                        "feedbackEmail": "beta@example.com",
                        "description": "Try the new flows."
                    }
                }
            })))
            .mount(&server)
            .await;

        let localization = client(server.uri())
            .create_beta_app_localization(
                "app-1",
                "en-US",
                Some("beta@example.com"),
                Some("Try the new flows."),
            )
            .await
            .unwrap();

        assert_eq!(localization.id, "loc-1");
        assert_eq!(localization.locale, "en-US");
        assert_eq!(
            localization.feedback_email.as_deref(),
            Some("beta@example.com")
        );
        assert_eq!(
            localization.description.as_deref(),
            Some("Try the new flows.")
        );
    }

    #[tokio::test]
    async fn create_beta_app_localization_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/betaAppLocalizations"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_beta_app_localization("app-1", "en-US", None, None)
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_beta_app_localization_patches_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/betaAppLocalizations/loc-1"))
            // Only the provided attribute should be present; no relationships.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "betaAppLocalizations",
                    "id": "loc-1",
                    "attributes": {
                        "description": "Updated description."
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaAppLocalizations",
                    "id": "loc-1",
                    "attributes": {
                        "locale": "en-US",
                        "feedbackEmail": "beta@example.com",
                        "description": "Updated description."
                    }
                }
            })))
            .mount(&server)
            .await;

        let localization = client(server.uri())
            .update_beta_app_localization("loc-1", None, Some("Updated description."))
            .await
            .unwrap();

        assert_eq!(localization.id, "loc-1");
        assert_eq!(localization.locale, "en-US");
        assert_eq!(
            localization.feedback_email.as_deref(),
            Some("beta@example.com")
        );
        assert_eq!(
            localization.description.as_deref(),
            Some("Updated description.")
        );
    }

    #[tokio::test]
    async fn update_beta_app_localization_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/betaAppLocalizations/loc-1"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .update_beta_app_localization("loc-1", Some("beta@example.com"), None)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    #[tokio::test]
    async fn fetch_accessibility_declarations_maps_and_paginates() {
        let server = MockServer::start().await;
        let next = format!(
            "{}/v1/apps/app-1/accessibilityDeclarations?cursor=PAGE2",
            server.uri()
        );

        // Page 1: a fully-populated declaration with all nine flags true (note the
        // `supportsDifferentiateWithoutColorAlone` wire key), plus a `state` and
        // the `links.next` cursor. The first request hits the app-relationship
        // endpoint and carries the limit.
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1/accessibilityDeclarations"))
            .and(query_param("limit", "20"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "accessibilityDeclarations",
                    "id": "decl-1",
                    "attributes": {
                        "deviceFamily": "IPHONE",
                        "state": "PUBLISHED",
                        "supportsAudioDescriptions": true,
                        "supportsCaptions": true,
                        "supportsDarkInterface": true,
                        "supportsDifferentiateWithoutColorAlone": true,
                        "supportsLargerText": true,
                        "supportsReducedMotion": true,
                        "supportsSufficientContrast": true,
                        "supportsVoiceControl": true,
                        "supportsVoiceover": true
                    }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a sparse declaration (only the device family present) and no
        // further page, exercising the all-flags-absent default path.
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1/accessibilityDeclarations"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "accessibilityDeclarations",
                    "id": "decl-2",
                    "attributes": {
                        "deviceFamily": "IPAD"
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let declarations = client(server.uri())
            .fetch_accessibility_declarations("app-1", 20)
            .await
            .unwrap();
        assert_eq!(declarations.len(), 2);

        let first = &declarations[0];
        assert_eq!(first.id, "decl-1");
        assert_eq!(first.device_family, "IPHONE");
        assert_eq!(first.state.as_deref(), Some("PUBLISHED"));
        assert!(first.supports_audio_descriptions);
        assert!(first.supports_captions);
        assert!(first.supports_dark_interface);
        // The `...Alone` wire key maps onto the suffix-less host field.
        assert!(first.supports_differentiate_without_color);
        assert!(first.supports_larger_text);
        assert!(first.supports_reduced_motion);
        assert!(first.supports_sufficient_contrast);
        assert!(first.supports_voice_control);
        assert!(first.supports_voiceover);

        let second = &declarations[1];
        assert_eq!(second.id, "decl-2");
        assert_eq!(second.device_family, "IPAD");
        assert!(second.state.is_none());
        // Every missing bool defaults to false.
        assert!(!second.supports_audio_descriptions);
        assert!(!second.supports_captions);
        assert!(!second.supports_dark_interface);
        assert!(!second.supports_differentiate_without_color);
        assert!(!second.supports_larger_text);
        assert!(!second.supports_reduced_motion);
        assert!(!second.supports_sufficient_contrast);
        assert!(!second.supports_voice_control);
        assert!(!second.supports_voiceover);
    }

    #[tokio::test]
    async fn create_accessibility_declaration_posts_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/accessibilityDeclarations"))
            // Assert the request carries the `deviceFamily` attribute and wires the
            // app relationship, without over-constraining the rest of the envelope.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "accessibilityDeclarations",
                    "attributes": { "deviceFamily": "IPHONE" },
                    "relationships": {
                        "app": { "data": { "type": "apps", "id": "app-1" } }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "accessibilityDeclarations",
                    "id": "decl-1",
                    "attributes": {
                        "deviceFamily": "IPHONE",
                        "state": "DRAFT"
                    }
                }
            })))
            .mount(&server)
            .await;

        let declaration = client(server.uri())
            .create_accessibility_declaration("app-1", "IPHONE")
            .await
            .unwrap();

        assert_eq!(declaration.id, "decl-1");
        assert_eq!(declaration.device_family, "IPHONE");
        assert_eq!(declaration.state.as_deref(), Some("DRAFT"));
        assert!(!declaration.supports_voiceover);
    }

    #[tokio::test]
    async fn update_accessibility_declaration_with_publish_patches_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/accessibilityDeclarations/decl-1"))
            // With publish=true the body must carry `publish: true` plus all nine
            // supports flags, including the `...Alone` wire key.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "accessibilityDeclarations",
                    "id": "decl-1",
                    "attributes": {
                        "publish": true,
                        "supportsAudioDescriptions": true,
                        "supportsCaptions": true,
                        "supportsDarkInterface": true,
                        "supportsDifferentiateWithoutColorAlone": true,
                        "supportsLargerText": true,
                        "supportsReducedMotion": true,
                        "supportsSufficientContrast": true,
                        "supportsVoiceControl": true,
                        "supportsVoiceover": true
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "accessibilityDeclarations",
                    "id": "decl-1",
                    "attributes": {
                        "deviceFamily": "IPHONE",
                        "state": "PUBLISHED",
                        "supportsVoiceover": true
                    }
                }
            })))
            .mount(&server)
            .await;

        let declaration = client(server.uri())
            .update_accessibility_declaration(
                "decl-1", true, true, true, true, true, true, true, true, true, true,
            )
            .await
            .unwrap();

        assert_eq!(declaration.id, "decl-1");
        assert_eq!(declaration.state.as_deref(), Some("PUBLISHED"));
        assert!(declaration.supports_voiceover);
    }

    #[tokio::test]
    async fn update_accessibility_declaration_without_publish_omits_publish_key() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/accessibilityDeclarations/decl-1"))
            // The supports flags are still all present even when not publishing.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "attributes": {
                        "supportsAudioDescriptions": false,
                        "supportsVoiceover": false
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "accessibilityDeclarations",
                    "id": "decl-1",
                    "attributes": { "deviceFamily": "IPHONE", "state": "DRAFT" }
                }
            })))
            .mount(&server)
            .await;

        let declaration = client(server.uri())
            .update_accessibility_declaration(
                "decl-1", false, false, false, false, false, false, false, false, false, false,
            )
            .await
            .unwrap();
        assert_eq!(declaration.state.as_deref(), Some("DRAFT"));

        // Inspect the recorded request body to assert the `publish` key is absent
        // entirely (a partial-json matcher cannot assert absence).
        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let attributes = &body["data"]["attributes"];
        assert!(
            attributes.get("publish").is_none(),
            "publish key must be omitted when publish is false, got: {attributes}"
        );
    }

    #[tokio::test]
    async fn delete_accessibility_declaration_succeeds_on_204() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/accessibilityDeclarations/decl-1"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        client(server.uri())
            .delete_accessibility_declaration("decl-1")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn create_accessibility_declaration_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/accessibilityDeclarations"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_accessibility_declaration("app-1", "IPHONE")
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_app_info_localizations_maps_and_paginates() {
        let server = MockServer::start().await;
        let next = format!(
            "{}/v1/appInfos/info-1/appInfoLocalizations?cursor=PAGE2",
            server.uri()
        );

        // Page 1: a fully-populated localization (name, subtitle, and all three
        // privacy attributes), plus the `links.next` cursor. The first request
        // hits the appInfo-relationship endpoint.
        Mock::given(method("GET"))
            .and(path("/v1/appInfos/info-1/appInfoLocalizations"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "appInfoLocalizations",
                    "id": "aloc-1",
                    "attributes": {
                        "locale": "en-US",
                        "name": "My App",
                        "subtitle": "The best app",
                        "privacyPolicyUrl": "https://example.com/privacy",
                        "privacyChoicesUrl": "https://example.com/choices",
                        "privacyPolicyText": "We respect your privacy."
                    }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a sparse localization (only the locale present) and no further
        // page, exercising the all-attributes-absent path.
        Mock::given(method("GET"))
            .and(path("/v1/appInfos/info-1/appInfoLocalizations"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "appInfoLocalizations",
                    "id": "aloc-2",
                    "attributes": {
                        "locale": "pt-BR"
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let localizations = client(server.uri())
            .fetch_app_info_localizations("info-1")
            .await
            .unwrap();
        assert_eq!(localizations.len(), 2);

        let first = &localizations[0];
        assert_eq!(first.id, "aloc-1");
        assert_eq!(first.locale, "en-US");
        assert_eq!(first.name.as_deref(), Some("My App"));
        assert_eq!(first.subtitle.as_deref(), Some("The best app"));
        assert_eq!(
            first.privacy_policy_url.as_deref(),
            Some("https://example.com/privacy")
        );
        assert_eq!(
            first.privacy_choices_url.as_deref(),
            Some("https://example.com/choices")
        );
        assert_eq!(
            first.privacy_policy_text.as_deref(),
            Some("We respect your privacy.")
        );

        let second = &localizations[1];
        assert_eq!(second.id, "aloc-2");
        assert_eq!(second.locale, "pt-BR");
        assert!(second.name.is_none());
        assert!(second.subtitle.is_none());
        assert!(second.privacy_policy_url.is_none());
        assert!(second.privacy_choices_url.is_none());
        assert!(second.privacy_policy_text.is_none());
    }

    #[tokio::test]
    async fn fetch_app_info_localizations_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/appInfos/info-1/appInfoLocalizations"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_app_info_localizations("info-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 500, .. }));
    }

    #[tokio::test]
    async fn update_app_info_localization_patches_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/appInfoLocalizations/aloc-1"))
            // `name` is always present; `subtitle` is included only when
            // provided. No relationships.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "appInfoLocalizations",
                    "id": "aloc-1",
                    "attributes": {
                        "name": "New Name",
                        "subtitle": "New Subtitle"
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "appInfoLocalizations",
                    "id": "aloc-1",
                    "attributes": {
                        "locale": "en-US",
                        "name": "New Name",
                        "subtitle": "New Subtitle"
                    }
                }
            })))
            .mount(&server)
            .await;

        let localization = client(server.uri())
            .update_app_info_localization("aloc-1", "New Name", Some("New Subtitle"))
            .await
            .unwrap();

        assert_eq!(localization.id, "aloc-1");
        assert_eq!(localization.locale, "en-US");
        assert_eq!(localization.name.as_deref(), Some("New Name"));
        assert_eq!(localization.subtitle.as_deref(), Some("New Subtitle"));
    }

    #[tokio::test]
    async fn update_app_info_localization_privacy_sends_only_privacy_attrs() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/appInfoLocalizations/aloc-1"))
            // Only the provided privacy attributes should be present; no name /
            // subtitle and no relationships.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "appInfoLocalizations",
                    "id": "aloc-1",
                    "attributes": {
                        "privacyPolicyUrl": "https://example.com/privacy",
                        "privacyPolicyText": "Updated privacy text."
                    }
                }
            })))
            // Reject any body that leaks a `name` attribute: this asserts the
            // privacy update does not send the product-page attributes.
            .and(|req: &wiremock::Request| {
                let value: serde_json::Value = match serde_json::from_slice(&req.body) {
                    Ok(value) => value,
                    Err(_) => return false,
                };
                value
                    .get("data")
                    .and_then(|d| d.get("attributes"))
                    .and_then(|a| a.as_object())
                    .map(|attrs| {
                        !attrs.contains_key("name")
                            && !attrs.contains_key("subtitle")
                            && !attrs.contains_key("privacyChoicesUrl")
                    })
                    .unwrap_or(false)
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "appInfoLocalizations",
                    "id": "aloc-1",
                    "attributes": {
                        "locale": "en-US",
                        "name": "My App",
                        "privacyPolicyUrl": "https://example.com/privacy",
                        "privacyPolicyText": "Updated privacy text."
                    }
                }
            })))
            .mount(&server)
            .await;

        let localization = client(server.uri())
            .update_app_info_localization_privacy(
                "aloc-1",
                Some("https://example.com/privacy"),
                None,
                Some("Updated privacy text."),
            )
            .await
            .unwrap();

        assert_eq!(localization.id, "aloc-1");
        assert_eq!(
            localization.privacy_policy_url.as_deref(),
            Some("https://example.com/privacy")
        );
        assert_eq!(
            localization.privacy_policy_text.as_deref(),
            Some("Updated privacy text.")
        );
    }

    #[tokio::test]
    async fn create_app_info_localization_posts_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/appInfoLocalizations"))
            // Assert the request always carries `locale` + `name`, includes the
            // provided optional `subtitle`, and wires the appInfo relationship.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "appInfoLocalizations",
                    "attributes": {
                        "locale": "fr-FR",
                        "name": "Mon App",
                        "subtitle": "La meilleure app"
                    },
                    "relationships": {
                        "appInfo": { "data": { "type": "appInfos", "id": "info-1" } }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "appInfoLocalizations",
                    "id": "aloc-9",
                    "attributes": {
                        "locale": "fr-FR",
                        "name": "Mon App",
                        "subtitle": "La meilleure app"
                    }
                }
            })))
            .mount(&server)
            .await;

        let localization = client(server.uri())
            .create_app_info_localization("info-1", "fr-FR", "Mon App", Some("La meilleure app"))
            .await
            .unwrap();

        assert_eq!(localization.id, "aloc-9");
        assert_eq!(localization.locale, "fr-FR");
        assert_eq!(localization.name.as_deref(), Some("Mon App"));
        assert_eq!(localization.subtitle.as_deref(), Some("La meilleure app"));
    }

    #[tokio::test]
    async fn create_app_info_localization_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/appInfoLocalizations"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_app_info_localization("info-1", "en-US", "My App", None)
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_app_info_localization_succeeds_on_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/appInfoLocalizations/aloc-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        client(server.uri())
            .delete_app_info_localization("aloc-1")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delete_app_info_localization_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/appInfoLocalizations/aloc-1"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .delete_app_info_localization("aloc-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    #[tokio::test]
    async fn fetch_localizations_maps_and_paginates() {
        let server = MockServer::start().await;
        let next = format!(
            "{}/v1/appStoreVersions/ver-1/appStoreVersionLocalizations?cursor=PAGE2",
            server.uri()
        );

        // Page 1: a fully-populated localization, plus the `links.next` cursor.
        // The first request hits the version-relationship endpoint.
        Mock::given(method("GET"))
            .and(path(
                "/v1/appStoreVersions/ver-1/appStoreVersionLocalizations",
            ))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "appStoreVersionLocalizations",
                    "id": "vloc-1",
                    "attributes": {
                        "locale": "en-US",
                        "description": "An amazing app.",
                        "keywords": "amazing,app",
                        "promotionalText": "Now even better!",
                        "supportUrl": "https://example.com/support",
                        "marketingUrl": "https://example.com/marketing",
                        "whatsNew": "Bug fixes."
                    }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a sparse localization (only the locale present) and no further
        // page, exercising the all-attributes-absent path.
        Mock::given(method("GET"))
            .and(path(
                "/v1/appStoreVersions/ver-1/appStoreVersionLocalizations",
            ))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "appStoreVersionLocalizations",
                    "id": "vloc-2",
                    "attributes": {
                        "locale": "pt-BR"
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let localizations = client(server.uri())
            .fetch_localizations("ver-1")
            .await
            .unwrap();
        assert_eq!(localizations.len(), 2);

        let first = &localizations[0];
        assert_eq!(first.id, "vloc-1");
        assert_eq!(first.locale.as_deref(), Some("en-US"));
        assert_eq!(first.description.as_deref(), Some("An amazing app."));
        assert_eq!(first.keywords.as_deref(), Some("amazing,app"));
        assert_eq!(first.promotional_text.as_deref(), Some("Now even better!"));
        assert_eq!(
            first.support_url.as_deref(),
            Some("https://example.com/support")
        );
        assert_eq!(
            first.marketing_url.as_deref(),
            Some("https://example.com/marketing")
        );
        assert_eq!(first.whats_new.as_deref(), Some("Bug fixes."));

        let second = &localizations[1];
        assert_eq!(second.id, "vloc-2");
        assert_eq!(second.locale.as_deref(), Some("pt-BR"));
        assert!(second.description.is_none());
        assert!(second.keywords.is_none());
        assert!(second.promotional_text.is_none());
        assert!(second.support_url.is_none());
        assert!(second.marketing_url.is_none());
        assert!(second.whats_new.is_none());
    }

    #[tokio::test]
    async fn fetch_localizations_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/v1/appStoreVersions/ver-1/appStoreVersionLocalizations",
            ))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_localizations("ver-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 500, .. }));
    }

    #[tokio::test]
    async fn fetch_localizations_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/v1/appStoreVersions/ver-1/appStoreVersionLocalizations",
            ))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_localizations("ver-1")
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_localization_sends_only_provided_attributes() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/appStoreVersionLocalizations/vloc-1"))
            // Only the provided attributes should be present; no relationships.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "appStoreVersionLocalizations",
                    "id": "vloc-1",
                    "attributes": {
                        "description": "Updated description.",
                        "whatsNew": "New stuff."
                    }
                }
            })))
            // Reject any body that leaks an unset attribute: this asserts the
            // update sends only the `Some` attributes.
            .and(|req: &wiremock::Request| {
                let value: serde_json::Value = match serde_json::from_slice(&req.body) {
                    Ok(value) => value,
                    Err(_) => return false,
                };
                value
                    .get("data")
                    .and_then(|d| d.get("attributes"))
                    .and_then(|a| a.as_object())
                    .map(|attrs| {
                        !attrs.contains_key("keywords")
                            && !attrs.contains_key("promotionalText")
                            && !attrs.contains_key("supportUrl")
                            && !attrs.contains_key("marketingUrl")
                    })
                    .unwrap_or(false)
            })
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        client(server.uri())
            .update_localization(
                "vloc-1",
                Some("Updated description."),
                None,
                None,
                None,
                None,
                Some("New stuff."),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn update_localization_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/appStoreVersionLocalizations/vloc-1"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .update_localization("vloc-1", Some("x"), None, None, None, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn fetch_screenshot_sets_maps_sets_and_screenshots_from_included() {
        let server = MockServer::start().await;

        // A single page: one set with two screenshots resolved from `included[]`.
        // The first request must carry the `appScreenshots` include.
        Mock::given(method("GET"))
            .and(path(
                "/v1/appStoreVersionLocalizations/vloc-1/appScreenshotSets",
            ))
            .and(query_param("include", "appScreenshots"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "appScreenshotSets",
                    "id": "set-1",
                    "attributes": {
                        "screenshotDisplayType": "APP_IPHONE_67"
                    },
                    "relationships": {
                        "appScreenshots": {
                            "data": [
                                { "type": "appScreenshots", "id": "shot-1" },
                                { "type": "appScreenshots", "id": "shot-2" }
                            ]
                        }
                    }
                }],
                "included": [
                    {
                        "type": "appScreenshots",
                        "id": "shot-1",
                        "attributes": {
                            "fileName": "screen1.png",
                            "fileSize": 204800,
                            "imageAsset": {
                                "templateUrl": "https://cdn.example.com/shot/{w}x{h}.{f}",
                                "width": 1290,
                                "height": 2796
                            }
                        }
                    },
                    {
                        "type": "appScreenshots",
                        "id": "shot-2",
                        "attributes": {
                            "fileName": "screen2.png"
                        }
                    }
                ],
                "links": {}
            })))
            .mount(&server)
            .await;

        let sets = client(server.uri())
            .fetch_screenshot_sets("vloc-1")
            .await
            .unwrap();
        assert_eq!(sets.len(), 1);

        let set = &sets[0];
        assert_eq!(set.id, "set-1");
        assert_eq!(set.display_type.as_deref(), Some("APP_IPHONE_67"));
        // Screenshots resolved in relationship order.
        assert_eq!(set.screenshots.len(), 2);

        let first = &set.screenshots[0];
        assert_eq!(first.id, "shot-1");
        assert_eq!(first.file_name.as_deref(), Some("screen1.png"));
        assert_eq!(first.file_size, Some(204800));
        assert_eq!(first.width, Some(1290));
        assert_eq!(first.height, Some(2796));
        assert_eq!(
            first.image_url.as_deref(),
            Some("https://cdn.example.com/shot/1290x2796.png")
        );

        // A sparse screenshot (no image asset) → computed url absent.
        let second = &set.screenshots[1];
        assert_eq!(second.id, "shot-2");
        assert_eq!(second.file_name.as_deref(), Some("screen2.png"));
        assert!(second.file_size.is_none());
        assert!(second.width.is_none());
        assert!(second.height.is_none());
        assert!(second.image_url.is_none());
    }

    #[tokio::test]
    async fn fetch_screenshot_sets_paginates() {
        let server = MockServer::start().await;
        let next = format!(
            "{}/v1/appStoreVersionLocalizations/vloc-1/appScreenshotSets?cursor=PAGE2",
            server.uri()
        );

        Mock::given(method("GET"))
            .and(path(
                "/v1/appStoreVersionLocalizations/vloc-1/appScreenshotSets",
            ))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "appScreenshotSets",
                    "id": "set-1",
                    "attributes": { "screenshotDisplayType": "APP_IPHONE_67" },
                    "relationships": { "appScreenshots": { "data": [] } }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a set with no display type and no screenshots; no further page.
        Mock::given(method("GET"))
            .and(path(
                "/v1/appStoreVersionLocalizations/vloc-1/appScreenshotSets",
            ))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "appScreenshotSets",
                    "id": "set-2",
                    "attributes": {},
                    "relationships": {}
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let sets = client(server.uri())
            .fetch_screenshot_sets("vloc-1")
            .await
            .unwrap();
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].id, "set-1");
        assert!(sets[0].screenshots.is_empty());
        assert_eq!(sets[1].id, "set-2");
        assert!(sets[1].display_type.is_none());
        assert!(sets[1].screenshots.is_empty());
    }

    #[tokio::test]
    async fn fetch_screenshot_sets_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/v1/appStoreVersionLocalizations/vloc-1/appScreenshotSets",
            ))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_screenshot_sets("vloc-1")
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_app_info_merges_two_requests() {
        let server = MockServer::start().await;

        // Request 1: the app-info resource with category/age-rating ids in its
        // relationships, the appStoreAgeRating attribute, and `included[]`
        // carrying the localizations + the age-rating declaration.
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1/appInfos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "appInfos",
                    "id": "info-1",
                    "attributes": { "appStoreAgeRating": "FOUR_PLUS" },
                    "relationships": {
                        "primaryCategory": { "data": { "type": "appCategories", "id": "CAT_PRIMARY" } },
                        "primarySubcategoryOne": { "data": { "type": "appCategories", "id": "SUB_PRIMARY" } },
                        "secondaryCategory": { "data": { "type": "appCategories", "id": "CAT_SECONDARY" } },
                        "secondarySubcategoryOne": { "data": { "type": "appCategories", "id": "SUB_SECONDARY" } },
                        "ageRatingDeclaration": { "data": { "type": "ageRatingDeclarations", "id": "decl-1" } }
                    }
                }],
                "included": [
                    {
                        "type": "appInfoLocalizations",
                        "id": "aloc-1",
                        "attributes": { "locale": "en-US", "name": "My App", "subtitle": "Best" }
                    },
                    {
                        "type": "ageRatingDeclarations",
                        "id": "decl-1",
                        "attributes": {
                            "gamblingSimulated": "NONE",
                            "violenceRealistic": "INFREQUENT_OR_MILD",
                            "isAdvertising": true,
                            "isGambling": false,
                            "isUnrestrictedWebAccess": true,
                            "isUserGeneratedContent": false,
                            "ageRatingOverrideV2": "NONE"
                        }
                    },
                    {
                        "type": "appCategories",
                        "id": "CAT_PRIMARY",
                        "attributes": {}
                    }
                ]
            })))
            .mount(&server)
            .await;

        // Request 2: the owning app's sku / primaryLocale / contentRightsDeclaration.
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1"))
            .and(query_param(
                "fields[apps]",
                "sku,primaryLocale,contentRightsDeclaration",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "apps",
                    "id": "app-1",
                    "attributes": {
                        "sku": "SKU123",
                        "primaryLocale": "en-US",
                        "contentRightsDeclaration": "DOES_NOT_USE_THIRD_PARTY_CONTENT"
                    }
                }
            })))
            .mount(&server)
            .await;

        let detail = client(server.uri()).fetch_app_info("app-1").await.unwrap();

        assert_eq!(detail.app_info_id, "info-1");
        assert_eq!(detail.app_id, "app-1");
        assert_eq!(detail.app_store_age_rating.as_deref(), Some("FOUR_PLUS"));

        // Category ids come from the app-info RELATIONSHIPS.
        assert_eq!(detail.primary_category_id.as_deref(), Some("CAT_PRIMARY"));
        assert_eq!(
            detail.primary_subcategory_one_id.as_deref(),
            Some("SUB_PRIMARY")
        );
        assert_eq!(
            detail.secondary_category_id.as_deref(),
            Some("CAT_SECONDARY")
        );
        assert_eq!(
            detail.secondary_subcategory_one_id.as_deref(),
            Some("SUB_SECONDARY")
        );
        assert_eq!(detail.age_rating_declaration_id.as_deref(), Some("decl-1"));

        // Localizations come from `included[]`.
        assert_eq!(detail.localizations.len(), 1);
        assert_eq!(detail.localizations[0].id, "aloc-1");
        assert_eq!(detail.localizations[0].name.as_deref(), Some("My App"));

        // Age rating resolved from `included[]` by the relationship id.
        let age_rating = detail.age_rating.expect("age rating present");
        assert_eq!(age_rating.id, "decl-1");
        assert_eq!(age_rating.gambling_simulated.as_deref(), Some("NONE"));
        assert_eq!(
            age_rating.violence_realistic.as_deref(),
            Some("INFREQUENT_OR_MILD")
        );
        assert_eq!(age_rating.is_advertising, Some(true));
        assert_eq!(age_rating.is_gambling, Some(false));
        assert_eq!(age_rating.is_unrestricted_web_access, Some(true));
        assert_eq!(age_rating.is_user_generated_content, Some(false));
        assert_eq!(age_rating.age_rating_override_v2.as_deref(), Some("NONE"));

        // App attributes come from the SECOND request.
        assert_eq!(detail.sku.as_deref(), Some("SKU123"));
        assert_eq!(detail.primary_locale.as_deref(), Some("en-US"));
        assert_eq!(
            detail.content_rights_declaration.as_deref(),
            Some("DOES_NOT_USE_THIRD_PARTY_CONTENT")
        );
    }

    #[tokio::test]
    async fn fetch_app_info_errors_when_no_app_info() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1/appInfos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [], "included": []
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_app_info("app-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    #[tokio::test]
    async fn fetch_app_info_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1/appInfos"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_app_info("app-1")
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_app_categories_maps_subcategory_ids() {
        let server = MockServer::start().await;
        let next = format!(
            "{}/v1/appCategories?filter[platforms]=IOS&exists[parent]=false&include=subcategories&cursor=PAGE2",
            server.uri()
        );

        // Page 1: a top-level category with two subcategory relationships.
        Mock::given(method("GET"))
            .and(path("/v1/appCategories"))
            .and(query_param("filter[platforms]", "IOS"))
            .and(query_param("exists[parent]", "false"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "appCategories",
                    "id": "GAMES",
                    "relationships": {
                        "subcategories": {
                            "data": [
                                { "type": "appCategories", "id": "GAMES_ACTION" },
                                { "type": "appCategories", "id": "GAMES_PUZZLE" }
                            ]
                        }
                    }
                }],
                "included": [
                    { "type": "appCategories", "id": "GAMES_ACTION", "attributes": {} },
                    { "type": "appCategories", "id": "GAMES_PUZZLE", "attributes": {} }
                ],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a category with no subcategories, and no further page.
        Mock::given(method("GET"))
            .and(path("/v1/appCategories"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "appCategories",
                    "id": "UTILITIES",
                    "relationships": { "subcategories": { "data": [] } }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let categories = client(server.uri()).fetch_app_categories().await.unwrap();
        assert_eq!(categories.len(), 2);
        assert_eq!(categories[0].id, "GAMES");
        assert_eq!(
            categories[0].subcategory_ids,
            vec!["GAMES_ACTION".to_string(), "GAMES_PUZZLE".to_string()]
        );
        assert_eq!(categories[1].id, "UTILITIES");
        assert!(categories[1].subcategory_ids.is_empty());
    }

    #[tokio::test]
    async fn update_app_info_category_sends_only_some_relationships() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/appInfos/info-1"))
            // Provided relationships must be present.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "appInfos",
                    "id": "info-1",
                    "relationships": {
                        "primaryCategory": { "data": { "type": "appCategories", "id": "GAMES" } },
                        "secondaryCategory": { "data": { "type": "appCategories", "id": "UTILITIES" } }
                    }
                }
            })))
            // Omitted relationships must NOT be present (not sent as null).
            .and(|req: &wiremock::Request| {
                let value: serde_json::Value = match serde_json::from_slice(&req.body) {
                    Ok(value) => value,
                    Err(_) => return false,
                };
                value
                    .get("data")
                    .and_then(|d| d.get("relationships"))
                    .and_then(|r| r.as_object())
                    .map(|rels| {
                        !rels.contains_key("primarySubcategoryOne")
                            && !rels.contains_key("secondarySubcategoryOne")
                    })
                    .unwrap_or(false)
            })
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        client(server.uri())
            .update_app_info_category("info-1", Some("GAMES"), None, Some("UTILITIES"), None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn update_app_sends_only_some_attributes() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/apps/app-1"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "apps",
                    "id": "app-1",
                    "attributes": { "primaryLocale": "pt-BR" }
                }
            })))
            // `contentRightsDeclaration` must be absent when `None`.
            .and(|req: &wiremock::Request| {
                let value: serde_json::Value = match serde_json::from_slice(&req.body) {
                    Ok(value) => value,
                    Err(_) => return false,
                };
                value
                    .get("data")
                    .and_then(|d| d.get("attributes"))
                    .and_then(|a| a.as_object())
                    .map(|attrs| !attrs.contains_key("contentRightsDeclaration"))
                    .unwrap_or(false)
            })
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        client(server.uri())
            .update_app("app-1", None, Some("pt-BR"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn update_age_rating_sends_all_eighteen_attributes() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/ageRatingDeclarations/decl-1"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "ageRatingDeclarations",
                    "id": "decl-1",
                    "attributes": {
                        "alcoholTobaccoOrDrugUseOrReferences": "NONE",
                        "contests": "NONE",
                        "gamblingSimulated": "NONE",
                        "gunsOrOtherWeapons": "NONE",
                        "medicalOrTreatmentInformation": "NONE",
                        "profanityOrCrudeHumor": "NONE",
                        "sexualContentGraphicAndNudity": "NONE",
                        "sexualContentOrNudity": "NONE",
                        "horrorOrFearThemes": "NONE",
                        "matureOrSuggestiveThemes": "NONE",
                        "violenceCartoonOrFantasy": "NONE",
                        "violenceRealistic": "NONE",
                        "violenceRealisticProlongedGraphicOrSadistic": "NONE",
                        "isAdvertising": true,
                        "isGambling": false,
                        "isUnrestrictedWebAccess": true,
                        "isUserGeneratedContent": false,
                        "ageRatingOverrideV2": "NONE"
                    }
                }
            })))
            // Assert all 18 attribute keys are present.
            .and(|req: &wiremock::Request| {
                let value: serde_json::Value = match serde_json::from_slice(&req.body) {
                    Ok(value) => value,
                    Err(_) => return false,
                };
                value
                    .get("data")
                    .and_then(|d| d.get("attributes"))
                    .and_then(|a| a.as_object())
                    .map(|attrs| attrs.len() == 18)
                    .unwrap_or(false)
            })
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        client(server.uri())
            .update_age_rating(
                "decl-1", "NONE", "NONE", "NONE", "NONE", "NONE", "NONE", "NONE", "NONE", "NONE",
                "NONE", "NONE", "NONE", "NONE", true, false, true, false, "NONE",
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn fetch_icon_url_computes_from_latest_build() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/builds"))
            .and(query_param("filter[app]", "app-1"))
            .and(query_param("sort", "-uploadedDate"))
            .and(query_param("limit", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "builds",
                    "id": "build-1",
                    "attributes": {
                        "iconAssetToken": {
                            "templateUrl": "https://img.example.com/{w}x{h}.{f}",
                            "width": 1024,
                            "height": 1024
                        }
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let url = client(server.uri()).fetch_icon_url("app-1").await.unwrap();
        assert_eq!(
            url.as_deref(),
            Some("https://img.example.com/1024x1024.png")
        );
    }

    #[tokio::test]
    async fn fetch_icon_url_returns_none_when_no_build() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/builds"))
            .and(query_param("filter[app]", "app-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [], "links": {}
            })))
            .mount(&server)
            .await;

        let url = client(server.uri()).fetch_icon_url("app-1").await.unwrap();
        assert!(url.is_none());
    }

    #[tokio::test]
    async fn fetch_beta_app_review_detail_maps_all_fields() {
        let server = MockServer::start().await;

        // The singular app-relationship endpoint returns a single-resource
        // document with every attribute populated.
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1/betaAppReviewDetail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaAppReviewDetails",
                    "id": "detail-1",
                    "attributes": {
                        "contactFirstName": "Ada",
                        "contactLastName": "Lovelace",
                        "contactEmail": "ada@example.com",
                        "contactPhone": "+15551234567",
                        "demoAccountName": "demo-user",
                        "demoAccountPassword": "s3cr3t",
                        "isDemoAccountRequired": true,
                        "notes": "Use the demo account to reach the paywall."
                    }
                }
            })))
            .mount(&server)
            .await;

        let detail = client(server.uri())
            .fetch_beta_app_review_detail("app-1")
            .await
            .unwrap();

        assert_eq!(detail.id, "detail-1");
        assert_eq!(detail.contact_first_name.as_deref(), Some("Ada"));
        assert_eq!(detail.contact_last_name.as_deref(), Some("Lovelace"));
        assert_eq!(detail.contact_email.as_deref(), Some("ada@example.com"));
        assert_eq!(detail.contact_phone.as_deref(), Some("+15551234567"));
        assert_eq!(detail.demo_account_name.as_deref(), Some("demo-user"));
        assert_eq!(detail.demo_account_password.as_deref(), Some("s3cr3t"));
        assert_eq!(detail.is_demo_account_required, Some(true));
        assert_eq!(
            detail.notes.as_deref(),
            Some("Use the demo account to reach the paywall.")
        );
    }

    #[tokio::test]
    async fn fetch_beta_app_review_detail_handles_missing_attributes() {
        let server = MockServer::start().await;

        // A sparse detail: only `id` present, attributes object empty/absent,
        // exercising the all-`None` mapping path.
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1/betaAppReviewDetail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaAppReviewDetails",
                    "id": "detail-2"
                }
            })))
            .mount(&server)
            .await;

        let detail = client(server.uri())
            .fetch_beta_app_review_detail("app-1")
            .await
            .unwrap();

        assert_eq!(detail.id, "detail-2");
        assert!(detail.contact_first_name.is_none());
        assert!(detail.contact_last_name.is_none());
        assert!(detail.contact_email.is_none());
        assert!(detail.contact_phone.is_none());
        assert!(detail.demo_account_name.is_none());
        assert!(detail.demo_account_password.is_none());
        assert!(detail.is_demo_account_required.is_none());
        assert!(detail.notes.is_none());
    }

    #[tokio::test]
    async fn fetch_beta_app_review_detail_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps/app-1/betaAppReviewDetail"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .fetch_beta_app_review_detail("app-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    #[tokio::test]
    async fn update_beta_app_review_detail_patches_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/betaAppReviewDetails/detail-1"))
            // Only the provided attributes should be present; no relationships.
            // `isDemoAccountRequired` is sent as a bool.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "betaAppReviewDetails",
                    "id": "detail-1",
                    "attributes": {
                        "contactEmail": "ada@example.com",
                        "isDemoAccountRequired": false
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "betaAppReviewDetails",
                    "id": "detail-1",
                    "attributes": {
                        "contactFirstName": "Ada",
                        "contactLastName": "Lovelace",
                        "contactEmail": "ada@example.com",
                        "contactPhone": "+15551234567",
                        "demoAccountName": "demo-user",
                        "demoAccountPassword": "s3cr3t",
                        "isDemoAccountRequired": false,
                        "notes": "No demo account needed."
                    }
                }
            })))
            .mount(&server)
            .await;

        let detail = client(server.uri())
            .update_beta_app_review_detail(
                "detail-1",
                None,
                None,
                Some("ada@example.com"),
                None,
                None,
                None,
                Some(false),
                None,
            )
            .await
            .unwrap();

        assert_eq!(detail.id, "detail-1");
        assert_eq!(detail.contact_email.as_deref(), Some("ada@example.com"));
        assert_eq!(detail.is_demo_account_required, Some(false));
        assert_eq!(detail.notes.as_deref(), Some("No demo account needed."));
    }

    #[tokio::test]
    async fn update_beta_app_review_detail_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/betaAppReviewDetails/detail-1"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .update_beta_app_review_detail(
                "detail-1", None, None, None, None, None, None, None, None,
            )
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_beta_app_review_detail_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/betaAppReviewDetails/detail-1"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .update_beta_app_review_detail(
                "detail-1",
                Some("Ada"),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    #[tokio::test]
    async fn fetch_app_review_detail_maps_all_fields() {
        let server = MockServer::start().await;

        // The version's singular relationship endpoint returns a single-resource
        // document with every attribute populated.
        Mock::given(method("GET"))
            .and(path("/v1/appStoreVersions/ver-1/appStoreReviewDetail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "appStoreReviewDetails",
                    "id": "detail-1",
                    "attributes": {
                        "contactFirstName": "Ada",
                        "contactLastName": "Lovelace",
                        "contactEmail": "ada@example.com",
                        "contactPhone": "+15551234567",
                        "notes": "Use the demo account to reach the paywall.",
                        "demoAccountName": "demo-user",
                        "demoAccountPassword": "s3cr3t",
                        "isDemoAccountRequired": true
                    }
                }
            })))
            .mount(&server)
            .await;

        let detail = client(server.uri())
            .fetch_app_review_detail("ver-1")
            .await
            .unwrap()
            .expect("expected an app review detail");

        assert_eq!(detail.id, "detail-1");
        assert_eq!(detail.contact_first_name.as_deref(), Some("Ada"));
        assert_eq!(detail.contact_last_name.as_deref(), Some("Lovelace"));
        assert_eq!(detail.contact_email.as_deref(), Some("ada@example.com"));
        assert_eq!(detail.contact_phone.as_deref(), Some("+15551234567"));
        assert_eq!(
            detail.notes.as_deref(),
            Some("Use the demo account to reach the paywall.")
        );
        assert_eq!(detail.demo_account_name.as_deref(), Some("demo-user"));
        assert_eq!(detail.demo_account_password.as_deref(), Some("s3cr3t"));
        assert_eq!(detail.is_demo_account_required, Some(true));
    }

    #[tokio::test]
    async fn fetch_app_review_detail_returns_none_when_data_null() {
        let server = MockServer::start().await;

        // ASC may return a document with a null `data` when no app review detail
        // is attached to the version.
        Mock::given(method("GET"))
            .and(path("/v1/appStoreVersions/ver-1/appStoreReviewDetail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": null
            })))
            .mount(&server)
            .await;

        let detail = client(server.uri())
            .fetch_app_review_detail("ver-1")
            .await
            .unwrap();
        assert!(detail.is_none());
    }

    #[tokio::test]
    async fn fetch_app_review_detail_returns_none_on_404() {
        let server = MockServer::start().await;

        // A 404 (no app-review-detail relationship) resolves to `Ok(None)`.
        Mock::given(method("GET"))
            .and(path("/v1/appStoreVersions/ver-1/appStoreReviewDetail"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let detail = client(server.uri())
            .fetch_app_review_detail("ver-1")
            .await
            .unwrap();
        assert!(detail.is_none());
    }

    #[tokio::test]
    async fn update_app_review_detail_patches_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            // Note the PLURAL path resource, distinct from the singular
            // relationship segment used by the fetch.
            .and(path("/v1/appStoreReviewDetails/detail-1"))
            // Only the provided attributes should be present; no relationships.
            // `isDemoAccountRequired` is sent as a bool.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "appStoreReviewDetails",
                    "id": "detail-1",
                    "attributes": {
                        "contactEmail": "ada@example.com",
                        "notes": "No demo account needed.",
                        "isDemoAccountRequired": false
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "appStoreReviewDetails",
                    "id": "detail-1",
                    "attributes": {
                        "contactFirstName": "Ada",
                        "contactLastName": "Lovelace",
                        "contactEmail": "ada@example.com",
                        "contactPhone": "+15551234567",
                        "notes": "No demo account needed.",
                        "demoAccountName": "demo-user",
                        "demoAccountPassword": "s3cr3t",
                        "isDemoAccountRequired": false
                    }
                }
            })))
            .mount(&server)
            .await;

        let detail = client(server.uri())
            .update_app_review_detail(
                "detail-1",
                None,
                None,
                Some("ada@example.com"),
                None,
                Some("No demo account needed."),
                None,
                None,
                Some(false),
            )
            .await
            .unwrap();

        assert_eq!(detail.id, "detail-1");
        assert_eq!(detail.contact_email.as_deref(), Some("ada@example.com"));
        assert_eq!(detail.notes.as_deref(), Some("No demo account needed."));
        assert_eq!(detail.is_demo_account_required, Some(false));
    }

    #[tokio::test]
    async fn update_app_review_detail_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/appStoreReviewDetails/detail-1"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .update_app_review_detail("detail-1", None, None, None, None, None, None, None, None)
            .await
            .unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_app_review_detail_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/appStoreReviewDetails/detail-1"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .update_app_review_detail(
                "detail-1",
                Some("Ada"),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    // -----------------------------------------------------------------------
    // Users & user invitations
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_team_members_maps_basic_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/users"))
            .and(query_param(
                "fields[users]",
                "firstName,lastName,username,roles",
            ))
            .and(query_param("limit", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "users",
                    "id": "user-1",
                    "attributes": {
                        "firstName": "Ada",
                        "lastName": "Lovelace",
                        "username": "ada@example.com",
                        "roles": ["ADMIN", "DEVELOPER"]
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let members = client(server.uri()).fetch_team_members().await.unwrap();
        assert_eq!(
            members,
            vec![TeamMemberInfo {
                id: "user-1".into(),
                first_name: Some("Ada".into()),
                last_name: Some("Lovelace".into()),
                username: Some("ada@example.com".into()),
                roles: vec!["ADMIN".into(), "DEVELOPER".into()],
            }]
        );
    }

    #[tokio::test]
    async fn fetch_team_members_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/users"))
            .respond_with(ResponseTemplate::new(403).set_body_string("You have pending agreements"))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_team_members().await.unwrap_err();
        assert!(matches!(err, StackError::PendingAgreements { .. }));
    }

    #[tokio::test]
    async fn fetch_users_merges_active_and_pending() {
        let server = MockServer::start().await;

        // Active members: email comes from `username`, is_pending = false.
        Mock::given(method("GET"))
            .and(path("/v1/users"))
            .and(query_param(
                "fields[users]",
                "firstName,lastName,username,roles,allAppsVisible,provisioningAllowed",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "users",
                    "id": "user-1",
                    "attributes": {
                        "firstName": "Ada",
                        "lastName": "Lovelace",
                        "username": "ada@example.com",
                        "roles": ["ADMIN"],
                        "allAppsVisible": true,
                        "provisioningAllowed": false
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        // Pending invitations: email + expirationDate from the invitation,
        // is_pending = true.
        Mock::given(method("GET"))
            .and(path("/v1/userInvitations"))
            .and(query_param(
                "fields[userInvitations]",
                "firstName,lastName,email,roles,allAppsVisible,provisioningAllowed,expirationDate",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "userInvitations",
                    "id": "invite-1",
                    "attributes": {
                        "firstName": "Grace",
                        "lastName": "Hopper",
                        "email": "grace@example.com",
                        "roles": ["DEVELOPER"],
                        "allAppsVisible": false,
                        "provisioningAllowed": true,
                        "expirationDate": "2026-07-01T00:00:00Z"
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let users = client(server.uri()).fetch_users().await.unwrap();
        assert_eq!(users.len(), 2);
        // Active first, then pending.
        assert_eq!(
            users[0],
            UserInfo {
                id: "user-1".into(),
                first_name: Some("Ada".into()),
                last_name: Some("Lovelace".into()),
                email: Some("ada@example.com".into()),
                roles: vec!["ADMIN".into()],
                all_apps_visible: true,
                provisioning_allowed: false,
                is_pending: false,
                expiration_date: None,
            }
        );
        assert_eq!(
            users[1],
            UserInfo {
                id: "invite-1".into(),
                first_name: Some("Grace".into()),
                last_name: Some("Hopper".into()),
                email: Some("grace@example.com".into()),
                roles: vec!["DEVELOPER".into()],
                all_apps_visible: false,
                provisioning_allowed: true,
                is_pending: true,
                expiration_date: Some("2026-07-01T00:00:00Z".into()),
            }
        );
    }

    #[tokio::test]
    async fn invite_user_posts_expected_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/userInvitations"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "userInvitations",
                    "attributes": {
                        "email": "new@example.com",
                        "firstName": "New",
                        "lastName": "User",
                        "roles": ["APP_MANAGER", "DEVELOPER"],
                        "allAppsVisible": true,
                        "provisioningAllowed": false
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": { "type": "userInvitations", "id": "invite-9", "attributes": {} }
            })))
            .mount(&server)
            .await;

        let roles = vec!["APP_MANAGER".to_string(), "DEVELOPER".to_string()];
        assert!(client(server.uri())
            .invite_user("new@example.com", "New", "User", &roles, true, false)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn delete_user_active_hits_users_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/users/user-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .delete_user("user-1", false)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn delete_user_pending_hits_user_invitations_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/userInvitations/invite-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .delete_user("invite-1", true)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn delete_user_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/users/user-1"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .delete_user("user-1", false)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    // -----------------------------------------------------------------------
    // Debug logger (HTTP tracing)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn debug_logger_traces_get_request_and_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [], "links": {}
            })))
            .mount(&server)
            .await;

        let logger = Arc::new(CapturingLogger::default());
        let client = client_with_logger(server.uri(), logger.clone());
        client.fetch_apps().await.unwrap();

        let messages = logger.messages();
        // One request line and one response line per HTTP call.
        let request = messages
            .iter()
            .find(|m| m.starts_with("[RustCore] → request"))
            .expect("a request trace line");
        // Runnable cURL: method + full request URL.
        assert!(request.contains("curl -X GET"));
        assert!(request.contains(&format!("{}/v1/apps", server.uri())));
        // The Authorization bearer header is included verbatim (opt-in debug sink).
        assert!(request.contains("authorization: Bearer "));
        // A GET has no body → no `-d` line.
        assert!(!request.contains("-d '"));

        let response = messages
            .iter()
            .find(|m| m.starts_with("[RustCore] ← "))
            .expect("a response trace line");
        assert!(response.contains("200"));
    }

    #[tokio::test]
    async fn debug_logger_traces_post_body_as_pretty_json() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/customerReviewResponses"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "customerReviewResponses",
                    "id": "resp-1",
                    "attributes": { "responseBody": "Thanks!", "state": "PUBLISHED" }
                }
            })))
            .mount(&server)
            .await;

        let logger = Arc::new(CapturingLogger::default());
        let client = client_with_logger(server.uri(), logger.clone());
        client.reply_to_review("rev-1", "Thanks!").await.unwrap();

        let messages = logger.messages();
        let request = messages
            .iter()
            .find(|m| m.starts_with("[RustCore] → request"))
            .expect("a request trace line");
        assert!(request.contains("curl -X POST"));
        assert!(request.contains(&format!("{}/v1/customerReviewResponses", server.uri())));
        // The JSON body is rendered with a `-d` line and pretty-printed (the
        // pretty form spans multiple indented lines and quotes the attribute).
        assert!(request.contains("-d '"));
        assert!(request.contains("\"responseBody\": \"Thanks!\""));

        let response = messages
            .iter()
            .find(|m| m.starts_with("[RustCore] ← "))
            .expect("a response trace line");
        assert!(response.contains("201"));
        // The response body is pretty-printed JSON as well.
        assert!(response.contains("\"id\": \"resp-1\""));
    }

    #[tokio::test]
    async fn no_logger_by_default_works_without_tracing() {
        // The default `new(auth)` path (no logger) must behave exactly as before:
        // the call succeeds and nothing is logged (there is nowhere to log to).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "apps",
                    "id": "111",
                    "attributes": { "name": "Foo", "bundleId": "com.foo" }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        // `client(..)` uses `with_base_url`, whose `debug_logger` defaults to None.
        let apps = client(server.uri()).fetch_apps().await.unwrap();
        assert_eq!(apps.len(), 1);
    }

    #[test]
    fn pretty_json_falls_back_to_raw_for_non_json() {
        // Non-JSON bytes must not panic and must round-trip as lossy UTF-8.
        assert_eq!(pretty_json(b"not json"), "not json");
        // Valid JSON is re-rendered in pretty (multi-line) form.
        let pretty = pretty_json(br#"{"a":1}"#);
        assert!(pretty.contains("\"a\": 1"));
        assert!(pretty.contains('\n'));
    }

    // -----------------------------------------------------------------------
    // Devices
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_devices_maps_all_fields_and_paginates() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/devices?cursor=PAGE2", server.uri());

        // Page 1: a fully-populated device; assert the sort/limit query is sent.
        Mock::given(method("GET"))
            .and(path("/v1/devices"))
            .and(query_param("sort", "name"))
            .and(query_param("limit", "200"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "devices",
                    "id": "dev-1",
                    "attributes": {
                        "name": "Alice iPhone",
                        "udid": "00008030-ABC",
                        "platform": "IOS",
                        "deviceClass": "IPHONE",
                        "model": "iPhone 15 Pro",
                        "status": "ENABLED",
                        "addedDate": "2026-03-01T12:00:00Z"
                    }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a device with missing `name`/`status` — the fallbacks apply.
        Mock::given(method("GET"))
            .and(path("/v1/devices"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "devices",
                    "id": "dev-2",
                    "attributes": {
                        "udid": "00008030-DEF",
                        "platform": "MAC_OS"
                    }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let devices = client(server.uri()).fetch_devices().await.unwrap();
        assert_eq!(devices.len(), 2);
        assert_eq!(
            devices[0],
            DeviceInfo {
                id: "dev-1".into(),
                name: "Alice iPhone".into(),
                udid: Some("00008030-ABC".into()),
                platform: Some("IOS".into()),
                device_class: Some("IPHONE".into()),
                model: Some("iPhone 15 Pro".into()),
                status: "ENABLED".into(),
                added_date: Some("2026-03-01T12:00:00Z".into()),
            }
        );
        // The second device exercises the non-optional fallbacks.
        assert_eq!(devices[1].id, "dev-2");
        assert_eq!(devices[1].name, "");
        assert_eq!(devices[1].status, "ENABLED");
        assert_eq!(devices[1].udid.as_deref(), Some("00008030-DEF"));
        assert_eq!(devices[1].platform.as_deref(), Some("MAC_OS"));
        assert!(devices[1].device_class.is_none());
        assert!(devices[1].added_date.is_none());
    }

    #[tokio::test]
    async fn fetch_devices_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/devices"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending acceptance." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_devices().await.unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_devices_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/devices"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_devices().await.unwrap_err();
        assert!(matches!(err, StackError::Http { status: 500, .. }));
    }

    #[tokio::test]
    async fn create_device_posts_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/devices"))
            // Assert the request carries the expected name/platform/udid attributes.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "devices",
                    "attributes": {
                        "name": "Bob iPad",
                        "platform": "IOS",
                        "udid": "00008030-XYZ"
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "devices",
                    "id": "dev-new",
                    "attributes": {
                        "name": "Bob iPad",
                        "udid": "00008030-XYZ",
                        "platform": "IOS",
                        "deviceClass": "IPAD",
                        "model": "iPad Pro",
                        "status": "ENABLED",
                        "addedDate": "2026-03-02T08:00:00Z"
                    }
                }
            })))
            .mount(&server)
            .await;

        let device = client(server.uri())
            .create_device("Bob iPad", "IOS", "00008030-XYZ")
            .await
            .unwrap();

        assert_eq!(device.id, "dev-new");
        assert_eq!(device.name, "Bob iPad");
        assert_eq!(device.udid.as_deref(), Some("00008030-XYZ"));
        assert_eq!(device.platform.as_deref(), Some("IOS"));
        assert_eq!(device.device_class.as_deref(), Some("IPAD"));
        assert_eq!(device.model.as_deref(), Some("iPad Pro"));
        assert_eq!(device.status, "ENABLED");
        assert_eq!(device.added_date.as_deref(), Some("2026-03-02T08:00:00Z"));
    }

    #[tokio::test]
    async fn create_device_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/devices"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_device("Bob iPad", "IOS", "00008030-XYZ")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn update_device_sends_only_name_when_status_absent() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/devices/dev-1"))
            // Only `name` must be present; `status` must be omitted entirely.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "devices",
                    "id": "dev-1",
                    "attributes": { "name": "Renamed" }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "type": "devices", "id": "dev-1", "attributes": {} }
            })))
            .mount(&server)
            .await;

        let result = client(server.uri())
            .update_device("dev-1", Some("Renamed"), None)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn update_device_sends_only_status_when_name_absent() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/devices/dev-1"))
            // Only `status` must be present; `name` must be omitted entirely.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "devices",
                    "id": "dev-1",
                    "attributes": { "status": "DISABLED" }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "type": "devices", "id": "dev-1", "attributes": {} }
            })))
            .mount(&server)
            .await;

        let result = client(server.uri())
            .update_device("dev-1", None, Some("DISABLED"))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn update_device_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/devices/dev-1"))
            .respond_with(ResponseTemplate::new(422).set_body_string("unprocessable"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .update_device("dev-1", Some("Renamed"), None)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 422, .. }));
    }

    // -----------------------------------------------------------------------
    // Bundle IDs
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_bundle_ids_maps_all_fields_and_paginates() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/bundleIds?cursor=PAGE2", server.uri());

        // Page 1: a fully-populated bundle id; assert the sort/limit query is sent.
        Mock::given(method("GET"))
            .and(path("/v1/bundleIds"))
            .and(query_param("sort", "name"))
            .and(query_param("limit", "200"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "bundleIds",
                    "id": "bid-1",
                    "attributes": {
                        "identifier": "com.example.app",
                        "name": "Example App",
                        "platform": "IOS",
                        "seedId": "SEED123"
                    }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a bundle id with missing identifier/name/platform — the
        // empty-string fallbacks apply, and `seedId` is absent.
        Mock::given(method("GET"))
            .and(path("/v1/bundleIds"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "bundleIds",
                    "id": "bid-2",
                    "attributes": {}
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let bundle_ids = client(server.uri()).fetch_bundle_ids().await.unwrap();
        assert_eq!(bundle_ids.len(), 2);
        assert_eq!(
            bundle_ids[0],
            BundleIdInfo {
                id: "bid-1".into(),
                identifier: "com.example.app".into(),
                name: "Example App".into(),
                platform: "IOS".into(),
                seed_id: Some("SEED123".into()),
            }
        );
        // The second bundle id exercises the empty-string fallbacks + absent seed.
        assert_eq!(bundle_ids[1].id, "bid-2");
        assert_eq!(bundle_ids[1].identifier, "");
        assert_eq!(bundle_ids[1].name, "");
        assert_eq!(bundle_ids[1].platform, "");
        assert!(bundle_ids[1].seed_id.is_none());
    }

    #[tokio::test]
    async fn fetch_bundle_ids_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/bundleIds"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending acceptance." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_bundle_ids().await.unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_bundle_ids_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/bundleIds"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_bundle_ids().await.unwrap_err();
        assert!(matches!(err, StackError::Http { status: 500, .. }));
    }

    #[tokio::test]
    async fn create_bundle_id_posts_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/bundleIds"))
            // Assert the request carries name/platform/identifier attributes.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "bundleIds",
                    "attributes": {
                        "name": "Example App",
                        "platform": "IOS",
                        "identifier": "com.example.app"
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "bundleIds",
                    "id": "bid-new",
                    "attributes": {
                        "identifier": "com.example.app",
                        "name": "Example App",
                        "platform": "IOS",
                        "seedId": "SEED999"
                    }
                }
            })))
            .mount(&server)
            .await;

        let bundle_id = client(server.uri())
            .create_bundle_id("com.example.app", "Example App", "IOS")
            .await
            .unwrap();

        assert_eq!(
            bundle_id,
            BundleIdInfo {
                id: "bid-new".into(),
                identifier: "com.example.app".into(),
                name: "Example App".into(),
                platform: "IOS".into(),
                seed_id: Some("SEED999".into()),
            }
        );
    }

    #[tokio::test]
    async fn create_bundle_id_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/bundleIds"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_bundle_id("com.example.app", "Example App", "IOS")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn update_bundle_id_sends_only_name() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/v1/bundleIds/bid-1"))
            // Only `name` must be present in the attributes.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "bundleIds",
                    "id": "bid-1",
                    "attributes": { "name": "Renamed" }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "type": "bundleIds", "id": "bid-1", "attributes": {} }
            })))
            .mount(&server)
            .await;

        let result = client(server.uri())
            .update_bundle_id("bid-1", "Renamed")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn update_bundle_id_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/bundleIds/bid-1"))
            .respond_with(ResponseTemplate::new(422).set_body_string("unprocessable"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .update_bundle_id("bid-1", "Renamed")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 422, .. }));
    }

    #[tokio::test]
    async fn delete_bundle_id_hits_bundle_ids_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/bundleIds/bid-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri()).delete_bundle_id("bid-1").await.is_ok());
    }

    #[tokio::test]
    async fn delete_bundle_id_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/bundleIds/bid-1"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .delete_bundle_id("bid-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    #[tokio::test]
    async fn fetch_bundle_id_capabilities_maps_skips_empty_and_sends_no_limit() {
        let server = MockServer::start().await;
        let next = format!(
            "{}/v1/bundleIds/bid-1/bundleIdCapabilities?cursor=PAGE2",
            server.uri()
        );

        // Page 1: one valid capability + one with empty capabilityType (skipped) +
        // one with the attribute missing entirely (skipped). Assert NO `limit`
        // query param is sent — the relationship endpoint rejects it.
        Mock::given(method("GET"))
            .and(path("/v1/bundleIds/bid-1/bundleIdCapabilities"))
            .and(wiremock::matchers::query_param_is_missing("limit"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "type": "bundleIdCapabilities",
                        "id": "cap-1",
                        "attributes": { "capabilityType": "PUSH_NOTIFICATIONS" }
                    },
                    {
                        "type": "bundleIdCapabilities",
                        "id": "cap-empty",
                        "attributes": { "capabilityType": "" }
                    },
                    {
                        "type": "bundleIdCapabilities",
                        "id": "cap-missing",
                        "attributes": {}
                    }
                ],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a newer, non-enum capability type passed through verbatim.
        Mock::given(method("GET"))
            .and(path("/v1/bundleIds/bid-1/bundleIdCapabilities"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "bundleIdCapabilities",
                    "id": "cap-2",
                    "attributes": { "capabilityType": "FONT_INSTALLATION" }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let capabilities = client(server.uri())
            .fetch_bundle_id_capabilities("bid-1")
            .await
            .unwrap();

        // Only the two non-empty capabilities survive; the empty + missing are skipped.
        assert_eq!(capabilities.len(), 2);
        assert_eq!(
            capabilities[0],
            BundleIdCapabilityInfo {
                id: "cap-1".into(),
                capability_type: "PUSH_NOTIFICATIONS".into(),
            }
        );
        assert_eq!(
            capabilities[1],
            BundleIdCapabilityInfo {
                id: "cap-2".into(),
                capability_type: "FONT_INSTALLATION".into(),
            }
        );
    }

    #[tokio::test]
    async fn enable_capability_posts_and_maps() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/bundleIdCapabilities"))
            // Assert attributes.capabilityType + relationships.bundleId are sent.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "bundleIdCapabilities",
                    "attributes": { "capabilityType": "PUSH_NOTIFICATIONS" },
                    "relationships": {
                        "bundleId": {
                            "data": { "type": "bundleIds", "id": "bid-1" }
                        }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "bundleIdCapabilities",
                    "id": "cap-new",
                    "attributes": { "capabilityType": "PUSH_NOTIFICATIONS" }
                }
            })))
            .mount(&server)
            .await;

        let capability = client(server.uri())
            .enable_capability("bid-1", "PUSH_NOTIFICATIONS")
            .await
            .unwrap();

        assert_eq!(
            capability,
            BundleIdCapabilityInfo {
                id: "cap-new".into(),
                capability_type: "PUSH_NOTIFICATIONS".into(),
            }
        );
    }

    #[tokio::test]
    async fn enable_capability_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/bundleIdCapabilities"))
            .respond_with(ResponseTemplate::new(422).set_body_string("unprocessable"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .enable_capability("bid-1", "PUSH_NOTIFICATIONS")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 422, .. }));
    }

    #[tokio::test]
    async fn disable_capability_hits_bundle_id_capabilities_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/bundleIdCapabilities/cap-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .disable_capability("cap-1")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn disable_capability_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/bundleIdCapabilities/cap-1"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .disable_capability("cap-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    // -----------------------------------------------------------------------
    // Certificates
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_certificates_maps_all_fields_and_paginates() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/certificates?cursor=PAGE2", server.uri());

        // Page 1: a fully-populated certificate; assert the sort/limit query is
        // sent and that the list omits certificate content (→ None).
        Mock::given(method("GET"))
            .and(path("/v1/certificates"))
            .and(query_param("sort", "displayName"))
            .and(query_param("limit", "200"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "certificates",
                    "id": "cert-1",
                    "attributes": {
                        "displayName": "Apple Distribution",
                        "name": "Acme Inc.",
                        "certificateType": "DISTRIBUTION",
                        "platform": "IOS",
                        "serialNumber": "ABC123",
                        "expirationDate": "2027-01-01T00:00:00Z",
                        // NB: wire key is `activated`, not `isActivated`.
                        "activated": true
                    }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a certificate with missing string attributes — the empty-string
        // fallbacks apply, `activated` defaults to false, optionals stay None.
        Mock::given(method("GET"))
            .and(path("/v1/certificates"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "certificates",
                    "id": "cert-2",
                    "attributes": {}
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let certs = client(server.uri()).fetch_certificates().await.unwrap();
        assert_eq!(certs.len(), 2);
        assert_eq!(
            certs[0],
            CertificateInfo {
                id: "cert-1".into(),
                display_name: "Apple Distribution".into(),
                name: "Acme Inc.".into(),
                certificate_type: "DISTRIBUTION".into(),
                platform: Some("IOS".into()),
                serial_number: Some("ABC123".into()),
                expiration_date: Some("2027-01-01T00:00:00Z".into()),
                is_activated: true,
                // The list never includes certificate content.
                certificate_content: None,
            }
        );
        // The second certificate exercises the fallbacks + `activated` default.
        assert_eq!(certs[1].id, "cert-2");
        assert_eq!(certs[1].display_name, "");
        assert_eq!(certs[1].name, "");
        assert_eq!(certs[1].certificate_type, "");
        assert!(!certs[1].is_activated);
        assert!(certs[1].platform.is_none());
        assert!(certs[1].serial_number.is_none());
        assert!(certs[1].expiration_date.is_none());
        assert!(certs[1].certificate_content.is_none());
    }

    #[tokio::test]
    async fn fetch_certificates_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/certificates"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending acceptance." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_certificates().await.unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_certificates_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/certificates"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_certificates().await.unwrap_err();
        assert!(matches!(err, StackError::Http { status: 500, .. }));
    }

    #[tokio::test]
    async fn fetch_certificate_content_returns_content_and_sends_fields_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/certificates/cert-1"))
            // The explicit fields[certificates] selector must be sent so the
            // single-resource doc includes certificateContent.
            .and(query_param(
                "fields[certificates]",
                "certificateContent,displayName,name,certificateType,platform,serialNumber,expirationDate,activated",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "certificates",
                    "id": "cert-1",
                    "attributes": {
                        "certificateContent": "BASE64CONTENT=="
                    }
                }
            })))
            .mount(&server)
            .await;

        let content = client(server.uri())
            .fetch_certificate_content("cert-1")
            .await
            .unwrap();
        assert_eq!(content.as_deref(), Some("BASE64CONTENT=="));
    }

    #[tokio::test]
    async fn fetch_certificate_content_returns_none_when_absent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/certificates/cert-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "certificates",
                    "id": "cert-1",
                    "attributes": {}
                }
            })))
            .mount(&server)
            .await;

        let content = client(server.uri())
            .fetch_certificate_content("cert-1")
            .await
            .unwrap();
        assert!(content.is_none());
    }

    #[tokio::test]
    async fn create_certificate_with_pass_type_id_posts_relationship() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/certificates"))
            // The attributes carry the raw csrContent/certificateType, and the
            // passTypeId relationship is attached.
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "certificates",
                    "attributes": {
                        "csrContent": "CSR",
                        "certificateType": "PASS_TYPE_ID"
                    },
                    "relationships": {
                        "passTypeId": {
                            "data": { "type": "passTypeIds", "id": "pass-1" }
                        }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "certificates",
                    "id": "cert-new",
                    "attributes": {
                        "displayName": "Pass Cert",
                        "name": "Acme",
                        "certificateType": "PASS_TYPE_ID",
                        "activated": true,
                        "certificateContent": "NEWCONTENT=="
                    }
                }
            })))
            .mount(&server)
            .await;

        let cert = client(server.uri())
            .create_certificate("CSR", "PASS_TYPE_ID", Some("pass-1"), None)
            .await
            .unwrap();
        assert_eq!(cert.id, "cert-new");
        assert_eq!(cert.certificate_content.as_deref(), Some("NEWCONTENT=="));
    }

    #[tokio::test]
    async fn create_certificate_with_merchant_id_posts_relationship() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/certificates"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "certificates",
                    "attributes": {
                        "csrContent": "CSR",
                        "certificateType": "APPLE_PAY_MERCHANT_IDENTITY"
                    },
                    "relationships": {
                        "merchantId": {
                            "data": { "type": "merchantIds", "id": "merch-1" }
                        }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "certificates",
                    "id": "cert-merch",
                    "attributes": { "certificateContent": "MERCHCONTENT==" }
                }
            })))
            .mount(&server)
            .await;

        // passTypeId is None, so merchantId is used.
        let cert = client(server.uri())
            .create_certificate("CSR", "APPLE_PAY_MERCHANT_IDENTITY", None, Some("merch-1"))
            .await
            .unwrap();
        assert_eq!(cert.id, "cert-merch");
        assert_eq!(cert.certificate_content.as_deref(), Some("MERCHCONTENT=="));
    }

    #[tokio::test]
    async fn create_certificate_with_neither_posts_no_relationships_and_maps_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/certificates"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "certificates",
                    "attributes": {
                        "csrContent": "CSR",
                        "certificateType": "DISTRIBUTION"
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "certificates",
                    "id": "cert-x",
                    "attributes": {
                        "displayName": "Dist",
                        "certificateType": "DISTRIBUTION",
                        "certificateContent": "PLAINCONTENT=="
                    }
                }
            })))
            .mount(&server)
            .await;

        // Empty strings are treated as absent → no relationship attached.
        let cert = client(server.uri())
            .create_certificate("CSR", "DISTRIBUTION", Some(""), Some(""))
            .await
            .unwrap();
        assert_eq!(cert.id, "cert-x");
        assert_eq!(cert.certificate_content.as_deref(), Some("PLAINCONTENT=="));

        // Assert the request body carried NO `relationships` object at all
        // (body_partial_json cannot assert absence, so inspect the request).
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(
            body["data"].get("relationships").is_none(),
            "expected no relationships object, got: {body}"
        );
    }

    #[tokio::test]
    async fn create_certificate_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/certificates"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_certificate("CSR", "DISTRIBUTION", None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn revoke_certificate_hits_certificates_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/certificates/cert-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri())
            .revoke_certificate("cert-1")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn revoke_certificate_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/certificates/cert-1"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .revoke_certificate("cert-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    // -----------------------------------------------------------------------
    // Provisioning profiles
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_profiles_maps_all_fields_resolves_bundle_id_and_paginates() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/profiles?cursor=PAGE2", server.uri());

        // Page 1: a fully-populated profile whose bundleId relationship resolves
        // to the included bundleIds resource's identifier. Assert the
        // sort/limit/include query is sent and that the list omits content.
        Mock::given(method("GET"))
            .and(path("/v1/profiles"))
            .and(query_param("sort", "name"))
            .and(query_param("limit", "200"))
            .and(query_param("include", "bundleId"))
            .and(wiremock::matchers::query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "profiles",
                    "id": "prof-1",
                    "attributes": {
                        "name": "Acme App Store",
                        "profileType": "IOS_APP_STORE",
                        "profileState": "ACTIVE",
                        "platform": "IOS",
                        "uuid": "UUID-1",
                        "createdDate": "2026-01-01T00:00:00Z",
                        "expirationDate": "2027-01-01T00:00:00Z"
                    },
                    "relationships": {
                        "bundleId": {
                            "data": { "type": "bundleIds", "id": "bid-1" }
                        }
                    }
                }],
                "included": [{
                    "type": "bundleIds",
                    "id": "bid-1",
                    "attributes": { "identifier": "com.acme.app" }
                }],
                "links": { "next": next }
            })))
            .mount(&server)
            .await;

        // Page 2: a profile with missing string attributes — the empty-string
        // fallbacks apply, optionals stay None, content stays None. Its bundleId
        // is present in this page's included.
        Mock::given(method("GET"))
            .and(path("/v1/profiles"))
            .and(query_param("cursor", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "profiles",
                    "id": "prof-2",
                    "attributes": {},
                    "relationships": {
                        "bundleId": {
                            "data": { "type": "bundleIds", "id": "bid-2" }
                        }
                    }
                }],
                "included": [{
                    "type": "bundleIds",
                    "id": "bid-2",
                    "attributes": { "identifier": "com.acme.two" }
                }],
                "links": {}
            })))
            .mount(&server)
            .await;

        let profiles = client(server.uri()).fetch_profiles().await.unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(
            profiles[0],
            ProvisioningProfileInfo {
                id: "prof-1".into(),
                name: "Acme App Store".into(),
                profile_type: "IOS_APP_STORE".into(),
                profile_state: "ACTIVE".into(),
                platform: Some("IOS".into()),
                uuid: Some("UUID-1".into()),
                // Resolved from the included bundleIds, NOT the relationship id.
                bundle_id: Some("com.acme.app".into()),
                created_date: Some("2026-01-01T00:00:00Z".into()),
                expiration_date: Some("2027-01-01T00:00:00Z".into()),
                // The list never includes profile content.
                profile_content: None,
            }
        );
        // The second profile exercises the empty-string fallbacks + per-page
        // included resolution.
        assert_eq!(profiles[1].id, "prof-2");
        assert_eq!(profiles[1].name, "");
        assert_eq!(profiles[1].profile_type, "");
        assert_eq!(profiles[1].profile_state, "");
        assert!(profiles[1].platform.is_none());
        assert!(profiles[1].uuid.is_none());
        assert_eq!(profiles[1].bundle_id.as_deref(), Some("com.acme.two"));
        assert!(profiles[1].created_date.is_none());
        assert!(profiles[1].expiration_date.is_none());
        assert!(profiles[1].profile_content.is_none());
    }

    #[tokio::test]
    async fn fetch_profiles_leaves_bundle_id_none_when_missing_from_included() {
        let server = MockServer::start().await;
        // The profile references bid-missing, but it is not present in included[].
        Mock::given(method("GET"))
            .and(path("/v1/profiles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "type": "profiles",
                    "id": "prof-1",
                    "attributes": { "name": "Orphan" },
                    "relationships": {
                        "bundleId": {
                            "data": { "type": "bundleIds", "id": "bid-missing" }
                        }
                    }
                }],
                "included": [],
                "links": {}
            })))
            .mount(&server)
            .await;

        let profiles = client(server.uri()).fetch_profiles().await.unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Orphan");
        // Referenced bundle ID absent from included → bundle_id stays None.
        assert!(profiles[0].bundle_id.is_none());
    }

    #[tokio::test]
    async fn fetch_profiles_surfaces_pending_agreements() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/profiles"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{ "detail": "The agreement is pending acceptance." }]
            })))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_profiles().await.unwrap_err();
        match err {
            StackError::PendingAgreements { message } => {
                assert!(message.contains("pending agreements"))
            }
            other => panic!("expected PendingAgreements, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_profiles_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/profiles"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let err = client(server.uri()).fetch_profiles().await.unwrap_err();
        assert!(matches!(err, StackError::Http { status: 500, .. }));
    }

    #[tokio::test]
    async fn create_profile_with_devices_posts_expected_relationships() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/profiles"))
            // attributes carry name/profileType; bundleId to-one, certificates
            // to-many, and devices to-many (present because device_ids non-empty).
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "profiles",
                    "attributes": {
                        "name": "Dev Profile",
                        "profileType": "IOS_APP_DEVELOPMENT"
                    },
                    "relationships": {
                        "bundleId": {
                            "data": { "type": "bundleIds", "id": "bid-1" }
                        },
                        "certificates": {
                            "data": [{ "type": "certificates", "id": "cert-1" }]
                        },
                        "devices": {
                            "data": [{ "type": "devices", "id": "dev-1" }]
                        }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "profiles",
                    "id": "prof-new",
                    "attributes": {
                        "name": "Dev Profile",
                        "profileType": "IOS_APP_DEVELOPMENT",
                        "profileContent": "NEWPROFILE=="
                    }
                }
            })))
            .mount(&server)
            .await;

        let profile = client(server.uri())
            .create_profile(
                "Dev Profile",
                "IOS_APP_DEVELOPMENT",
                "bid-1",
                &["cert-1".to_string()],
                &["dev-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(profile.id, "prof-new");
        // Content is populated on create; bundle_id is not resolved → None.
        assert_eq!(profile.profile_content.as_deref(), Some("NEWPROFILE=="));
        assert!(profile.bundle_id.is_none());

        // Assert the devices relationship was actually present in the body.
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(
            body["data"]["relationships"].get("devices").is_some(),
            "expected devices relationship, got: {body}"
        );
    }

    #[tokio::test]
    async fn create_profile_with_empty_devices_omits_devices_but_keeps_certificates() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/profiles"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "data": {
                    "type": "profiles",
                    "attributes": {
                        "name": "Store Profile",
                        "profileType": "IOS_APP_STORE"
                    },
                    "relationships": {
                        "bundleId": {
                            "data": { "type": "bundleIds", "id": "bid-1" }
                        },
                        // certificates is ALWAYS sent, even with an empty array.
                        "certificates": { "data": [] }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "type": "profiles",
                    "id": "prof-store",
                    "attributes": { "profileContent": "STOREPROFILE==" }
                }
            })))
            .mount(&server)
            .await;

        let profile = client(server.uri())
            .create_profile("Store Profile", "IOS_APP_STORE", "bid-1", &[], &[])
            .await
            .unwrap();
        assert_eq!(profile.id, "prof-store");
        assert_eq!(profile.profile_content.as_deref(), Some("STOREPROFILE=="));

        // Assert devices was OMITTED entirely while certificates is present
        // (body_partial_json cannot assert absence, so inspect the request).
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let relationships = &body["data"]["relationships"];
        assert!(
            relationships.get("devices").is_none(),
            "expected no devices relationship, got: {body}"
        );
        assert!(
            relationships.get("certificates").is_some(),
            "expected certificates relationship, got: {body}"
        );
    }

    #[tokio::test]
    async fn create_profile_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/profiles"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .create_profile("P", "IOS_APP_STORE", "bid-1", &[], &[])
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 409, .. }));
    }

    #[tokio::test]
    async fn delete_profile_hits_profiles_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/profiles/prof-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        assert!(client(server.uri()).delete_profile("prof-1").await.is_ok());
    }

    #[tokio::test]
    async fn delete_profile_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/profiles/prof-1"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client(server.uri())
            .delete_profile("prof-1")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
    }

    #[tokio::test]
    async fn fetch_profile_content_returns_content_and_sends_fields_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/profiles/prof-1"))
            // The explicit fields[profiles] selector must be sent so the
            // single-resource doc includes profileContent.
            .and(query_param(
                "fields[profiles]",
                "profileContent,name,profileType,platform,profileState,uuid,createdDate,expirationDate",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "profiles",
                    "id": "prof-1",
                    "attributes": { "profileContent": "BASE64PROFILE==" }
                }
            })))
            .mount(&server)
            .await;

        let content = client(server.uri())
            .fetch_profile_content("prof-1")
            .await
            .unwrap();
        assert_eq!(content.as_deref(), Some("BASE64PROFILE=="));
    }

    #[tokio::test]
    async fn fetch_profile_content_returns_none_when_absent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/profiles/prof-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "type": "profiles",
                    "id": "prof-1",
                    "attributes": {}
                }
            })))
            .mount(&server)
            .await;

        let content = client(server.uri())
            .fetch_profile_content("prof-1")
            .await
            .unwrap();
        assert!(content.is_none());
    }
}
