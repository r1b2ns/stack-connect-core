use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::error::StackError;

/// App Store Connect requires `aud = "appstoreconnect-v1"`.
const AUDIENCE: &str = "appstoreconnect-v1";
/// Token lifetime. Apple rejects tokens expiring more than 20 minutes out, so we
/// stay safely under the cap (matches the Swift SDK's recommended 20-minute max).
const TTL_SECS: i64 = 1200;
/// Regenerate this long before `exp` to avoid handing out a token that expires
/// mid-flight.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// Team-key JWT header for App Store Connect: `alg = ES256`, `typ = JWT`,
/// `kid = <Key ID>`.
#[derive(Serialize)]
struct TeamClaims {
    iss: String,
    iat: i64,
    exp: i64,
    aud: &'static str,
}

/// Signs an ES256 App Store Connect team JWT.
///
/// `now` is the Unix time in seconds, injected so golden tests are deterministic.
/// `private_key_p8` is the raw `.p8` the developer downloads from App Store
/// Connect — PEM PKCS#8 EC (`-----BEGIN PRIVATE KEY-----`).
///
/// # Errors
/// [`StackError::Auth`] if the key is not a valid P-256 PKCS#8 PEM or signing fails.
pub(crate) fn sign_team_token(
    issuer_id: &str,
    key_id: &str,
    private_key_p8: &[u8],
    now: i64,
) -> Result<String, StackError> {
    let claims = TeamClaims {
        iss: issuer_id.to_string(),
        iat: now,
        exp: now + TTL_SECS,
        aud: AUDIENCE,
    };

    let mut header = Header::new(Algorithm::ES256);
    header.typ = Some("JWT".to_string());
    header.kid = Some(key_id.to_string());

    let key = EncodingKey::from_ec_pem(private_key_p8)
        .map_err(|e| StackError::auth(format!("invalid .p8 private key: {e}")))?;

    encode(&header, &claims, &key).map_err(|e| StackError::auth(format!("JWT signing failed: {e}")))
}

#[derive(Clone)]
struct CachedToken {
    token: String,
    /// Unix-seconds `exp` of the cached token.
    expires_at: i64,
}

/// Mints and caches App Store Connect team JWTs until shortly before they expire.
/// Cloning the team key once keeps the credentials inside the core; the cache is
/// guarded by a `tokio::sync::Mutex` so concurrent requests share one token.
pub(crate) struct AppStoreAuthenticator {
    issuer_id: String,
    key_id: String,
    private_key_p8: Vec<u8>,
    cache: Mutex<Option<CachedToken>>,
}

impl AppStoreAuthenticator {
    pub(crate) fn new(issuer_id: String, key_id: String, private_key_p8: Vec<u8>) -> Self {
        Self {
            issuer_id,
            key_id,
            private_key_p8,
            cache: Mutex::new(None),
        }
    }

    /// Returns a valid bearer token, signing a fresh one only when the cache is
    /// empty or within [`REFRESH_MARGIN`] of expiry.
    pub(crate) async fn bearer_token(&self) -> Result<String, StackError> {
        let now = unix_now();
        let mut cache = self.cache.lock().await;
        if let Some(cached) = cache.as_ref() {
            if !needs_refresh(cached.expires_at, now) {
                return Ok(cached.token.clone());
            }
        }

        let token = sign_team_token(&self.issuer_id, &self.key_id, &self.private_key_p8, now)?;
        *cache = Some(CachedToken {
            token: token.clone(),
            expires_at: now + TTL_SECS,
        });
        Ok(token)
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A cached token must be refreshed once `now` reaches `exp - REFRESH_MARGIN`.
fn needs_refresh(expires_at: i64, now: i64) -> bool {
    now + REFRESH_MARGIN.as_secs() as i64 >= expires_at
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};

    const PRIVATE_P8: &[u8] = include_bytes!("../../tests/fixtures/test_ec_private.p8");
    const PUBLIC_PEM: &[u8] = include_bytes!("../../tests/fixtures/test_ec_public.pem");

    #[derive(serde::Deserialize)]
    struct Decoded {
        iss: String,
        iat: i64,
        exp: i64,
        aud: String,
    }

    #[test]
    fn signs_verifiable_es256_team_token() {
        let now = 1_700_000_000;
        let jwt = sign_team_token("issuer-123", "KEYID4567", PRIVATE_P8, now).unwrap();

        let header = decode_header(&jwt).unwrap();
        assert_eq!(header.alg, Algorithm::ES256);
        assert_eq!(header.typ.as_deref(), Some("JWT"));
        assert_eq!(header.kid.as_deref(), Some("KEYID4567"));

        let key = DecodingKey::from_ec_pem(PUBLIC_PEM).unwrap();
        let mut validation = Validation::new(Algorithm::ES256);
        validation.validate_exp = false; // fixed `now` is in the past
        validation.set_audience(&[AUDIENCE]);

        let data = decode::<Decoded>(&jwt, &key, &validation).unwrap();
        assert_eq!(data.claims.iss, "issuer-123");
        assert_eq!(data.claims.aud, AUDIENCE);
        assert_eq!(data.claims.iat, now);
        assert_eq!(data.claims.exp, now + TTL_SECS);
    }

    #[test]
    fn rejects_non_ec_key() {
        let err = sign_team_token("iss", "kid", b"not a pem", 0).unwrap_err();
        assert!(matches!(err, StackError::Auth { .. }));
    }

    #[test]
    fn token_fresh_outside_margin() {
        // exp 120s out, now=0 → no refresh.
        assert!(!needs_refresh(120, 0));
    }

    #[test]
    fn token_stale_inside_margin() {
        // exp 30s out, now=0 → within the 60s margin → refresh.
        assert!(needs_refresh(30, 0));
    }

    #[tokio::test]
    async fn caches_token_across_calls() {
        let auth = AppStoreAuthenticator::new("issuer".into(), "kid".into(), PRIVATE_P8.to_vec());
        let first = auth.bearer_token().await.unwrap();
        let second = auth.bearer_token().await.unwrap();
        // Same `now` second → identical signed token served from cache.
        assert_eq!(first, second);
    }
}
