use std::collections::HashSet;
use std::sync::Arc;

use serde::Deserialize;

use super::error::map_google_error;
use crate::auth::oauth_jwt::ServiceAccountAuthenticator;
use crate::domain::AppInfo;
use crate::error::StackError;
use crate::http_debug::send_and_read;
use crate::ports::DebugLogger;

const DEFAULT_BASE_URL: &str = "https://playdeveloperreporting.googleapis.com";
/// The only endpoint that lists a Play account's apps.
const APPS_SEARCH_PATH: &str = "/v1beta1/apps:search";
/// `validate` only needs to know the call is authorized.
const VALIDATE_PAGE_SIZE: u32 = 1;
/// The documented maximum; larger values are coerced to it.
const FETCH_PAGE_SIZE: u32 = 1000;
/// Hard cap on `apps:search` pages (100 000 apps), so a server that never stops
/// paginating cannot loop forever.
const MAX_PAGES: usize = 100;
/// `AppInfo::platform` for every Play app; the field that marks it as Android.
const PLATFORM_ANDROID: &str = "ANDROID";

/// One `apps:search` page. Proto3 JSON omits empty fields, so an empty result
/// is `{}` and the last page has no `nextPageToken`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchAppsResponse {
    #[serde(default)]
    apps: Vec<ReportingApp>,
    #[serde(default)]
    next_page_token: Option<String>,
}

/// A Reporting API `App` resource.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportingApp {
    /// `apps/{packageName}`.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    package_name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

impl ReportingApp {
    /// Maps to an [`AppInfo`] keyed by package name (`id == bundle_id`), falling
    /// back to `name` without its `apps/` prefix. `None` when neither yields a
    /// non-blank identifier: an empty id would collide in the host's blob store.
    /// The display name falls back to the package name.
    fn into_app_info(self) -> Option<AppInfo> {
        fn non_blank(value: Option<&str>) -> Option<&str> {
            value.map(str::trim).filter(|v| !v.is_empty())
        }

        let package = non_blank(self.package_name.as_deref())
            .or_else(|| non_blank(self.name.as_deref()?.trim().strip_prefix("apps/")))?
            .to_string();
        let name = non_blank(self.display_name.as_deref())
            .map(str::to_string)
            .unwrap_or_else(|| package.clone());

        Some(AppInfo {
            id: package.clone(),
            name,
            bundle_id: package,
            platform: Some(PLATFORM_ANDROID.to_string()),
        })
    }
}

/// Minimal Google Play client: validate credentials and list apps through the
/// Play Developer Reporting API. `base_url` is injectable so tests can point it
/// at a mock server.
pub(crate) struct GooglePlayClient {
    base_url: String,
    /// Shared with the authenticator (one connection pool, one set of timeouts).
    http: reqwest::Client,
    auth: ServiceAccountAuthenticator,
    /// Optional HTTP tracing sink. `None` by default; set via
    /// [`Self::with_debug_logger`], which also hands it to the authenticator so
    /// the token exchange is traced too.
    debug_logger: Option<Arc<dyn DebugLogger>>,
}

impl GooglePlayClient {
    pub(crate) fn new(auth: ServiceAccountAuthenticator, http: reqwest::Client) -> Self {
        Self::with_base_url(auth, http, DEFAULT_BASE_URL.to_string())
    }

    pub(crate) fn with_base_url(
        auth: ServiceAccountAuthenticator,
        http: reqwest::Client,
        base_url: String,
    ) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
            auth,
            debug_logger: None,
        }
    }

    /// Attaches an optional HTTP tracing sink to this client and its
    /// authenticator. Builder-style; `None` is a no-op (no logging).
    pub(crate) fn with_debug_logger(mut self, logger: Option<Arc<dyn DebugLogger>>) -> Self {
        self.auth = self.auth.with_debug_logger(logger.clone());
        self.debug_logger = logger;
        self
    }

    /// One `GET {base}/v1beta1/apps:search?pageSize=…[&pageToken=…]`, returning
    /// the 2xx body. The token goes through `.query()`, so it is URL-encoded.
    ///
    /// On a 401 the cached access token is dropped and the call retried once
    /// with a fresh one: Google decides token validity server-side, so a cached
    /// token can die before its `expires_in`.
    ///
    /// # Errors
    /// The authenticator's errors, [`StackError::Network`] on transport failure,
    /// or [`map_google_error`] for a non-2xx response.
    async fn search_apps(
        &self,
        page_size: u32,
        page_token: Option<&str>,
    ) -> Result<String, StackError> {
        let (mut status, mut body) = self.send_search(page_size, page_token).await?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.auth.invalidate().await;
            (status, body) = self.send_search(page_size, page_token).await?;
        }

        if status.is_success() {
            return Ok(body);
        }
        Err(map_google_error(
            status.as_u16(),
            &body,
            self.auth.client_email(),
        ))
    }

    /// Sends one `apps:search` request with the current bearer token.
    async fn send_search(
        &self,
        page_size: u32,
        page_token: Option<&str>,
    ) -> Result<(reqwest::StatusCode, String), StackError> {
        let token = self.auth.bearer_token().await?;
        let mut request = self
            .http
            .get(format!("{}{APPS_SEARCH_PATH}", self.base_url))
            .bearer_auth(token)
            .query(&[("pageSize", page_size)]);
        if let Some(page_token) = page_token {
            request = request.query(&[("pageToken", page_token)]);
        }
        send_and_read(request, self.debug_logger.as_deref()).await
    }

    /// Cheap credential check: a token exchange, then
    /// `apps:search?pageSize=1`.
    ///
    /// `Ok` means the key works and the Reporting API is enabled and answering.
    /// It does **not** mean any app is visible yet: a service account that was
    /// never invited in Play Console, or whose invite is still propagating, may
    /// get an empty `{}`.
    ///
    /// # Errors
    /// [`StackError::Auth`] when the key or the account's access is rejected,
    /// [`StackError::Http`] for other non-2xx responses, [`StackError::Decode`]
    /// on a malformed body, or [`StackError::Network`] on transport failure.
    pub(crate) async fn validate(&self) -> Result<(), StackError> {
        let body = self.search_apps(VALIDATE_PAGE_SIZE, None).await?;
        serde_json::from_str::<SearchAppsResponse>(&body)
            .map(|_| ())
            .map_err(|e| StackError::decode(format!("apps:search response: {e}")))
    }

    /// Lists every app visible to the service account, following
    /// `nextPageToken` until it is absent or blank. Apps are deduplicated by
    /// package name, keeping server order.
    ///
    /// # Errors
    /// [`StackError::Decode`] on malformed JSON, a repeated page token, or more
    /// than [`MAX_PAGES`] pages; otherwise the [`Self::validate`] mapping.
    pub(crate) async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        let mut apps = Vec::new();
        let mut seen_apps = HashSet::new();
        let mut seen_tokens = HashSet::new();
        let mut page_token: Option<String> = None;

        for _ in 0..MAX_PAGES {
            let body = self
                .search_apps(FETCH_PAGE_SIZE, page_token.as_deref())
                .await?;
            let page: SearchAppsResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("apps:search response: {e}")))?;

            for app in page
                .apps
                .into_iter()
                .filter_map(ReportingApp::into_app_info)
            {
                if seen_apps.insert(app.id.clone()) {
                    apps.push(app);
                }
            }

            match page.next_page_token.filter(|t| !t.trim().is_empty()) {
                None => return Ok(apps),
                // A token seen before would loop forever.
                Some(next) if !seen_tokens.insert(next.clone()) => {
                    return Err(StackError::decode("apps:search repeated nextPageToken"));
                }
                Some(next) => page_token = Some(next),
            }
        }

        Err(StackError::decode(format!(
            "apps:search exceeded {MAX_PAGES} pages"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth_jwt::ServiceAccountKey;
    use crate::providers::googleplay::PLAY_SCOPES;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use wiremock::matchers::{bearer_token, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    const PKCS8_PEM: &str = include_str!("../../../tests/fixtures/test_rsa_private_pkcs8.pem");
    const EMAIL: &str = "play-bot@my-project.iam.gserviceaccount.com";
    const KID: &str = "0123456789abcdef0123456789abcdef01234567";

    /// The real SERVICE_DISABLED 403 (same capture as the `error` tests).
    const SERVICE_DISABLED_BODY: &str = r#"{"error":{"code":403,"message":"Google Play Developer Reporting API has not been used in project 764216752430 before or it is disabled.","status":"PERMISSION_DENIED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"SERVICE_DISABLED","domain":"googleapis.com","metadata":{"service":"playdeveloperreporting.googleapis.com","consumer":"projects/764216752430","containerInfo":"764216752430","serviceTitle":"Google Play Developer Reporting API","activationUrl":"https://console.developers.google.com/apis/api/playdeveloperreporting.googleapis.com/overview?project=764216752430"}}]}}"#;

    /// A client whose authenticator posts to `{server}/token` and whose
    /// `apps:search` calls go to the same mock server.
    fn client(server: &MockServer) -> GooglePlayClient {
        let http = reqwest::Client::new();
        let key = ServiceAccountKey::parse(EMAIL, KID, PKCS8_PEM).unwrap();
        let auth = ServiceAccountAuthenticator::with_token_url(
            key,
            PLAY_SCOPES,
            http.clone(),
            format!("{}/token", server.uri()),
        );
        GooglePlayClient::with_base_url(auth, http, server.uri())
    }

    /// Mounts a token endpoint answering `token`, expected `times` times.
    async fn mount_token(server: &MockServer, token: &str, times: u64) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": token,
                "expires_in": 3600,
                "token_type": "Bearer"
            })))
            .expect(times)
            .mount(server)
            .await;
    }

    fn search() -> wiremock::MockBuilder {
        Mock::given(method("GET")).and(path("/v1beta1/apps:search"))
    }

    fn app(package: &str, display: &str) -> serde_json::Value {
        serde_json::json!({
            "name": format!("apps/{package}"),
            "packageName": package,
            "displayName": display
        })
    }

    fn android(package: &str, name: &str) -> AppInfo {
        AppInfo {
            id: package.into(),
            name: name.into(),
            bundle_id: package.into(),
            platform: Some("ANDROID".into()),
        }
    }

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

    /// Answers every page with a fresh `nextPageToken`, so pagination never ends
    /// on its own.
    struct EndlessPages {
        served: AtomicUsize,
    }

    impl Respond for EndlessPages {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let n = self.served.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app(&format!("com.example.app{n}"), "App")],
                "nextPageToken": format!("T{n}")
            }))
        }
    }

    // -----------------------------------------------------------------------
    // validate
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn validate_ok_with_apps() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.example.one", "One")],
                "nextPageToken": "PAGE2"
            })))
            .expect(1)
            .mount(&server)
            .await;

        client(&server).validate().await.unwrap();
    }

    #[tokio::test]
    async fn validate_ok_on_empty_object() {
        // An un-invited (or still propagating) service account may see `{}`.
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        client(&server).validate().await.unwrap();
    }

    #[tokio::test]
    async fn validate_sends_page_size_1_and_bearer() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .and(query_param("pageSize", "1"))
            .and(query_param_is_missing("pageToken"))
            .and(bearer_token("tok-1"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        client(&server).validate().await.unwrap();
    }

    #[tokio::test]
    async fn validate_malformed_json_is_decode() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let err = client(&server).validate().await.unwrap_err();
        assert!(matches!(err, StackError::Decode { .. }));
    }

    #[tokio::test]
    async fn validate_token_invalid_grant_is_auth_and_skips_search() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "Invalid JWT Signature."
            })))
            .expect(1)
            .mount(&server)
            .await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(0)
            .mount(&server)
            .await;

        match client(&server).validate().await.unwrap_err() {
            StackError::Auth { message } => assert!(message.contains("invalid_grant")),
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_401_is_auth_after_one_retry() {
        let server = MockServer::start().await;
        // The 401 drops the cached token, so the retry exchanges a new one.
        mount_token(&server, "tok-1", 2).await;
        search()
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {
                    "code": 401,
                    "message": "Request had invalid authentication credentials.",
                    "status": "UNAUTHENTICATED"
                }
            })))
            .expect(2)
            .mount(&server)
            .await;

        match client(&server).validate().await.unwrap_err() {
            StackError::Auth { message } => assert!(message.contains("HTTP 401")),
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_401_invalidates_token_and_retries_once() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok-1", "expires_in": 3600
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok-2", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        // The cached token was revoked server-side…
        search()
            .and(bearer_token("tok-1"))
            .respond_with(ResponseTemplate::new(401).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;
        // …so the retry must carry a freshly exchanged one.
        search()
            .and(bearer_token("tok-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.example.one", "One")]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let apps = client(&server).fetch_apps().await.unwrap();
        assert_eq!(apps, vec![android("com.example.one", "One")]);
    }

    #[tokio::test]
    async fn validate_service_disabled_is_auth_with_url() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(403).set_body_string(SERVICE_DISABLED_BODY))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server).validate().await.unwrap_err() {
            StackError::Auth { message } => {
                assert!(message.starts_with("Google Play Developer Reporting API is not enabled"));
                assert!(message.contains(
                    "Enable it at https://console.developers.google.com/apis/api/\
                     playdeveloperreporting.googleapis.com/overview?project=764216752430"
                ));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_permission_denied_is_auth_with_invite_hint() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": {
                    "code": 403,
                    "message": "The caller does not have permission",
                    "status": "PERMISSION_DENIED"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server).validate().await.unwrap_err() {
            StackError::Auth { message } => {
                assert!(message.contains(EMAIL));
                assert!(message.contains("Users and permissions"));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_429_is_http() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "error": { "code": 429, "message": "Quota exceeded", "status": "RESOURCE_EXHAUSTED" }
            })))
            .mount(&server)
            .await;

        match client(&server).validate().await.unwrap_err() {
            StackError::Http { status, message } => {
                assert_eq!(status, 429);
                assert_eq!(message, "Quota exceeded [RESOURCE_EXHAUSTED]");
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_503_is_http() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .mount(&server)
            .await;

        let err = client(&server).validate().await.unwrap_err();
        assert!(matches!(err, StackError::Http { status: 503, .. }));
    }

    // -----------------------------------------------------------------------
    // fetch_apps
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_apps_maps_and_paginates() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .and(query_param("pageSize", "1000"))
            .and(query_param_is_missing("pageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.example.one", "One")],
                "nextPageToken": "PAGE2"
            })))
            .expect(1)
            .mount(&server)
            .await;
        search()
            .and(query_param("pageSize", "1000"))
            .and(query_param("pageToken", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.example.two", "Two")]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let apps = client(&server).fetch_apps().await.unwrap();
        assert_eq!(
            apps,
            vec![
                android("com.example.one", "One"),
                android("com.example.two", "Two")
            ]
        );
    }

    #[tokio::test]
    async fn fetch_apps_url_encodes_page_token() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .and(query_param_is_missing("pageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.example.one", "One")],
                "nextPageToken": "a+b/c="
            })))
            .mount(&server)
            .await;
        // `query_param` decodes the query: an unencoded `+` would arrive as a
        // space and miss this mock.
        search()
            .and(query_param("pageToken", "a+b/c="))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        let apps = client(&server).fetch_apps().await.unwrap();
        assert_eq!(apps.len(), 1);

        let requests = server.received_requests().await.unwrap();
        let second_page = requests
            .iter()
            .filter(|r| r.url.path() == "/v1beta1/apps:search")
            .nth(1)
            .expect("a second page request");
        assert!(second_page
            .url
            .query()
            .unwrap()
            .contains("pageToken=a%2Bb%2Fc%3D"));
    }

    #[tokio::test]
    async fn fetch_apps_stops_on_empty_next_page_token() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.example.one", "One")],
                "nextPageToken": ""
            })))
            .expect(1)
            .mount(&server)
            .await;

        let apps = client(&server).fetch_apps().await.unwrap();
        assert_eq!(apps, vec![android("com.example.one", "One")]);
    }

    #[tokio::test]
    async fn fetch_apps_empty_object_is_empty_vec() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client(&server).fetch_apps().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn fetch_apps_falls_back_to_name_without_package_name() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [
                    { "name": "apps/com.example.named", "displayName": "Named" },
                    { "name": "apps/com.example.blankpkg", "packageName": "  ", "displayName": "Blank" }
                ]
            })))
            .mount(&server)
            .await;

        let apps = client(&server).fetch_apps().await.unwrap();
        assert_eq!(
            apps,
            vec![
                android("com.example.named", "Named"),
                android("com.example.blankpkg", "Blank")
            ]
        );
    }

    #[tokio::test]
    async fn fetch_apps_skips_apps_without_identifier() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [
                    { "displayName": "No ids" },
                    { "name": "", "packageName": "", "displayName": "Blank ids" },
                    { "name": "apps/  ", "displayName": "Blank after prefix" },
                    { "name": "com.example.noprefix", "displayName": "No prefix" },
                    app("com.example.kept", "Kept")
                ]
            })))
            .mount(&server)
            .await;

        let apps = client(&server).fetch_apps().await.unwrap();
        assert_eq!(apps, vec![android("com.example.kept", "Kept")]);
    }

    #[tokio::test]
    async fn fetch_apps_name_falls_back_to_package_when_display_blank() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [
                    { "packageName": "com.example.blank", "displayName": "   " },
                    { "packageName": "com.example.missing" },
                    { "packageName": "com.example.trimmed", "displayName": "  Trimmed  " }
                ]
            })))
            .mount(&server)
            .await;

        let apps = client(&server).fetch_apps().await.unwrap();
        assert_eq!(
            apps,
            vec![
                android("com.example.blank", "com.example.blank"),
                android("com.example.missing", "com.example.missing"),
                android("com.example.trimmed", "Trimmed")
            ]
        );
    }

    #[tokio::test]
    async fn fetch_apps_dedupes_preserving_order() {
        // A service account in several developer accounts can see an app twice.
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .and(query_param_is_missing("pageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.b", "B"), app("com.a", "A"), app("com.b", "B again")],
                "nextPageToken": "PAGE2"
            })))
            .mount(&server)
            .await;
        search()
            .and(query_param("pageToken", "PAGE2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.a", "A again"), app("com.c", "C")]
            })))
            .mount(&server)
            .await;

        let apps = client(&server).fetch_apps().await.unwrap();
        assert_eq!(
            apps,
            vec![
                android("com.b", "B"),
                android("com.a", "A"),
                android("com.c", "C")
            ]
        );
    }

    #[tokio::test]
    async fn fetch_apps_sets_platform_android() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.example.one", "One"), app("com.example.two", "Two")]
            })))
            .mount(&server)
            .await;

        let apps = client(&server).fetch_apps().await.unwrap();
        assert_eq!(apps.len(), 2);
        assert!(apps
            .iter()
            .all(|a| a.platform.as_deref() == Some("ANDROID")));
    }

    #[tokio::test]
    async fn fetch_apps_repeated_token_is_decode() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .and(query_param_is_missing("pageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [], "nextPageToken": "T1"
            })))
            .expect(1)
            .mount(&server)
            .await;
        search()
            .and(query_param("pageToken", "T1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [], "nextPageToken": "T1"
            })))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server).fetch_apps().await.unwrap_err() {
            StackError::Decode { message } => assert!(message.contains("repeated nextPageToken")),
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_apps_stops_after_max_pages() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        // Literal, not `MAX_PAGES`, so a changed cap fails here.
        search()
            .respond_with(EndlessPages {
                served: AtomicUsize::new(0),
            })
            .expect(100)
            .mount(&server)
            .await;

        match client(&server).fetch_apps().await.unwrap_err() {
            StackError::Decode { message } => assert!(message.contains("exceeded 100 pages")),
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_apps_malformed_json_is_decode() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"apps\": 42}"))
            .mount(&server)
            .await;

        let err = client(&server).fetch_apps().await.unwrap_err();
        assert!(matches!(err, StackError::Decode { .. }));
    }

    #[tokio::test]
    async fn fetch_apps_500_is_http() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        match client(&server).fetch_apps().await.unwrap_err() {
            StackError::Http { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(message, "boom");
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_apps_service_disabled_is_auth() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(403).set_body_string(SERVICE_DISABLED_BODY))
            .mount(&server)
            .await;

        match client(&server).fetch_apps().await.unwrap_err() {
            StackError::Auth { message } => assert!(message.contains("is not enabled")),
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_apps_reuses_token_across_pages() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .and(bearer_token("tok-1"))
            .and(query_param_is_missing("pageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.example.one", "One")], "nextPageToken": "P2"
            })))
            .mount(&server)
            .await;
        search()
            .and(bearer_token("tok-1"))
            .and(query_param("pageToken", "P2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.example.two", "Two")], "nextPageToken": "P3"
            })))
            .mount(&server)
            .await;
        search()
            .and(bearer_token("tok-1"))
            .and(query_param("pageToken", "P3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let apps = client(&server).fetch_apps().await.unwrap();
        assert_eq!(apps.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Debug logger (HTTP tracing)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn debug_logger_traces_token_then_search() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let logger = Arc::new(CapturingLogger::default());
        let client = client(&server).with_debug_logger(Some(logger.clone()));
        client.validate().await.unwrap();

        // The logger reached the authenticator too: four lines, in order.
        let messages = logger.messages();
        assert_eq!(messages.len(), 4);
        assert!(messages[0].starts_with("[RustCore] → request"));
        assert!(messages[0].contains(&format!("curl -X POST '{}/token'", server.uri())));
        assert!(messages[0].contains("-d 'grant_type="));
        assert!(messages[1].starts_with("[RustCore] ← 200"));
        assert!(messages[1].contains("\"access_token\": \"tok-1\""));
        assert!(messages[2].starts_with("[RustCore] → request"));
        assert!(messages[2].contains(&format!(
            "curl -X GET '{}/v1beta1/apps:search?pageSize=1'",
            server.uri()
        )));
        assert!(messages[2].contains("authorization: Bearer tok-1"));
        assert!(messages[3].starts_with("[RustCore] ← 200"));
    }

    #[tokio::test]
    async fn no_logger_by_default_works_without_tracing() {
        // `client(..)` uses `with_base_url`, whose `debug_logger` defaults to
        // None (the authenticator's too): the calls succeed with nowhere to log.
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [app("com.example.one", "One")]
            })))
            .mount(&server)
            .await;

        let client = client(&server);
        assert!(client.debug_logger.is_none());
        assert_eq!(client.fetch_apps().await.unwrap().len(), 1);
    }

    #[test]
    fn new_targets_reporting_api() {
        let http = reqwest::Client::new();
        let key = ServiceAccountKey::parse(EMAIL, KID, PKCS8_PEM).unwrap();
        let auth = ServiceAccountAuthenticator::new(key, PLAY_SCOPES, http.clone());
        let client = GooglePlayClient::new(auth, http);
        assert_eq!(
            client.base_url,
            "https://playdeveloperreporting.googleapis.com"
        );
    }

    #[test]
    fn with_base_url_trims_trailing_slash() {
        let http = reqwest::Client::new();
        let key = ServiceAccountKey::parse(EMAIL, KID, PKCS8_PEM).unwrap();
        let auth = ServiceAccountAuthenticator::new(key, PLAY_SCOPES, http.clone());
        let client = GooglePlayClient::with_base_url(auth, http, "http://mock.test/".into());
        assert_eq!(client.base_url, "http://mock.test");
    }
}
