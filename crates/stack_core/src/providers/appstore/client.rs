use std::collections::HashMap;

use serde::Deserialize;
use serde_json::json;

use crate::auth::es256::AppStoreAuthenticator;
use crate::domain::{AppInfo, CustomerReview, ReviewResponse, ReviewSubmission};
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
        Err(StackError::Http {
            status: status.as_u16(),
            message: body,
        })
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
            return Err(StackError::Http {
                status: status.as_u16(),
                message: body,
            });
        }
        Ok(body)
    }
}

/// Maps a non-success `validate` response. A 403 whose body mentions pending
/// agreements gets a clear, actionable message.
fn map_error_response(status: u16, body: &str) -> StackError {
    if status == 403 {
        let lowered = body.to_lowercase();
        if lowered.contains("agreement") || lowered.contains("pending") {
            return StackError::auth(
                "App Store Connect has pending agreements; accept them in the \
                 developer portal before connecting",
            );
        }
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
            StackError::Auth { message } => assert!(message.contains("pending agreements")),
            other => panic!("expected Auth, got {other:?}"),
        }
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
}
