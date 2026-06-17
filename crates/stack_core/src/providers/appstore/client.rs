use std::collections::HashMap;

use serde::Deserialize;
use serde_json::json;

use crate::auth::es256::AppStoreAuthenticator;
use crate::domain::{
    AppInfo, AppStoreVersionInfo, BetaBuildLocalizationInfo, BetaGroupInfo, BetaTesterInfo,
    BuildInfo, CustomerReview, CustomerReviewsPage, ReviewResponse, ReviewSubmission,
};
use crate::error::StackError;

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

// ---------------------------------------------------------------------------
// Builds (JSON:API)
// ---------------------------------------------------------------------------

/// A JSON:API document page of `builds` resources.
#[derive(Deserialize)]
struct BuildsResponse {
    #[serde(default)]
    data: Vec<BuildResource>,
    #[serde(default)]
    links: Links,
}

#[derive(Deserialize)]
struct BuildResource {
    id: String,
    #[serde(default)]
    attributes: BuildAttributes,
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
}

impl BuildResource {
    fn into_build_info(self, app_id: &str) -> BuildInfo {
        BuildInfo {
            id: self.id,
            app_id: app_id.to_string(),
            version: self.attributes.version,
            uploaded_date: self.attributes.uploaded_date,
            expired: self.attributes.expired,
            processing_state: self.attributes.processing_state,
            min_os_version: self.attributes.min_os_version,
            expiration_date: self.attributes.expiration_date,
        }
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
}

/// Minimal App Store Connect client: validate credentials and list apps.
/// `base_url` is injectable so tests can point it at a mock server.
pub(crate) struct AppStoreClient {
    base_url: String,
    http: reqwest::Client,
    auth: AppStoreAuthenticator,
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
        }
    }

    /// Cheap credential check: `GET /v1/apps?limit=1`.
    ///
    /// # Errors
    /// [`StackError::Auth`] on rejection — a 403 mentioning pending agreements is
    /// surfaced with an explanatory message; otherwise the raw status/body.
    pub(crate) async fn validate(&self) -> Result<(), StackError> {
        let url = format!("{}/v1/apps?limit=1", self.base_url);
        let token = self.auth.bearer_token().await?;
        let response = self
            .http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        let body = response.text().await.unwrap_or_default();
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

        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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
        let response = self
            .http
            .delete(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        let body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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

        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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

        let response = self
            .http
            .patch(&url)
            .bearer_auth(token)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        let body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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
        let response = self
            .http
            .delete(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        let body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
        if let Some(err) = pending_agreements_error(status.as_u16(), &body) {
            return Err(err);
        }
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
    }

    /// Lists the builds for `app_id`, newest first (by upload date), mapping each
    /// into a [`BuildInfo`] with `app_id` set from the parameter.
    ///
    /// `GET /v1/builds?filter[app]={app_id}&sort=-uploadedDate&limit={limit}`,
    /// following `links.next` pagination until exhausted.
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
            "{}/v1/builds?filter[app]={app_id}&sort=-uploadedDate&limit={limit}",
            self.base_url
        ));

        while let Some(url) = next_url {
            let body = self.get_page(&url).await?;
            let page: BuildsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("builds response: {e}")))?;
            builds.extend(page.data.into_iter().map(|b| b.into_build_info(app_id)));

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(builds)
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

        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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

        let response = self
            .http
            .patch(&url)
            .bearer_auth(token)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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
        let response = self
            .http
            .delete(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        let body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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

        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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

        let response = self
            .http
            .delete(&url)
            .bearer_auth(token)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        let body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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
            let page: BetaBuildLocalizationsResponse = serde_json::from_str(&body).map_err(|e| {
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

        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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

        let response = self
            .http
            .patch(&url)
            .bearer_auth(token)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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

    /// Authenticated `GET` of one JSON:API page, returning the raw body or mapping
    /// the failure: non-2xx → [`StackError::Http`], transport → [`StackError::Network`].
    async fn get_page(&self, url: &str) -> Result<String, StackError> {
        let token = self.auth.bearer_token().await?;
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
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
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PRIVATE_P8: &[u8] = include_bytes!("../../../tests/fixtures/test_ec_private.p8");

    fn client(base_url: String) -> AppStoreClient {
        let auth = AppStoreAuthenticator::new("issuer".into(), "kid".into(), PRIVATE_P8.to_vec());
        AppStoreClient::with_base_url(auth, base_url)
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
    async fn fetch_builds_maps_and_paginates() {
        let server = MockServer::start().await;
        let next = format!("{}/v1/builds?cursor=PAGE2", server.uri());

        // Page 1: a fully-populated build, plus the `links.next` cursor. The first
        // request must carry the app filter, the newest-first sort, and the limit.
        Mock::given(method("GET"))
            .and(path("/v1/builds"))
            .and(query_param("filter[app]", "APP1"))
            .and(query_param("sort", "-uploadedDate"))
            .and(query_param("limit", "20"))
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
                        "expirationDate": "2026-06-01T12:00:00Z"
                    }
                }],
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
        assert_eq!(first.whats_new.as_deref(), Some("Bug fixes and improvements."));

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
}
