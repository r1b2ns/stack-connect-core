use serde::Deserialize;

use crate::auth::es256::AppStoreAuthenticator;
use crate::domain::AppInfo;
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
            let token = self.auth.bearer_token().await?;
            let response = self
                .http
                .get(&url)
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

            let page: AppsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("apps response: {e}")))?;
            apps.extend(page.data.into_iter().map(AppResource::into_app_info));

            // `links.next` is an absolute URL; follow it verbatim until absent.
            next_url = page.links.next.filter(|u| !u.is_empty());
        }

        Ok(apps)
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
}
