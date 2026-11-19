//! Raw requests with a signed-in session, for the dev-only
//! `apple_signin_probe` CLI: it checks which App Store Connect and Apple
//! Developer endpoints a web session reaches before the core relies on them
//! (provisioning on `services-account`, open question Q1 of the Apple ID
//! sign-in plan).
//!
//! Like the rest of the module, nothing here logs a header, a cookie or a
//! body: requests go through [`transport::send`]. A [`SessionProbe`] keeps the
//! `csrf` / `csrf_ts` tokens of the latest response and sends them on the
//! following requests, as spaceship's `store_csrf_tokens` does.
//!
//! A `POST` without a body is sent with an empty body and `Content-Length: 0`,
//! as Ruby's `Net::HTTP` (under spaceship's Faraday client) sends it:
//! `HTTPGenericRequest#set_body_internal` turns a missing body into `""` for
//! any method that permits one. hyper writes no `Content-Length` for an empty
//! body, and the Developer Portal answers such a `POST` with HTTP 411. Like
//! spaceship's `NetHTTPGenericRequestMonkeypatch` for `*.apple.com`, no
//! default `Content-Type` is added.

use reqwest::header::CONTENT_LENGTH;
use serde_json::{json, Value};

use super::error::AppleSignInError;
pub use super::portal::summarize_errors;
use super::portal::{self, text};
use super::session::AppleSession;
use super::transport;

/// The CSRF token headers echoed back (see the module docs).
const CSRF_HEADERS: [&str; 2] = ["csrf", "csrf_ts"];

/// Which host a probe goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeHost {
    /// `appstoreconnect.apple.com`.
    AppStoreConnect,
    /// `developer.apple.com`.
    Developer,
}

/// One probe request. The path is relative to the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRequest {
    /// The host.
    pub host: ProbeHost,
    /// `GET` or `POST`.
    pub method: reqwest::Method,
    /// The path and query, without a leading `/`.
    pub path: String,
    /// Extra headers.
    pub headers: Vec<(String, String)>,
    /// A JSON body.
    pub body: Option<Value>,
}

impl ProbeRequest {
    /// spaceship's `PortalClient#teams`: `request(:post, "account/listTeams.action")`
    /// on `https://developer.apple.com/services-account/QH65B2/`, with no
    /// params and no body (so `Content-Length: 0`, see the module docs), the
    /// session cookies and the latest CSRF tokens. The only other headers
    /// are the ones `Net::HTTP` adds by itself ([`net_http_defaults`]): the
    /// Faraday client has no request middleware, and `teams` sets none.
    pub fn portal_teams() -> Self {
        Self {
            host: ProbeHost::Developer,
            method: reqwest::Method::POST,
            path: portal::list_teams_path(),
            headers: net_http_defaults(),
            body: None,
        }
    }

    /// spaceship's `Provisioning::Client#proxy_get` for `resource` (e.g.
    /// `devices`): the App Store Connect API path `v1/{resource}` on
    /// `services-account`, sent as a `POST` with `X-HTTP-Method-Override: GET`
    /// and the Developer Portal `team_id` plus the query in the body. Its
    /// headers are the three `proxy_get` sets plus [`net_http_defaults`].
    pub fn provisioning_list(resource: &str, team_id: &str, query: &str) -> Self {
        Self {
            host: ProbeHost::Developer,
            method: reqwest::Method::POST,
            path: format!("{}/v1/{resource}", portal::SERVICES_ACCOUNT),
            headers: [
                ("Content-Type", portal::PROXY_CONTENT_TYPE),
                (
                    portal::METHOD_OVERRIDE_HEADER,
                    portal::ProxiedMethod::Get.override_value(),
                ),
                ("X-Requested-With", portal::PROXY_REQUESTED_WITH),
            ]
            .into_iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .chain(net_http_defaults())
            .collect(),
            body: Some(json!({ "urlEncodedQueryParams": query, "teamId": team_id })),
        }
    }
}

/// [`portal::net_http_defaults`] as owned pairs.
fn net_http_defaults() -> Vec<(String, String)> {
    portal::net_http_defaults()
        .into_iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// What a probe answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReply {
    /// The HTTP status.
    pub status: u16,
    /// The body.
    pub body: String,
    /// Whether the response carried CSRF tokens.
    pub csrf_received: bool,
    /// The response `Content-Type`, empty when absent.
    pub content_type: String,
    /// The response `Content-Encoding`, empty when absent (the transport has
    /// already undone a gzip / deflate one).
    pub content_encoding: String,
}

/// Sends probes with a session, carrying the CSRF tokens across requests.
#[derive(Debug)]
pub struct SessionProbe<'a> {
    session: &'a AppleSession,
    csrf: Vec<(&'static str, String)>,
}

impl<'a> SessionProbe<'a> {
    /// A probe over `session`.
    pub fn new(session: &'a AppleSession) -> Self {
        Self {
            session,
            csrf: Vec::new(),
        }
    }

    /// Sends `request`.
    ///
    /// # Errors
    /// [`AppleSignInError::InvalidInput`] for an absolute path;
    /// [`AppleSignInError::Network`] on transport failure. Any status is a
    /// reply, not an error.
    pub async fn send(&mut self, request: &ProbeRequest) -> Result<ProbeReply, AppleSignInError> {
        if request.path.contains("://") || request.path.starts_with('/') {
            return Err(AppleSignInError::invalid_input(
                "the probe path must be relative to its host",
            ));
        }
        let endpoints = self.session.endpoints();
        let url = match request.host {
            ProbeHost::AppStoreConnect => endpoints.app_store_connect(&request.path),
            ProbeHost::Developer => endpoints.developer(&request.path),
        };
        let mut builder = self.session.http().request(request.method.clone(), url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        for (name, value) in &self.csrf {
            builder = builder.header(*name, value);
        }
        builder = match &request.body {
            Some(body) => builder.body(body.to_string()),
            None if method_permits_body(&request.method) => {
                builder.header(CONTENT_LENGTH, "0").body(Vec::<u8>::new())
            }
            None => builder,
        };
        let reply = transport::send(builder).await?;

        let tokens: Vec<(&'static str, String)> = CSRF_HEADERS
            .iter()
            .filter_map(|name| reply.header(name).map(|v| (*name, v.to_string())))
            .collect();
        let csrf_received = !tokens.is_empty();
        if csrf_received {
            self.csrf = tokens;
        }
        Ok(ProbeReply {
            status: reply.code(),
            content_type: transport::header_text(&reply.headers, "content-type"),
            content_encoding: transport::header_text(&reply.headers, "content-encoding"),
            body: reply.body,
            csrf_received,
        })
    }
}

/// Whether Ruby's `Net::HTTP` gives `method` a request body (and so a
/// `Content-Length`) when none is set: `Post`, `Put` and `Patch` have
/// `REQUEST_HAS_BODY = true`.
fn method_permits_body(method: &reqwest::Method) -> bool {
    [
        reqwest::Method::POST,
        reqwest::Method::PUT,
        reqwest::Method::PATCH,
    ]
    .contains(method)
}

/// Why a probe failed, for a terminal line: Apple's errors in `body` (see
/// [`summarize_errors`]), else what the body is (empty, its size when not
/// JSON). Never quotes a header or a non-JSON body.
pub fn failure_reason(body: &str) -> String {
    let summary = summarize_errors(body);
    if !summary.is_empty() {
        return summary;
    }
    if body.trim().is_empty() {
        "empty body".to_string()
    } else if serde_json::from_str::<Value>(body).is_ok() {
        "no Apple error in the JSON body".to_string()
    } else {
        format!("non-JSON body ({} bytes)", body.len())
    }
}

/// The Developer Portal teams in a `listTeams.action` answer:
/// `(teamId, name, type)`.
pub fn portal_teams(body: &str) -> Vec<(String, String, String)> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("teams").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .map(|t| (text(t, "teamId"), text(t, "name"), text(t, "type")))
        .filter(|(id, _, _)| !id.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::apple_session::test_support::session_on;
    use crate::auth::apple_session::AppleEndpoints;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn provisioning_list_is_spaceships_proxied_get() {
        let request = ProbeRequest::provisioning_list("devices", "ABCDE12345", "limit=1");
        assert_eq!(request.method, reqwest::Method::POST);
        assert_eq!(request.path, "services-account/v1/devices");
        assert!(request
            .headers
            .contains(&("X-HTTP-Method-Override".into(), "GET".into())));
        assert_eq!(
            request.body,
            Some(json!({"urlEncodedQueryParams": "limit=1", "teamId": "ABCDE12345"}))
        );
        assert_eq!(
            ProbeRequest::portal_teams().path,
            "services-account/QH65B2/account/listTeams.action"
        );
    }

    #[test]
    fn summarize_reads_every_error_shape() {
        assert_eq!(
            summarize_errors(
                r#"{"errors":[{"status":"404","code":"PATH_ERROR","title":"The URL path is not valid"}]}"#
            ),
            "PATH_ERROR: The URL path is not valid"
        );
        assert_eq!(
            summarize_errors(r#"{"resultCode":1100,"userString":"Your session has expired."}"#),
            "resultCode 1100: Your session has expired."
        );
        assert_eq!(summarize_errors(r#"{"resultCode":0,"teams":[]}"#), "");
        assert_eq!(summarize_errors("<html>"), "");
        assert_eq!(
            summarize_errors(r#"{"errors":[{"code":"X","detail":"user dev@example.com"}]}"#),
            "X: user d***@example.com"
        );
    }

    #[test]
    fn summarize_quotes_the_portal_result_string() {
        assert_eq!(
            summarize_errors(
                r#"{"resultCode":1100,"resultString":"webservice.session.expired","userString":"Your session has expired."}"#
            ),
            "resultCode 1100 (webservice.session.expired): Your session has expired."
        );
    }

    #[test]
    fn failure_reason_never_quotes_a_non_json_body() {
        assert_eq!(
            failure_reason(r#"{"resultCode":35,"userString":"No team."}"#),
            "resultCode 35: No team."
        );
        assert_eq!(failure_reason(""), "empty body");
        assert_eq!(
            failure_reason(r#"{"teams":[]}"#),
            "no Apple error in the JSON body"
        );
        let html = "<html><body>411 Length Required secret-cookie</body></html>";
        let reason = failure_reason(html);
        assert_eq!(reason, format!("non-JSON body ({} bytes)", html.len()));
        assert!(!reason.contains("secret"));
    }

    /// The request wiremock received for the only call to `path`.
    async fn received(server: &MockServer, path: &str) -> wiremock::Request {
        let mut requests = server.received_requests().await.unwrap_or_default();
        requests.retain(|r| r.url.path() == path);
        assert_eq!(requests.len(), 1, "one request to {path}");
        requests.remove(0)
    }

    fn header_value<'r>(request: &'r wiremock::Request, name: &str) -> Option<&'r str> {
        request.headers.get(name).and_then(|v| v.to_str().ok())
    }

    #[tokio::test]
    async fn list_teams_is_sent_with_an_empty_body_and_content_length_zero() {
        let server = MockServer::start().await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri())
            .with_developer_base_url(&server.uri());
        let session = session_on(&endpoints, None);
        Mock::given(method("POST"))
            .and(path("/services-account/QH65B2/account/listTeams.action"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"resultCode":0,"teams":[]}"#),
            )
            .mount(&server)
            .await;

        let reply = SessionProbe::new(&session)
            .send(&ProbeRequest::portal_teams())
            .await
            .unwrap();
        assert_eq!(reply.status, 200);

        let sent = received(&server, "/services-account/QH65B2/account/listTeams.action").await;
        assert_eq!(header_value(&sent, "content-length"), Some("0"));
        assert!(sent.body.is_empty());
        assert_eq!(header_value(&sent, "content-type"), None);
        // Exactly spaceship's headers: no X-Requested-With, Net::HTTP's Accept.
        assert_eq!(header_value(&sent, "x-requested-with"), None);
        assert_eq!(header_value(&sent, "accept"), Some("*/*"));
        assert_eq!(
            header_value(&sent, "accept-encoding"),
            Some("gzip;q=1.0,deflate;q=0.6,identity;q=0.3")
        );
        assert_eq!(
            header_value(&sent, "user-agent"),
            Some(transport::USER_AGENT)
        );
    }

    #[tokio::test]
    async fn proxied_get_carries_its_json_body_and_length() {
        let server = MockServer::start().await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri())
            .with_developer_base_url(&server.uri());
        let session = session_on(&endpoints, None);
        Mock::given(method("POST"))
            .and(path("/services-account/v1/profiles"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data":[]}"#))
            .mount(&server)
            .await;

        SessionProbe::new(&session)
            .send(&ProbeRequest::provisioning_list(
                "profiles", "T1", "limit=1",
            ))
            .await
            .unwrap();

        let sent = received(&server, "/services-account/v1/profiles").await;
        let expected = json!({"urlEncodedQueryParams": "limit=1", "teamId": "T1"});
        assert_eq!(
            serde_json::from_slice::<Value>(&sent.body).unwrap(),
            expected
        );
        assert_eq!(
            header_value(&sent, "content-length"),
            Some(sent.body.len().to_string().as_str())
        );
        assert_eq!(
            header_value(&sent, "content-type"),
            Some("application/vnd.api+json")
        );
        assert_eq!(header_value(&sent, "x-http-method-override"), Some("GET"));
        assert_eq!(
            header_value(&sent, "x-requested-with"),
            Some("XMLHttpRequest")
        );
        assert_eq!(header_value(&sent, "accept"), Some("*/*"));
        assert_eq!(
            header_value(&sent, "accept-encoding"),
            Some(transport::NET_HTTP_ACCEPT_ENCODING)
        );
    }

    #[tokio::test]
    async fn a_gzipped_list_teams_answer_is_read_and_its_type_reported() {
        use flate2::write::GzEncoder;
        use std::io::Write as _;

        let server = MockServer::start().await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri())
            .with_developer_base_url(&server.uri());
        let session = session_on(&endpoints, None);
        let json = r#"{"resultCode":0,"teams":[{"teamId":"ABCDE12345","name":"Main","type":"Individual"}]}"#;
        let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(json.as_bytes()).unwrap();
        Mock::given(method("POST"))
            .and(path("/services-account/QH65B2/account/listTeams.action"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json;charset=UTF-8")
                    .insert_header("content-encoding", "gzip")
                    .set_body_bytes(encoder.finish().unwrap()),
            )
            .mount(&server)
            .await;

        let reply = SessionProbe::new(&session)
            .send(&ProbeRequest::portal_teams())
            .await
            .unwrap();
        assert_eq!(reply.content_type, "application/json;charset=UTF-8");
        assert_eq!(reply.content_encoding, "gzip");
        assert_eq!(portal_teams(&reply.body).len(), 1);
    }

    #[tokio::test]
    async fn a_bodyless_get_gets_no_content_length() {
        let server = MockServer::start().await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri())
            .with_developer_base_url(&server.uri());
        let session = session_on(&endpoints, None);
        Mock::given(method("GET"))
            .and(path("/iris/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data":[]}"#))
            .mount(&server)
            .await;

        SessionProbe::new(&session)
            .send(&ProbeRequest {
                host: ProbeHost::AppStoreConnect,
                method: reqwest::Method::GET,
                path: "iris/v1/devices".into(),
                headers: Vec::new(),
                body: None,
            })
            .await
            .unwrap();

        let sent = received(&server, "/iris/v1/devices").await;
        assert_eq!(header_value(&sent, "content-length"), None);
        assert!(sent.body.is_empty());
    }

    #[test]
    fn portal_teams_are_parsed() {
        let body = r#"{"teams":[{"teamId":"ABCDE12345","name":"Main","type":"Company/Organization"},{"name":"no id"}]}"#;
        assert_eq!(
            portal_teams(body),
            vec![(
                "ABCDE12345".to_string(),
                "Main".to_string(),
                "Company/Organization".to_string()
            )]
        );
        assert!(portal_teams("nope").is_empty());
    }

    #[tokio::test]
    async fn probe_echoes_csrf_tokens_and_refuses_absolute_paths() {
        let server = MockServer::start().await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri())
            .with_developer_base_url(&server.uri());
        let session = session_on(&endpoints, None);
        Mock::given(method("POST"))
            .and(path("/services-account/QH65B2/account/listTeams.action"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("csrf", "tok")
                    .insert_header("csrf_ts", "123")
                    .set_body_string(r#"{"resultCode":0,"teams":[]}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/services-account/v1/devices"))
            .and(header("csrf", "tok"))
            .and(header("csrf_ts", "123"))
            .and(header("x-http-method-override", "GET"))
            .and(body_json(
                json!({"urlEncodedQueryParams": "limit=1", "teamId": "T1"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data":[]}"#))
            .expect(1)
            .mount(&server)
            .await;

        let mut probe = SessionProbe::new(&session);
        let teams = probe.send(&ProbeRequest::portal_teams()).await.unwrap();
        assert!(teams.csrf_received);
        let devices = probe
            .send(&ProbeRequest::provisioning_list("devices", "T1", "limit=1"))
            .await
            .unwrap();
        assert_eq!(devices.status, 200);
        assert!(!devices.csrf_received);

        Mock::given(method("GET"))
            .and(path("/iris/v1/devices"))
            .and(header("csrf", "tok"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_string(r#"{"errors":[{"code":"PATH_ERROR","title":"not here"}]}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        let iris = probe
            .send(&ProbeRequest {
                host: ProbeHost::AppStoreConnect,
                method: reqwest::Method::GET,
                path: "iris/v1/devices".into(),
                headers: Vec::new(),
                body: None,
            })
            .await
            .unwrap();
        assert_eq!(iris.status, 404);
        assert_eq!(summarize_errors(&iris.body), "PATH_ERROR: not here");

        let mut absolute = ProbeRequest::portal_teams();
        absolute.path = "https://example.com/x".into();
        assert!(matches!(
            probe.send(&absolute).await,
            Err(AppleSignInError::InvalidInput { .. })
        ));
    }
}
