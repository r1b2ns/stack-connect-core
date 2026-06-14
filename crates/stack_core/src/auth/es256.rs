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

/// PKCS#8 PEM header line.
const PEM_BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
/// PKCS#8 PEM footer line.
const PEM_END: &str = "-----END PRIVATE KEY-----";
/// Standard PEM base64 line width.
const PEM_LINE_WIDTH: usize = 64;

/// Normalizes a `.p8` private key into a full PKCS#8 PEM that
/// [`EncodingKey::from_ec_pem`] accepts.
///
/// App Store Connect keys legitimately reach the core in two shapes:
/// 1. The full PEM the developer downloads (`-----BEGIN PRIVATE KEY-----` …).
/// 2. The bare base64 PKCS#8 body with PEM headers and newlines stripped — the
///    shape the legacy AppStoreConnect-Swift-SDK persists and the iOS host
///    forwards.
///
/// A full PEM is detected by the presence of `BEGIN` and returned untouched.
/// Otherwise every ASCII whitespace byte is removed and the remaining base64 is
/// re-wrapped at [`PEM_LINE_WIDTH`] chars between the standard header/footer.
/// No base64 decoding happens here; `from_ec_pem` performs validation and decode,
/// so non-base64 garbage still fails there (preserving the rejection contract).
fn normalize_p8_pem(raw: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw);
    if text.contains("BEGIN") {
        return raw.to_vec();
    }

    let body: String = text.split_whitespace().collect();

    let mut pem = String::with_capacity(body.len() + PEM_BEGIN.len() + PEM_END.len() + 8);
    pem.push_str(PEM_BEGIN);
    pem.push('\n');
    for chunk in body.as_bytes().chunks(PEM_LINE_WIDTH) {
        // SAFETY of from_utf8: `body` came from a UTF-8 `String` and base64 is
        // ASCII; chunking on a byte boundary of ASCII content stays valid UTF-8.
        // We use the checked variant anyway and fall back to skipping malformed
        // chunks so this never panics on adversarial input.
        if let Ok(line) = std::str::from_utf8(chunk) {
            pem.push_str(line);
            pem.push('\n');
        }
    }
    pem.push_str(PEM_END);
    pem.push('\n');

    pem.into_bytes()
}

/// Signs an ES256 App Store Connect team JWT.
///
/// `now` is the Unix time in seconds, injected so golden tests are deterministic.
/// `private_key_p8` is the developer's `.p8` from App Store Connect. Both a full
/// PKCS#8 PEM (`-----BEGIN PRIVATE KEY-----`) and a bare base64 PKCS#8 body (no
/// headers/newlines) are accepted; see [`normalize_p8_pem`].
///
/// # Errors
/// [`StackError::Auth`] if the key is not a valid P-256 PKCS#8 key or signing fails.
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

    let key = EncodingKey::from_ec_pem(&normalize_p8_pem(private_key_p8))
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

    /// Strips the BEGIN/END lines and all newlines from a PEM, yielding the bare
    /// base64 body the legacy iOS host forwards.
    fn strip_pem_headers(pem: &[u8]) -> Vec<u8> {
        String::from_utf8_lossy(pem)
            .lines()
            .filter(|line| !line.contains("BEGIN") && !line.contains("END"))
            .collect::<String>()
            .into_bytes()
    }

    /// Decodes a JWT against the fixture public key and asserts the standard
    /// team-token claims/header, regardless of the input key shape.
    fn assert_valid_team_token(jwt: &str, now: i64) {
        let header = decode_header(jwt).unwrap();
        assert_eq!(header.alg, Algorithm::ES256);
        assert_eq!(header.typ.as_deref(), Some("JWT"));
        assert_eq!(header.kid.as_deref(), Some("KEYID4567"));

        let key = DecodingKey::from_ec_pem(PUBLIC_PEM).unwrap();
        let mut validation = Validation::new(Algorithm::ES256);
        validation.validate_exp = false; // fixed `now` is in the past
        validation.set_audience(&[AUDIENCE]);

        let data = decode::<Decoded>(jwt, &key, &validation).unwrap();
        assert_eq!(data.claims.iss, "issuer-123");
        assert_eq!(data.claims.aud, AUDIENCE);
        assert_eq!(data.claims.iat, now);
        assert_eq!(data.claims.exp, now + TTL_SECS);
    }

    #[test]
    fn signs_verifiable_es256_team_token() {
        let now = 1_700_000_000;
        let jwt = sign_team_token("issuer-123", "KEYID4567", PRIVATE_P8, now).unwrap();
        assert_valid_team_token(&jwt, now);
    }

    #[test]
    fn signs_verifiable_token_from_headerless_base64_key() {
        let now = 1_700_000_000;
        let headerless = strip_pem_headers(PRIVATE_P8);
        // Sanity: the derived input really is bare base64, no PEM armor.
        let as_text = String::from_utf8_lossy(&headerless);
        assert!(!as_text.contains("BEGIN"));
        assert!(!as_text.contains('\n'));

        let jwt = sign_team_token("issuer-123", "KEYID4567", &headerless, now).unwrap();
        assert_valid_team_token(&jwt, now);
    }

    #[test]
    fn normalize_passes_full_pem_through_unchanged() {
        // A full PEM must round-trip untouched and stay usable.
        let normalized = normalize_p8_pem(PRIVATE_P8);
        assert_eq!(normalized, PRIVATE_P8.to_vec());
        EncodingKey::from_ec_pem(&normalized).expect("full PEM must remain a valid encoding key");
    }

    #[test]
    fn normalize_rebuilds_pem_from_headerless_base64() {
        let normalized = normalize_p8_pem(&strip_pem_headers(PRIVATE_P8));
        let text = String::from_utf8_lossy(&normalized);
        assert!(text.starts_with(PEM_BEGIN));
        assert!(text.trim_end().ends_with(PEM_END));
        // Re-wrapped body lines never exceed the standard PEM width.
        for line in text.lines().filter(|l| !l.contains("PRIVATE KEY")) {
            assert!(line.len() <= PEM_LINE_WIDTH);
        }
        EncodingKey::from_ec_pem(&normalized).expect("rebuilt PEM must be a valid encoding key");
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
