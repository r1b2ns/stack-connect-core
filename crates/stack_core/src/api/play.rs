use serde::Deserialize;

use crate::auth::{GoogleAuthenticator, ServiceAccount, PLAY_SCOPES};
use crate::domain::AppInfo;
use crate::error::StackError;

const DEFAULT_BASE_URL: &str = "https://playdeveloperreporting.googleapis.com";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchPlayAppsResponse {
    #[serde(default)]
    apps: Vec<PlayReportingApp>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayReportingApp {
    /// Resource name, format `apps/{app}`. Used as `id` only when `packageName` is absent.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    package_name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

impl PlayReportingApp {
    fn into_app_info(self) -> AppInfo {
        let package = self.package_name;
        let id = package
            .clone()
            .or_else(|| self.name.clone())
            .unwrap_or_default();
        AppInfo {
            name: self
                .display_name
                .or_else(|| package.clone())
                .unwrap_or_default(),
            bundle_id: package.unwrap_or_default(),
            platform: None, // the reporting API exposes no platform field
            id,
        }
    }
}

/// Minimal Google Play Developer Reporting client (Phase 0: `apps:search` only).
pub(crate) struct PlayClient {
    base_url: String,
    http: reqwest::Client,
    auth: GoogleAuthenticator,
}

impl PlayClient {
    pub(crate) fn new(account: ServiceAccount) -> Self {
        Self::with_base_url(account, DEFAULT_BASE_URL.to_string())
    }

    pub(crate) fn with_base_url(account: ServiceAccount, base_url: String) -> Self {
        let http = reqwest::Client::new();
        let scopes = PLAY_SCOPES.iter().map(|s| s.to_string()).collect();
        let auth = GoogleAuthenticator::new(account, scopes, http.clone());
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
            auth,
        }
    }

    /// Lists every app the service account can see, following `nextPageToken`.
    pub(crate) async fn search_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        // Keep the `:search` colon literal — it is custom-method syntax, not a path
        // separator, and must NOT be percent-encoded.
        let url = format!("{}/v1beta1/apps:search", self.base_url);
        let mut apps = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let token = self.auth.access_token().await?;
            let mut request = self.http.get(&url).bearer_auth(token);
            if let Some(ref pt) = page_token {
                request = request.query(&[("pageToken", pt)]);
            }

            let response = request
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

            let page: SearchPlayAppsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("apps:search response: {e}")))?;
            apps.extend(page.apps.into_iter().map(PlayReportingApp::into_app_info));

            match page.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => break,
            }
        }

        Ok(apps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_account(token_uri: String) -> ServiceAccount {
        ServiceAccount {
            private_key_id: "kid".into(),
            private_key: include_str!("../../tests/fixtures/test_rsa_private.pem").into(),
            client_email: "svc@test.iam.gserviceaccount.com".into(),
            token_uri,
        }
    }

    async fn mock_token(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok",
                "token_type": "Bearer",
                "expires_in": 3600
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn maps_and_paginates_apps() {
        let server = MockServer::start().await;
        mock_token(&server).await;

        Mock::given(method("GET"))
            .and(path("/v1beta1/apps:search"))
            .and(query_param_is_missing("pageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [{ "name": "apps/com.foo", "packageName": "com.foo", "displayName": "Foo" }],
                "nextPageToken": "p2"
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1beta1/apps:search"))
            .and(query_param("pageToken", "p2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [{ "packageName": "com.bar", "displayName": "Bar" }]
            })))
            .mount(&server)
            .await;

        let client = PlayClient::with_base_url(
            test_account(format!("{}/token", server.uri())),
            server.uri(),
        );
        let apps = client.search_apps().await.unwrap();

        assert_eq!(apps.len(), 2);
        assert_eq!(
            apps[0],
            AppInfo {
                id: "com.foo".into(),
                name: "Foo".into(),
                bundle_id: "com.foo".into(),
                platform: None,
            }
        );
        assert_eq!(apps[1].id, "com.bar");
        assert_eq!(apps[1].bundle_id, "com.bar");
    }

    #[tokio::test]
    async fn surfaces_http_errors() {
        let server = MockServer::start().await;
        mock_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/v1beta1/apps:search"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let client = PlayClient::with_base_url(
            test_account(format!("{}/token", server.uri())),
            server.uri(),
        );
        let err = client.search_apps().await.unwrap_err();
        assert!(matches!(err, StackError::Http { status: 403, .. }));
    }
}
