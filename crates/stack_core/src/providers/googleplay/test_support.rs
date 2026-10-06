//! Test-only helpers shared by the Google Play module tests: a client wired to
//! a wiremock server (token endpoint and both APIs on the same server), a token
//! mock, and a capturing debug logger.

use std::sync::Mutex;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::client::GooglePlayClient;
use super::PLAY_SCOPES;
use crate::auth::oauth_jwt::{ServiceAccountAuthenticator, ServiceAccountKey};
use crate::ports::DebugLogger;

pub(super) const PKCS8_PEM: &str =
    include_str!("../../../tests/fixtures/test_rsa_private_pkcs8.pem");
pub(super) const EMAIL: &str = "play-bot@my-project.iam.gserviceaccount.com";
pub(super) const KID: &str = "0123456789abcdef0123456789abcdef01234567";

/// A client whose authenticator posts to `{server}/token` and whose Reporting
/// and Android Publisher calls both go to the same mock server.
pub(super) fn client(server: &MockServer) -> GooglePlayClient {
    let http = reqwest::Client::new();
    let key = ServiceAccountKey::parse(EMAIL, KID, PKCS8_PEM).unwrap();
    let auth = ServiceAccountAuthenticator::with_token_url(
        key,
        PLAY_SCOPES,
        http.clone(),
        format!("{}/token", server.uri()),
    );
    GooglePlayClient::with_base_urls(auth, http, server.uri(), server.uri())
}

/// Mounts a token endpoint answering `token`, expected `times` times.
pub(super) async fn mount_token(server: &MockServer, token: &str, times: u64) {
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

/// A [`DebugLogger`] that keeps every line, in order.
#[derive(Default)]
pub(super) struct CapturingLogger {
    messages: Mutex<Vec<String>>,
}

impl CapturingLogger {
    pub(super) fn messages(&self) -> Vec<String> {
        self.messages.lock().unwrap().clone()
    }
}

impl DebugLogger for CapturingLogger {
    fn log(&self, message: String) {
        self.messages.lock().unwrap().push(message);
    }
}
