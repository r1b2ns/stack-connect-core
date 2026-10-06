use std::collections::HashSet;
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::Mutex;

use super::error::{map_google_error, ErrorHints};
use crate::auth::oauth_jwt::ServiceAccountAuthenticator;
use crate::domain::AppInfo;
use crate::error::StackError;
use crate::http_debug::send_and_read;
use crate::ports::DebugLogger;

/// Play Developer Reporting API: lists apps (`apps:search`).
const DEFAULT_REPORTING_BASE_URL: &str = "https://playdeveloperreporting.googleapis.com";
/// Google Play Android Developer API (androidpublisher): reviews and edits.
const DEFAULT_PUBLISHER_BASE_URL: &str = "https://androidpublisher.googleapis.com";
/// Path segments from the Android Publisher host to the per-app resources.
const PUBLISHER_APPLICATIONS_PATH: [&str; 3] = ["androidpublisher", "v3", "applications"];
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

/// Whether `name` is a valid Android application id (package name): at least
/// two `.`-separated segments, each starting with an ASCII letter and made only
/// of ASCII letters, digits and `_`. These are the rules Android itself applies
/// (`PackageParser.validateName`), so no Play app can have any other name.
pub(super) fn is_valid_package_name(name: &str) -> bool {
    let mut segments = 0;
    for segment in name.split('.') {
        let mut chars = segment.chars();
        if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
            return false;
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

/// Rejects a package name no Play app can have, before any request is sent.
///
/// Reported as [`StackError::Http`] `404` — the answer Google gives for a
/// package it does not know — so hosts handle "no such app" in one place; the
/// message says the check was local. It also keeps URL building safe: `""`,
/// `.`, `..` or a `/` can never reach a path segment.
fn ensure_package_name(name: &str) -> Result<(), StackError> {
    if is_valid_package_name(name) {
        return Ok(());
    }
    Err(StackError::Http {
        status: 404,
        message: format!(
            "{name:?} is not a valid Android package name, so no Google Play app can \
             have it (checked locally; nothing was sent to Google)."
        ),
    })
}

/// Google Play client: the shared transport (bearer token, one retry on 401,
/// debug tracing, error mapping) plus the Reporting API calls that validate
/// credentials and list apps. Reviews and edit-based reads live in sibling
/// modules (`reviews`, `edits`) as further `impl` blocks over the `pub(super)`
/// helpers here.
///
/// Both base URLs are injectable so tests can point them at a mock server.
pub(crate) struct GooglePlayClient {
    /// Play Developer Reporting API base (`apps:search`).
    reporting_base_url: String,
    /// Android Publisher API base (reviews, edits).
    publisher_base_url: String,
    /// Shared with the authenticator (one connection pool, one set of timeouts).
    http: reqwest::Client,
    auth: ServiceAccountAuthenticator,
    /// Optional HTTP tracing sink. `None` by default; set via
    /// [`Self::with_debug_logger`], which also hands it to the authenticator so
    /// the token exchange is traced too.
    debug_logger: Option<Arc<dyn DebugLogger>>,
    /// Serializes edit sessions: Google keeps one open edit per service account
    /// and app, and opening one invalidates the previous one, so two
    /// concurrent edit-based reads through this client would break each other.
    edit_lock: Mutex<()>,
}

impl GooglePlayClient {
    pub(crate) fn new(auth: ServiceAccountAuthenticator, http: reqwest::Client) -> Self {
        Self::with_base_urls(
            auth,
            http,
            DEFAULT_REPORTING_BASE_URL.to_string(),
            DEFAULT_PUBLISHER_BASE_URL.to_string(),
        )
    }

    pub(crate) fn with_base_urls(
        auth: ServiceAccountAuthenticator,
        http: reqwest::Client,
        reporting_base_url: String,
        publisher_base_url: String,
    ) -> Self {
        Self {
            reporting_base_url: reporting_base_url.trim_end_matches('/').to_string(),
            publisher_base_url: publisher_base_url.trim_end_matches('/').to_string(),
            http,
            auth,
            debug_logger: None,
            edit_lock: Mutex::new(()),
        }
    }

    /// Attaches an optional HTTP tracing sink to this client and its
    /// authenticator. Builder-style; `None` is a no-op (no logging).
    pub(crate) fn with_debug_logger(mut self, logger: Option<Arc<dyn DebugLogger>>) -> Self {
        self.auth = self.auth.with_debug_logger(logger.clone());
        self.debug_logger = logger;
        self
    }

    /// The shared HTTP client, for building requests passed to
    /// [`Self::execute`].
    pub(super) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The lock every edit session holds from insert to delete.
    pub(super) fn edit_lock(&self) -> &Mutex<()> {
        &self.edit_lock
    }

    /// Writes `message()` to the debug logger, if one is attached. Lazy, so
    /// nothing is formatted without a logger.
    pub(super) fn trace(&self, message: impl FnOnce() -> String) {
        if let Some(logger) = &self.debug_logger {
            logger.log(message());
        }
    }

    /// `{publisher}/androidpublisher/v3/applications/{package_name}/{segments…}`.
    /// Every segment is percent-encoded on its own, so a `/`, `?` or `#` inside
    /// one cannot change the path.
    ///
    /// # Errors
    /// [`StackError::Http`] `404` for an invalid package name (see
    /// [`is_valid_package_name`]), [`StackError::Decode`] for a blank, `.` or
    /// `..` segment (only an id Google returned can produce one), or
    /// [`StackError::Network`] if the base URL is not a valid URL.
    pub(super) fn publisher_url(
        &self,
        package_name: &str,
        segments: &[&str],
    ) -> Result<reqwest::Url, StackError> {
        ensure_package_name(package_name)?;
        // The URL parser would drop a `.`/`..` segment and keep an empty one,
        // silently addressing another resource.
        if let Some(bad) = segments.iter().find(|s| matches!(**s, "" | "." | "..")) {
            return Err(StackError::decode(format!(
                "unusable Google Play resource id {bad:?}"
            )));
        }

        let mut url = reqwest::Url::parse(&self.publisher_base_url)
            .map_err(|e| StackError::network(format!("invalid Google Play API URL: {e}")))?;
        url.path_segments_mut()
            .map_err(|()| StackError::network("invalid Google Play API URL: not a base URL"))?
            .pop_if_empty()
            .extend(PUBLISHER_APPLICATIONS_PATH)
            .push(package_name)
            .extend(segments);
        Ok(url)
    }

    /// Sends the request `build` makes, authorized with the cached bearer
    /// token, and returns the 2xx body. `build` is called again for the retry,
    /// so it must not consume anything.
    ///
    /// On a 401 the cached access token is dropped and the call retried once
    /// with a fresh one: Google decides token validity server-side, so a cached
    /// token can die before its `expires_in`.
    ///
    /// # Errors
    /// The authenticator's errors, [`StackError::Network`] on transport failure,
    /// or [`map_google_error`] (with `hints`) for a non-2xx response.
    pub(super) async fn execute<F>(
        &self,
        build: F,
        hints: &ErrorHints<'_>,
    ) -> Result<String, StackError>
    where
        F: Fn() -> reqwest::RequestBuilder + Send + Sync,
    {
        let (mut status, mut body) = self.send_authorized(&build).await?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.auth.invalidate().await;
            (status, body) = self.send_authorized(&build).await?;
        }

        if status.is_success() {
            return Ok(body);
        }
        Err(map_google_error(
            status.as_u16(),
            &body,
            self.auth.client_email(),
            hints,
        ))
    }

    /// Sends one request built by `build` with the current bearer token.
    async fn send_authorized<F>(
        &self,
        build: &F,
    ) -> Result<(reqwest::StatusCode, String), StackError>
    where
        F: Fn() -> reqwest::RequestBuilder + Send + Sync,
    {
        let token = self.auth.bearer_token().await?;
        send_and_read(build().bearer_auth(token), self.debug_logger.as_deref()).await
    }

    /// One `GET {reporting}/v1beta1/apps:search?pageSize=…[&pageToken=…]`,
    /// returning the 2xx body. The token goes through `.query()`, so it is
    /// URL-encoded.
    ///
    /// # Errors
    /// See [`Self::execute`].
    async fn search_apps(
        &self,
        page_size: u32,
        page_token: Option<&str>,
    ) -> Result<String, StackError> {
        let url = format!("{}{APPS_SEARCH_PATH}", self.reporting_base_url);
        self.execute(
            || {
                let request = self.http.get(&url).query(&[("pageSize", page_size)]);
                match page_token {
                    Some(page_token) => request.query(&[("pageToken", page_token)]),
                    None => request,
                }
            },
            &ErrorHints::REPORTING,
        )
        .await
    }

    /// Cheap credential check: a token exchange, then
    /// `apps:search?pageSize=1`.
    ///
    /// `Ok` means the key works and the Reporting API is enabled and answering.
    /// It does **not** mean any app is visible yet: a service account that was
    /// never invited in Play Console, or whose invite is still propagating, may
    /// get an empty `{}`. Nor does it check the Android Publisher API (reviews,
    /// details, listings, tracks), which must be enabled separately.
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
    use crate::providers::googleplay::test_support::{
        client, mount_token, CapturingLogger, EMAIL, KID, PKCS8_PEM,
    };
    use crate::providers::googleplay::PLAY_SCOPES;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{bearer_token, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    /// The real SERVICE_DISABLED 403 (same capture as the `error` tests).
    const SERVICE_DISABLED_BODY: &str = r#"{"error":{"code":403,"message":"Google Play Developer Reporting API has not been used in project 764216752430 before or it is disabled.","status":"PERMISSION_DENIED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"SERVICE_DISABLED","domain":"googleapis.com","metadata":{"service":"playdeveloperreporting.googleapis.com","consumer":"projects/764216752430","containerInfo":"764216752430","serviceTitle":"Google Play Developer Reporting API","activationUrl":"https://console.developers.google.com/apis/api/playdeveloperreporting.googleapis.com/overview?project=764216752430"}}]}}"#;

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
        // `client(..)` uses `with_base_urls`, whose `debug_logger` defaults to
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

    /// A client that never touches the network, for URL-building tests.
    fn offline_client(reporting: &str, publisher: &str) -> GooglePlayClient {
        let http = reqwest::Client::new();
        let key = ServiceAccountKey::parse(EMAIL, KID, PKCS8_PEM).unwrap();
        let auth = ServiceAccountAuthenticator::new(key, PLAY_SCOPES, http.clone());
        GooglePlayClient::with_base_urls(auth, http, reporting.into(), publisher.into())
    }

    #[test]
    fn new_targets_reporting_and_publisher_apis() {
        let http = reqwest::Client::new();
        let key = ServiceAccountKey::parse(EMAIL, KID, PKCS8_PEM).unwrap();
        let auth = ServiceAccountAuthenticator::new(key, PLAY_SCOPES, http.clone());
        let client = GooglePlayClient::new(auth, http);
        assert_eq!(
            client.reporting_base_url,
            "https://playdeveloperreporting.googleapis.com"
        );
        assert_eq!(
            client.publisher_base_url,
            "https://androidpublisher.googleapis.com"
        );
    }

    #[test]
    fn with_base_urls_trims_trailing_slashes() {
        let client = offline_client("http://reporting.test/", "http://publisher.test//");
        assert_eq!(client.reporting_base_url, "http://reporting.test");
        assert_eq!(client.publisher_base_url, "http://publisher.test");
    }

    // -----------------------------------------------------------------------
    // Package names and publisher URLs
    // -----------------------------------------------------------------------

    #[test]
    fn package_name_rules_follow_android() {
        for valid in [
            "com.example.app",
            "a.b",
            "Com.Example_1.App2",
            "com.example.app_",
            "x1.y_2.z3",
        ] {
            assert!(is_valid_package_name(valid), "{valid} should be valid");
        }
        for invalid in [
            "",
            "com",
            ".",
            "..",
            "com.",
            ".com",
            "com..example",
            "1com.example",
            "com.1example",
            "_com.example",
            "com.exa-mple",
            "com.example/app",
            "com.example app",
            "com.exämple",
            " com.example",
        ] {
            assert!(
                !is_valid_package_name(invalid),
                "{invalid:?} should be invalid"
            );
        }
    }

    #[test]
    fn publisher_url_appends_encoded_segments() {
        let client = offline_client("http://reporting.test", "http://publisher.test");
        let url = client
            .publisher_url("com.example.app", &["reviews", "gp:AOqp/T?#%:reply"])
            .unwrap();
        assert_eq!(
            url.as_str(),
            "http://publisher.test/androidpublisher/v3/applications/com.example.app/\
             reviews/gp:AOqp%2FT%3F%23%25:reply"
        );

        let bare = client.publisher_url("com.example.app", &[]).unwrap();
        assert_eq!(
            bare.as_str(),
            "http://publisher.test/androidpublisher/v3/applications/com.example.app"
        );
    }

    #[test]
    fn publisher_url_keeps_a_base_path() {
        let client = offline_client("http://reporting.test", "http://proxy.test/google");
        let url = client.publisher_url("com.example.app", &["edits"]).unwrap();
        assert_eq!(
            url.as_str(),
            "http://proxy.test/google/androidpublisher/v3/applications/com.example.app/edits"
        );
    }

    #[test]
    fn publisher_url_rejects_invalid_package_as_local_404() {
        let client = offline_client("http://reporting.test", "http://publisher.test");
        for package in ["", "..", "com.example/../other.app", "not-a-package"] {
            match client.publisher_url(package, &["reviews"]).unwrap_err() {
                StackError::Http { status, message } => {
                    assert_eq!(status, 404);
                    assert!(message.contains("is not a valid Android package name"));
                    assert!(message.contains("nothing was sent to Google"));
                }
                other => panic!("expected Http 404 for {package:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn publisher_url_rejects_dot_and_blank_segments() {
        let client = offline_client("http://reporting.test", "http://publisher.test");
        for segment in ["", ".", ".."] {
            match client
                .publisher_url("com.example.app", &["edits", segment])
                .unwrap_err()
            {
                StackError::Decode { message } => {
                    assert!(message.contains("unusable Google Play resource id"))
                }
                other => panic!("expected Decode for {segment:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn publisher_url_with_invalid_base_is_network() {
        let client = offline_client("http://reporting.test", "not a url");
        let err = client.publisher_url("com.example.app", &[]).unwrap_err();
        assert!(matches!(err, StackError::Network { .. }));
    }

    #[test]
    fn trace_is_lazy_and_needs_a_logger() {
        let client = offline_client("http://reporting.test", "http://publisher.test");
        // No logger: the closure must not even run.
        client.trace(|| panic!("formatted without a logger"));

        let logger = Arc::new(CapturingLogger::default());
        let client = client.with_debug_logger(Some(logger.clone()));
        client.trace(|| "hello".to_string());
        assert_eq!(logger.messages(), vec!["hello".to_string()]);
    }
}
