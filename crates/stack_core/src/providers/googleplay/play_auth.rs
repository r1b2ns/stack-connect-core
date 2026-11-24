//! How a Google Play account authorizes its API calls: either a service
//! account the core signs for ([`ServiceAccountAuthenticator`]), or the
//! user's own Google sign-in, whose access token the host supplies
//! ([`HostTokenAuthenticator`]).

use std::sync::Arc;

use super::error::Principal;
use crate::auth::host_token::HostTokenAuthenticator;
use crate::auth::oauth_jwt::ServiceAccountAuthenticator;
use crate::error::StackError;
use crate::http_debug::BearerRedactingLogger;
use crate::ports::DebugLogger;

/// The authenticator behind a [`super::client::GooglePlayClient`]. A closed
/// set, so an enum rather than a trait object: the transport needs to know
/// which kind it holds (to retry a 401, to name the principal in errors, to
/// redact the debug trace).
pub(crate) enum PlayAuth {
    /// A service account: the core signs a JWT-bearer assertion, exchanges it
    /// and caches the access token. Unchanged from before host tokens existed.
    ServiceAccount(ServiceAccountAuthenticator),
    /// The user's own Google sign-in: the host supplies the access token
    /// before every request; the core never caches or stores it.
    HostToken(HostTokenAuthenticator),
}

impl From<ServiceAccountAuthenticator> for PlayAuth {
    fn from(auth: ServiceAccountAuthenticator) -> Self {
        Self::ServiceAccount(auth)
    }
}

impl From<HostTokenAuthenticator> for PlayAuth {
    fn from(auth: HostTokenAuthenticator) -> Self {
        Self::HostToken(auth)
    }
}

impl PlayAuth {
    /// The bearer token for the next request.
    ///
    /// # Errors
    /// The underlying authenticator's errors.
    pub(super) async fn bearer_token(&self) -> Result<String, StackError> {
        match self {
            Self::ServiceAccount(auth) => auth.bearer_token().await,
            Self::HostToken(auth) => auth.bearer_token().await,
        }
    }

    /// Whether a request answered with 401 is worth retrying once after
    /// [`Self::invalidate`].
    ///
    /// A service account drops its cached token, so a retry exchanges a fresh
    /// one (`true`). A host token is the host's to refresh: asking again right
    /// away would most likely return the same rejected token, so the 401 is
    /// reported as is (`false`).
    pub(super) fn retries_unauthorized(&self) -> bool {
        match self {
            Self::ServiceAccount(_) => true,
            Self::HostToken(_) => false,
        }
    }

    /// Drops a cached token, if this authenticator keeps one (only the
    /// service account does).
    pub(super) async fn invalidate(&self) {
        match self {
            Self::ServiceAccount(auth) => auth.invalidate().await,
            Self::HostToken(_) => {}
        }
    }

    /// Who the calls are made as, for error messages and log lines.
    pub(super) fn principal(&self) -> Principal<'_> {
        match self {
            Self::ServiceAccount(auth) => Principal::ServiceAccount(auth.client_email()),
            Self::HostToken(_) => Principal::SignedInUser,
        }
    }

    /// Hands the HTTP tracing sink to the authenticator, when it does HTTP of
    /// its own (the service-account token exchange). Builder-style.
    pub(super) fn with_debug_logger(self, logger: Option<Arc<dyn DebugLogger>>) -> Self {
        match self {
            Self::ServiceAccount(auth) => Self::ServiceAccount(auth.with_debug_logger(logger)),
            host @ Self::HostToken(_) => host,
        }
    }

    /// The tracing sink the API calls should log through. The service-account
    /// path keeps the documented verbatim trace; a host token is the user's
    /// own credential, so its `Authorization` header is redacted.
    pub(super) fn api_debug_logger(
        &self,
        logger: Option<Arc<dyn DebugLogger>>,
    ) -> Option<Arc<dyn DebugLogger>> {
        match self {
            Self::ServiceAccount(_) => logger,
            Self::HostToken(_) => logger
                .map(|inner| Arc::new(BearerRedactingLogger::new(inner)) as Arc<dyn DebugLogger>),
        }
    }
}

#[cfg(test)]
mod tests {
    //! The host-token auth path through the real Google Play transport: the
    //! header sent, how provider failures and Google's 401/403/429 surface,
    //! and that the token is never logged. The service-account path keeps its
    //! own tests in `client.rs`, unchanged.

    use std::sync::Arc;

    use wiremock::matchers::{bearer_token, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::test_support::{
        client, host_token_client, mount_token, CapturingLogger, HOST_ACCOUNT,
    };
    use super::*;
    use crate::auth::host_token::testing::{Answer, ScriptedTokenProvider};
    use crate::logging::testing::capture_logs;
    use crate::logging::LogLevel;

    const TOKEN: &str = "ya29.a0-user-token";
    const SEARCH_PATH: &str = "/v1beta1/apps:search";
    const USERS_PATH: &str = "/androidpublisher/v3/developers/5374/users";

    fn search() -> wiremock::MockBuilder {
        Mock::given(method("GET")).and(path(SEARCH_PATH))
    }

    /// The token endpoint must never be hit on the host-token path.
    async fn forbid_token_exchange(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(server)
            .await;
    }

    fn auth_message(err: StackError) -> String {
        match err {
            StackError::Auth { message } => message,
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sends_the_host_token_as_bearer_without_a_token_exchange() {
        let server = MockServer::start().await;
        forbid_token_exchange(&server).await;
        search()
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        let provider = ScriptedTokenProvider::token(TOKEN);
        host_token_client(&server, provider.clone())
            .validate()
            .await
            .unwrap();
        assert_eq!(provider.calls(), vec![HOST_ACCOUNT.to_string()]);
    }

    #[tokio::test]
    async fn asks_the_host_before_every_request() {
        // Two pages, and the host rotated its token in between: each request
        // carries the token the host handed out for it.
        let server = MockServer::start().await;
        search()
            .and(bearer_token("tok-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [{"packageName": "com.example.one", "displayName": "One"}],
                "nextPageToken": "P2"
            })))
            .expect(1)
            .mount(&server)
            .await;
        search()
            .and(bearer_token("tok-2"))
            .and(query_param("pageToken", "P2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apps": [{"packageName": "com.example.two", "displayName": "Two"}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = ScriptedTokenProvider::new(vec![
            Answer::Token("tok-1".into()),
            Answer::Token("tok-2".into()),
        ]);
        let apps = host_token_client(&server, provider.clone())
            .fetch_apps()
            .await
            .unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(provider.calls().len(), 2);
    }

    #[tokio::test]
    async fn the_token_also_authorizes_android_publisher_calls() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .and(bearer_token(TOKEN))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        let users = host_token_client(&server, ScriptedTokenProvider::token(TOKEN))
            .fetch_users("5374")
            .await
            .unwrap();
        assert!(users.is_empty());
    }

    #[tokio::test]
    async fn provider_failure_is_auth_and_sends_nothing() {
        let server = MockServer::start().await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(0)
            .mount(&server)
            .await;

        let provider = ScriptedTokenProvider::failing(StackError::auth(
            "invalid_grant: the Google sign-in expired",
        ));
        let message = auth_message(
            host_token_client(&server, provider)
                .validate()
                .await
                .unwrap_err(),
        );
        assert_eq!(message, "invalid_grant: the Google sign-in expired");
    }

    #[tokio::test]
    async fn provider_non_auth_failure_is_auth_too() {
        let server = MockServer::start().await;
        let provider = ScriptedTokenProvider::failing(StackError::decode("keychain item corrupt"));
        let message = auth_message(
            host_token_client(&server, provider)
                .validate()
                .await
                .unwrap_err(),
        );
        assert!(
            message.starts_with("the host's access token provider failed: "),
            "{message}"
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn provider_offline_stays_network() {
        let server = MockServer::start().await;
        let provider = ScriptedTokenProvider::failing(StackError::network("offline"));
        let err = host_token_client(&server, provider)
            .validate()
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Network { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn google_401_is_auth_without_retry() {
        let server = MockServer::start().await;
        search()
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {
                    "code": 401,
                    "message": "Request had invalid authentication credentials.",
                    "status": "UNAUTHENTICATED"
                }
            })))
            // No retry: the host refreshes its own tokens.
            .expect(1)
            .mount(&server)
            .await;

        let logs = capture_logs();
        let provider = ScriptedTokenProvider::token(TOKEN);
        let message = auth_message(
            host_token_client(&server, provider.clone())
                .validate()
                .await
                .unwrap_err(),
        );
        assert_eq!(
            message,
            "Google did not accept the signed-in Google account's authorization \
             (HTTP 401): the sign-in may have expired or been revoked (sign in with \
             Google again), or the account may lack access in Play Console. Google \
             said: Request had invalid authentication credentials. [UNAUTHENTICATED] \
             (HTTP 401 on GET v1beta1/apps:search)"
        );
        assert!(!message.contains("service account"));
        assert_eq!(provider.calls().len(), 1);
        assert_eq!(
            logs.messages(LogLevel::Info, target_auth()),
            [
                "Google answered HTTP 401 on GET v1beta1/apps:search to the host-supplied \
              access token; not retrying (the host refreshes its own tokens)"
            ]
        );
    }

    #[tokio::test]
    async fn google_403_names_the_signed_in_account_not_a_service_account() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": {
                    "code": 403,
                    "message": "The caller does not have permission",
                    "status": "PERMISSION_DENIED"
                }
            })))
            .mount(&server)
            .await;

        let message = auth_message(
            host_token_client(&server, ScriptedTokenProvider::token(TOKEN))
                .fetch_users("5374")
                .await
                .unwrap_err(),
        );
        assert!(
            message.starts_with(
                "The signed-in Google account cannot access the developer account 5374."
            ),
            "{message}"
        );
        assert!(!message.contains("service account"), "{message}");
        assert!(!message.contains(HOST_ACCOUNT), "{message}");
    }

    #[tokio::test]
    async fn quota_429_keeps_the_service_account_shape() {
        let body = serde_json::json!({
            "error": {"code": 429, "message": "Too many requests.", "status": "RESOURCE_EXHAUSTED"}
        });
        let server = MockServer::start().await;
        mount_token(&server, "sa-tok", 1).await;
        search()
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "7")
                    .set_body_json(body),
            )
            .mount(&server)
            .await;

        let host_err = host_token_client(&server, ScriptedTokenProvider::token(TOKEN))
            .validate()
            .await
            .unwrap_err();
        let sa_err = client(&server).validate().await.unwrap_err();
        match (&host_err, &sa_err) {
            (
                StackError::Http {
                    status: 429,
                    message: host,
                },
                StackError::Http {
                    status: 429,
                    message: sa,
                },
            ) => {
                assert_eq!(host, sa);
                assert!(host.contains(
                    "this is a Google Play API quota/rate limit, not a permission problem"
                ));
                assert!(host.contains("(Retry-After: 7 s)"), "{host}");
            }
            other => panic!("expected two Http 429, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_token_is_never_logged() {
        let server = MockServer::start().await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .respond_with(ResponseTemplate::new(401).set_body_string("{}"))
            .mount(&server)
            .await;

        let logs = capture_logs();
        let debug = Arc::new(CapturingLogger::default());
        let client = host_token_client(&server, ScriptedTokenProvider::token(TOKEN))
            .with_debug_logger(Some(debug.clone()));
        client.validate().await.unwrap();
        let _ = client.fetch_users("5374").await;

        // The debug trace still shows the request, with the header redacted…
        let traced = debug.messages();
        assert_eq!(traced.len(), 4, "{traced:?}");
        assert!(traced[0].contains("curl -X GET"));
        assert!(traced[0].contains("authorization: Bearer [redacted]"));
        // …and the token appears nowhere: not in the debug trace, not in the
        // host log.
        assert!(
            traced.iter().all(|line| !line.contains(TOKEN)),
            "{traced:?}"
        );
        assert!(!logs.all_text().contains(TOKEN));
    }

    #[tokio::test]
    async fn service_account_debug_trace_stays_verbatim() {
        // The documented verbatim trace of the service-account path is
        // unchanged by the host-token redaction.
        let server = MockServer::start().await;
        mount_token(&server, "sa-tok", 1).await;
        search()
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let debug = Arc::new(CapturingLogger::default());
        client(&server)
            .with_debug_logger(Some(debug.clone()))
            .validate()
            .await
            .unwrap();
        assert!(debug.messages()[2].contains("authorization: Bearer sa-tok"));
    }

    fn target_auth() -> &'static str {
        crate::logging::target::AUTH
    }
}
