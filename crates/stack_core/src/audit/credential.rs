//! Which credential a request carries, and when a JWT one expires: read from
//! the built request, never from the authenticators (so the hint describes
//! exactly what went on the wire).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use reqwest::header::AUTHORIZATION;

use super::types::{NetworkAuditCredentialKind, NetworkAuditProvider};

/// A JWT payload larger than this is not decoded (ours are a few hundred
/// bytes; the cap bounds the work on a hostile or broken token).
const MAX_JWT_PAYLOAD_CHARS: usize = 16 * 1024;

/// The credential kind and, for a JWT, its `exp` in Unix milliseconds.
pub(super) fn credential_of(
    request: &reqwest::Request,
    provider: NetworkAuditProvider,
    forced: Option<NetworkAuditCredentialKind>,
) -> (NetworkAuditCredentialKind, Option<i64>) {
    let bearer = bearer_token(request);
    let kind = forced.unwrap_or(match (bearer.is_some(), provider) {
        (true, NetworkAuditProvider::AppleApi | NetworkAuditProvider::AppleWebSession) => {
            NetworkAuditCredentialKind::AscJwt
        }
        (true, _) => NetworkAuditCredentialKind::GoogleAccessToken,
        (false, NetworkAuditProvider::AppleWebSession) => {
            NetworkAuditCredentialKind::AppleSessionCookie
        }
        (false, _) => NetworkAuditCredentialKind::Unauthenticated,
    });
    let expires_at_ms = match kind {
        NetworkAuditCredentialKind::OauthAssertion => request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .and_then(form_assertion)
            .and_then(|assertion| jwt_exp_ms(&assertion)),
        NetworkAuditCredentialKind::AscJwt | NetworkAuditCredentialKind::GoogleAccessToken => {
            bearer.and_then(jwt_exp_ms)
        }
        NetworkAuditCredentialKind::AppleSessionCookie
        | NetworkAuditCredentialKind::Unauthenticated => None,
    };
    (kind, expires_at_ms)
}

/// The token of an `Authorization: Bearer <token>` header.
fn bearer_token(request: &reqwest::Request) -> Option<&str> {
    let value = request.headers().get(AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// The `assertion` field of an `application/x-www-form-urlencoded` body.
fn form_assertion(body: &[u8]) -> Option<String> {
    let body = std::str::from_utf8(body).ok()?;
    body.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "assertion").then(|| {
            percent_encoding::percent_decode_str(&value.replace('+', " "))
                .decode_utf8_lossy()
                .into_owned()
        })
    })
}

/// The `exp` claim of a compact JWT (`header.payload.signature`), in Unix
/// milliseconds. The signature is not verified: this is a display hint about
/// a token the core itself sent. `None` for anything that is not a JWT with
/// an integral `exp`, or whose `exp` in milliseconds overflows.
pub(crate) fn jwt_exp_ms(token: &str) -> Option<i64> {
    let mut parts = token.split('.');
    let (_, payload, _) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || payload.is_empty() || payload.len() > MAX_JWT_PAYLOAD_CHARS {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("exp")?.as_i64()?.checked_mul(1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: &str) -> String {
        format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"K","typ":"JWT"}"#),
            URL_SAFE_NO_PAD.encode(payload.as_bytes())
        )
    }

    #[test]
    fn jwt_exp_is_read_in_milliseconds() {
        assert_eq!(
            jwt_exp_ms(&jwt(
                r#"{"iss":"i","exp":1764000000,"aud":"appstoreconnect-v1"}"#
            )),
            Some(1_764_000_000_000)
        );
        // Padded base64url is accepted too.
        let padded = format!(
            "h.{}.s",
            base64::engine::general_purpose::URL_SAFE.encode(br#"{"exp":1}"#)
        );
        assert_eq!(jwt_exp_ms(&padded), Some(1000));
    }

    #[test]
    fn jwt_exp_is_none_for_anything_else() {
        assert_eq!(jwt_exp_ms("ya29.a0-opaque-google-token"), None);
        assert_eq!(jwt_exp_ms(&jwt(r#"{"iss":"i"}"#)), None);
        assert_eq!(jwt_exp_ms(&jwt(r#"{"exp":"soon"}"#)), None);
        assert_eq!(jwt_exp_ms(&jwt(r#"{"exp":1.5}"#)), None);
        assert_eq!(
            jwt_exp_ms(&jwt(&format!(r#"{{"exp":{}}}"#, i64::MAX))),
            None
        );
        assert_eq!(jwt_exp_ms("a.b.c.d"), None);
        assert_eq!(jwt_exp_ms("a..c"), None);
        assert_eq!(jwt_exp_ms("a.!!!.c"), None);
        assert_eq!(jwt_exp_ms(""), None);
        let huge = format!("a.{}.c", "A".repeat(MAX_JWT_PAYLOAD_CHARS + 1));
        assert_eq!(jwt_exp_ms(&huge), None);
    }

    fn request(
        build: impl FnOnce(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> reqwest::Request {
        build(reqwest::Client::new().get("https://api.example.test/v1/apps"))
            .build()
            .unwrap()
    }

    #[test]
    fn a_bearer_is_an_asc_jwt_for_apple_and_an_access_token_for_google() {
        let token = jwt(r#"{"exp":1764000000}"#);
        let req = request(|b| b.bearer_auth(&token));
        assert_eq!(
            credential_of(&req, NetworkAuditProvider::AppleApi, None),
            (NetworkAuditCredentialKind::AscJwt, Some(1_764_000_000_000))
        );
        let opaque = request(|b| b.bearer_auth("ya29.opaque"));
        assert_eq!(
            credential_of(&opaque, NetworkAuditProvider::GooglePlay, None),
            (NetworkAuditCredentialKind::GoogleAccessToken, None)
        );
        // A lowercase scheme is still a bearer.
        let lower = request(|b| b.header("authorization", "bearer ya29.x"));
        assert_eq!(
            credential_of(&lower, NetworkAuditProvider::GooglePlay, None).0,
            NetworkAuditCredentialKind::GoogleAccessToken
        );
    }

    #[test]
    fn no_bearer_is_a_session_cookie_for_the_web_session_and_nothing_otherwise() {
        let plain = request(|b| b);
        assert_eq!(
            credential_of(&plain, NetworkAuditProvider::AppleWebSession, None),
            (NetworkAuditCredentialKind::AppleSessionCookie, None)
        );
        assert_eq!(
            credential_of(&plain, NetworkAuditProvider::AppleApi, None),
            (NetworkAuditCredentialKind::Unauthenticated, None)
        );
        assert_eq!(
            credential_of(
                &plain,
                NetworkAuditProvider::AppleWebSession,
                Some(NetworkAuditCredentialKind::Unauthenticated)
            ),
            (NetworkAuditCredentialKind::Unauthenticated, None)
        );
    }

    #[test]
    fn an_oauth_assertion_expiry_is_read_from_the_form_body() {
        let assertion = jwt(r#"{"iss":"sa@p.iam.gserviceaccount.com","exp":1764003600}"#);
        let req = reqwest::Client::new()
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .build()
            .unwrap();
        assert_eq!(
            credential_of(
                &req,
                NetworkAuditProvider::GoogleOauth,
                Some(NetworkAuditCredentialKind::OauthAssertion)
            ),
            (
                NetworkAuditCredentialKind::OauthAssertion,
                Some(1_764_003_600_000)
            )
        );
        assert_eq!(form_assertion(b"grant_type=x"), None);
        assert_eq!(
            form_assertion(b"assertion=a%2Eb&x=1"),
            Some("a.b".to_string())
        );
    }
}
