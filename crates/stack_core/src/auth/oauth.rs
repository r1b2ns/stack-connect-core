use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tokio::sync::Mutex;

use super::jwt::signed_assertion;
use super::service_account::ServiceAccount;
use crate::error::StackError;

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// Safety margin subtracted from the real expiry before a token is treated as stale.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
}

#[derive(Clone)]
struct CachedToken {
    token: String,
    expires_at: SystemTime,
}

/// Exchanges a service-account JWT for an OAuth2 access token and caches it until
/// 60s before its real expiry (mirrors the Swift `PlayAuthenticator`).
pub(crate) struct GoogleAuthenticator {
    account: ServiceAccount,
    scopes: Vec<String>,
    http: reqwest::Client,
    cache: Mutex<Option<CachedToken>>,
}

impl GoogleAuthenticator {
    pub(crate) fn new(account: ServiceAccount, scopes: Vec<String>, http: reqwest::Client) -> Self {
        Self {
            account,
            scopes,
            http,
            cache: Mutex::new(None),
        }
    }

    pub(crate) async fn access_token(&self) -> Result<String, StackError> {
        let mut cache = self.cache.lock().await;
        if let Some(cached) = cache.as_ref() {
            if !is_expired(cached.expires_at, SystemTime::now()) {
                return Ok(cached.token.clone());
            }
        }

        let fresh = self.exchange().await?;
        let token = fresh.token.clone();
        *cache = Some(fresh);
        Ok(token)
    }

    async fn exchange(&self) -> Result<CachedToken, StackError> {
        let assertion = signed_assertion(&self.account, &self.scopes, unix_now())?;

        let response = self
            .http
            .post(&self.account.token_uri)
            .form(&[
                ("grant_type", GRANT_TYPE),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| StackError::network(e.to_string()))?;
        if !status.is_success() {
            return Err(StackError::auth(format!(
                "token exchange failed ({}): {body}",
                status.as_u16()
            )));
        }

        let parsed: TokenResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("token response: {e}")))?;

        Ok(CachedToken {
            token: parsed.access_token,
            expires_at: SystemTime::now() + Duration::from_secs(parsed.expires_in),
        })
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A token is stale once `now` reaches `expires_at - 60s`.
fn is_expired(expires_at: SystemTime, now: SystemTime) -> bool {
    now + EXPIRY_MARGIN >= expires_at
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_account(token_uri: String) -> ServiceAccount {
        ServiceAccount {
            private_key_id: "kid".into(),
            private_key: include_str!("../../tests/fixtures/test_rsa_private.pem").into(),
            client_email: "svc@test.iam.gserviceaccount.com".into(),
            token_uri,
        }
    }

    #[test]
    fn token_valid_outside_margin() {
        let now = SystemTime::now();
        assert!(!is_expired(now + Duration::from_secs(120), now));
    }

    #[test]
    fn token_stale_inside_margin() {
        let now = SystemTime::now();
        assert!(is_expired(now + Duration::from_secs(30), now));
    }

    #[tokio::test]
    async fn exchanges_then_serves_from_cache() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "abc123",
                "token_type": "Bearer",
                "expires_in": 3600
            })))
            .expect(1) // second call must be served from cache
            .mount(&server)
            .await;

        let auth = GoogleAuthenticator::new(
            test_account(format!("{}/token", server.uri())),
            vec!["scope".into()],
            reqwest::Client::new(),
        );

        assert_eq!(auth.access_token().await.unwrap(), "abc123");
        assert_eq!(auth.access_token().await.unwrap(), "abc123");
        // `expect(1)` is verified when `server` drops.
    }

    #[tokio::test]
    async fn surfaces_token_exchange_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_string("invalid_grant"))
            .mount(&server)
            .await;

        let auth = GoogleAuthenticator::new(
            test_account(format!("{}/token", server.uri())),
            vec!["scope".into()],
            reqwest::Client::new(),
        );

        let err = auth.access_token().await.unwrap_err();
        assert!(matches!(err, StackError::Auth { .. }));
    }
}
