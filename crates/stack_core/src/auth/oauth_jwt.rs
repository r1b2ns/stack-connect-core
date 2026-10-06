//! Google service-account auth: the OAuth2 JWT-bearer grant (RFC 7523).
//!
//! The service account's RSA key signs a short-lived RS256 assertion locally;
//! the assertion is exchanged at Google's token endpoint for an `access_token`,
//! which is cached until shortly before it expires. Service-agnostic on purpose:
//! the caller passes the OAuth scope, so other Google services (e.g. Firebase)
//! can reuse it.

use std::sync::Arc;

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::token::{needs_refresh, unix_now, CachedToken};
use crate::error::StackError;
use crate::http_debug::send_and_read;
use crate::ports::DebugLogger;

/// Google's OAuth2 token endpoint. Also the assertion's `aud`: Google requires
/// the audience to equal the endpoint the assertion is posted to.
pub(crate) const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// RFC 7523 `grant_type` for exchanging a signed JWT for an access token.
const JWT_BEARER_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// Assertion lifetime. Google rejects assertions whose `exp` is more than one
/// hour after `iat`.
const ASSERTION_TTL_SECS: i64 = 3600;
/// `iat` is backdated by this much so a device clock running slightly ahead of
/// Google's does not produce an assertion "issued in the future".
const CLOCK_SKEW_SECS: i64 = 10;
/// Access-token lifetime assumed when the token response omits `expires_in` (or
/// sends a non-positive one). Google's tokens last one hour.
const DEFAULT_EXPIRES_IN_SECS: i64 = 3600;
/// Standard PEM base64 line width.
const PEM_LINE_WIDTH: usize = 64;

/// PKCS#8 PEM label (`-----BEGIN PRIVATE KEY-----`), the shape Google issues.
const LABEL_PKCS8: &str = "PRIVATE KEY";
/// PKCS#1 PEM label (`-----BEGIN RSA PRIVATE KEY-----`), e.g. after an
/// `openssl -traditional` conversion.
const LABEL_PKCS1: &str = "RSA PRIVATE KEY";
/// Password-protected PKCS#8; the core has no passphrase to decrypt it with.
const LABEL_ENCRYPTED: &str = "ENCRYPTED PRIVATE KEY";
const PEM_BEGIN_MARKER: &str = "-----BEGIN ";
const PEM_END_MARKER: &str = "-----END ";
const PEM_DASHES: &str = "-----";
/// Longest PEM label echoed back in an error. A longer label, or one outside
/// `[A-Z0-9 ]`, is most likely a mangled BEGIN line carrying key material, so it
/// is not repeated into a `StackError` the host displays and logs.
const MAX_ECHOED_LABEL_LEN: usize = 40;

/// A parsed Google service-account signing key.
///
/// Deliberately not `Debug`: it holds the RSA private key, and a stray `{:?}`
/// must not be able to print it.
pub(crate) struct ServiceAccountKey {
    client_email: String,
    private_key_id: String,
    encoding_key: EncodingKey,
}

impl ServiceAccountKey {
    /// Parses the three service-account fields the host extracted from the JSON
    /// key file (`client_email`, `private_key_id`, `private_key`).
    ///
    /// Inputs are trimmed. `private_key` is normalized by [`pem_candidates`]
    /// first, because users paste it in many shapes (straight out of the JSON
    /// with escapes, on one line, without armor, …). The first candidate that
    /// both decodes as an RSA key **and** produces a test signature wins: the
    /// decoder only checks PEM/ASN.1 structure, while ring validates the RSA key
    /// itself — including its 2048-bit minimum — only when it signs. The test
    /// signature makes a bad key fail here, at connect, rather than on the first
    /// request.
    ///
    /// # Errors
    /// [`StackError::InvalidCredentials`] if the email is blank or has no `@`,
    /// the key ID is blank, or no candidate yields a usable RSA private key. The
    /// messages never echo key material.
    pub(crate) fn parse(
        client_email: &str,
        private_key_id: &str,
        private_key: &str,
    ) -> Result<Self, StackError> {
        let client_email = client_email.trim();
        if client_email.is_empty() {
            return Err(StackError::invalid_credentials(
                "service account email is empty",
            ));
        }
        if !client_email.contains('@') {
            return Err(StackError::invalid_credentials(
                "service account email must be an email address (the `client_email` \
                 from the JSON key file)",
            ));
        }
        let private_key_id = private_key_id.trim();
        if private_key_id.is_empty() {
            return Err(StackError::invalid_credentials("private key ID is empty"));
        }

        for pem in pem_candidates(private_key)? {
            let Ok(encoding_key) = EncodingKey::from_rsa_pem(pem.as_bytes()) else {
                continue;
            };
            let key = Self {
                client_email: client_email.to_string(),
                private_key_id: private_key_id.to_string(),
                encoding_key,
            };
            if sign_assertion(&key, "", "", 0).is_ok() {
                return Ok(key);
            }
        }

        Err(StackError::invalid_credentials(
            "private key is not a usable RSA private key: expected the PKCS#8 or \
             PKCS#1 `private_key` from a Google service account JSON key (RSA, at \
             least 2048 bits)",
        ))
    }

    /// The service account's email (`iss` of every assertion). Not secret.
    pub(crate) fn client_email(&self) -> &str {
        &self.client_email
    }
}

/// RFC 7523 assertion claims. There is no `sub`: the service account acts as
/// itself, not on behalf of a user (that would need domain-wide delegation).
#[derive(Serialize)]
struct AssertionClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

/// Signs the RS256 JWT-bearer assertion for `scope` (space-separated OAuth
/// scopes), addressed to `audience` (the token endpoint).
///
/// Header: `alg = RS256`, `typ = JWT` (set by [`Header::new`]),
/// `kid = private_key_id` so Google can pick the matching public key. `now` is
/// the Unix time in seconds, injected so tests are deterministic;
/// `iat = now - 10` (see [`CLOCK_SKEW_SECS`]) and `exp = iat + 3600`.
///
/// # Errors
/// [`StackError::Auth`] if signing fails (ring rejects the key).
fn sign_assertion(
    key: &ServiceAccountKey,
    scope: &str,
    audience: &str,
    now: i64,
) -> Result<String, StackError> {
    let iat = now.saturating_sub(CLOCK_SKEW_SECS);
    let claims = AssertionClaims {
        iss: &key.client_email,
        scope,
        aud: audience,
        iat,
        exp: iat.saturating_add(ASSERTION_TTL_SECS),
    };

    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(key.private_key_id.clone());

    encode(&header, &claims, &key.encoding_key)
        .map_err(|e| StackError::auth(format!("signing the Google OAuth assertion failed: {e}")))
}

/// Normalizes a pasted `private_key` into the canonical PEM block(s) to try, in
/// order.
///
/// 1. Trim, drop one trailing `,` and one pair of surrounding `"` — the value
///    as copied out of the JSON key file.
/// 2. Reject the whole JSON key file (input starting with `{`).
/// 3. Decode JSON escapes when a `\` is present (see [`unescape_json_string`]).
/// 4. Armored input (`-----BEGIN …-----`) is always rebuilt by [`wrap_pem`],
///    never passed through: one-line and blank-line pastes are common and
///    pem 3.0.6 misreads both. Only the PKCS#8 and PKCS#1 private-key labels are
///    accepted.
/// 5. A bare base64 body yields two candidates: PKCS#8 first (what Google
///    issues), then PKCS#1. A PKCS#1 body has no algorithm OID, so it fails the
///    PKCS#8 classification and the second candidate catches it.
///
/// Whitespace (any `char::is_whitespace`, plus U+FEFF and U+200B) is removed
/// from the base64 body.
///
/// # Errors
/// [`StackError::InvalidCredentials`] for a whole JSON file, a malformed or
/// unsupported PEM block, or an empty / non-ASCII body.
fn pem_candidates(raw: &str) -> Result<Vec<String>, StackError> {
    let text = strip_json_value_wrapping(raw);
    if text.starts_with('{') {
        return Err(StackError::invalid_credentials(
            "private key must be the `private_key` value from the JSON key file, \
             not the whole file",
        ));
    }

    let text = unescape_json_string(text);
    match text.find(PEM_BEGIN_MARKER) {
        Some(begin) => armored_candidates(&text[begin..]),
        None => {
            let body = clean_body(&text)?;
            Ok(vec![
                wrap_pem(LABEL_PKCS8, &body),
                wrap_pem(LABEL_PKCS1, &body),
            ])
        }
    }
}

/// Step 1 of [`pem_candidates`]: strips what surrounds a value copied out of a
/// JSON document — outer whitespace / invisible characters, one trailing comma,
/// and one pair of double quotes.
fn strip_json_value_wrapping(raw: &str) -> &str {
    let trimmed = raw.trim_matches(|c: char| c.is_whitespace() || is_invisible(c));
    let trimmed = trimmed.strip_suffix(',').unwrap_or(trimmed).trim_end();
    trimmed
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(trimmed)
}

/// Step 3 of [`pem_candidates`]: undoes JSON string escaping.
///
/// A value copied out of the JSON key file keeps its escapes: `\n` line breaks,
/// `\/` (some serializers escape `/`) and `\u003d` (HTML-safe serializers escape
/// `=`). Decoding the text as a JSON string handles every escape at once; raw
/// control characters (a paste that mixes real line breaks with escapes) become
/// spaces first, since JSON forbids them inside a string and whitespace is
/// insignificant in a PEM body anyway.
///
/// Whatever backslashes survive — double-escaped input (`\\n`) decodes to a
/// literal `\n`, or text JSON cannot decode at all — are replaced by hand. That
/// is safe because base64 and PEM armor never contain a backslash: any `\` is an
/// escaping artifact, not key data.
fn unescape_json_string(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_string();
    }

    let json_safe: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let decoded = serde_json::from_str::<String>(&format!("\"{json_safe}\""))
        .unwrap_or_else(|_| text.to_string());
    if !decoded.contains('\\') {
        return decoded;
    }

    decoded
        .replace("\\n", " ")
        .replace("\\r", " ")
        .replace("\\t", " ")
        .replace("\\/", "/")
        .replace('\\', "")
}

/// Step 4 of [`pem_candidates`]: `text` starts at `-----BEGIN `. Splits it into
/// the label (up to the next `-----`) and the body (up to `-----END `), checks
/// the label, and rebuilds the block canonically. The END label is not checked:
/// the block is rebuilt with the BEGIN label either way.
fn armored_candidates(text: &str) -> Result<Vec<String>, StackError> {
    let malformed = || {
        StackError::invalid_credentials(
            "private key is a malformed PEM block (missing BEGIN/END line)",
        )
    };

    let after_begin = text.get(PEM_BEGIN_MARKER.len()..).ok_or_else(malformed)?;
    let label_end = after_begin.find(PEM_DASHES).ok_or_else(malformed)?;
    let label = after_begin[..label_end].trim();
    let after_label = &after_begin[label_end + PEM_DASHES.len()..];
    let body_end = after_label.find(PEM_END_MARKER).ok_or_else(malformed)?;

    match label {
        LABEL_PKCS8 | LABEL_PKCS1 => {
            let body = clean_body(&after_label[..body_end])?;
            Ok(vec![wrap_pem(label, &body)])
        }
        LABEL_ENCRYPTED => Err(StackError::invalid_credentials(
            "encrypted private keys are not supported; use the unencrypted \
             `private_key` from the JSON key file",
        )),
        other if is_echoable_label(other) => Err(StackError::invalid_credentials(format!(
            "expected an RSA private key, got a '{other}' PEM block"
        ))),
        _ => Err(StackError::invalid_credentials(
            "private key has an unrecognized PEM label",
        )),
    }
}

/// Removes every whitespace / invisible character from a base64 body.
///
/// # Errors
/// [`StackError::InvalidCredentials`] if nothing is left or the remainder is not
/// ASCII (base64 is ASCII; anything else is not key data, and an ASCII body is
/// what lets [`wrap_pem`] split it on byte boundaries).
fn clean_body(body: &str) -> Result<String, StackError> {
    let body: String = body
        .chars()
        .filter(|&c| !c.is_whitespace() && !is_invisible(c))
        .collect();
    if body.is_empty() {
        return Err(StackError::invalid_credentials("private key is empty"));
    }
    if !body.is_ascii() {
        return Err(StackError::invalid_credentials(
            "private key contains characters that are not base64",
        ));
    }
    Ok(body)
}

/// Zero-width characters that `char::is_whitespace` misses but rich-text pastes
/// carry: the byte-order mark (U+FEFF) and the zero-width space (U+200B).
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{FEFF}' | '\u{200B}')
}

/// Whether an unknown PEM label is safe to repeat in an error: 1 to
/// [`MAX_ECHOED_LABEL_LEN`] characters of `[A-Z0-9 ]`, like every real label.
fn is_echoable_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= MAX_ECHOED_LABEL_LEN
        && label
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == ' ')
}

/// Rebuilds a canonical PEM block: the BEGIN line, `body` split into
/// [`PEM_LINE_WIDTH`]-char lines joined by `\n`, then exactly one `\n` before the
/// END line.
///
/// Exactly one matters: pem 3.0.6 treats the first blank line (`\n\n`) inside a
/// block as the end of an RFC 1421 header section, so a blank line before
/// `-----END` — e.g. from terminating every line with `\n` when the body length
/// is a multiple of 64 — would make it parse the body as headers and reject the
/// key. `body` is non-empty ASCII (see [`clean_body`]), so splitting on byte
/// boundaries never splits a character.
fn wrap_pem(label: &str, body: &str) -> String {
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(PEM_LINE_WIDTH)
        .filter_map(|chunk| std::str::from_utf8(chunk).ok())
        .collect();
    format!(
        "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
        lines.join("\n")
    )
}

/// The token endpoint's success body. Only the fields the core reads; Google
/// also sends `token_type` and `scope`.
#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// The token endpoint's OAuth2 error body (RFC 6749 §5.2).
#[derive(Deserialize)]
struct OAuthErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Maps a token-endpoint response to `(access_token, expires_in_secs)`.
///
/// # Errors
/// - 2xx with a malformed body or a blank `access_token` → [`StackError::Decode`].
/// - 400 / 401 / 403 → [`StackError::Auth`]: the credentials were rejected
///   (`invalid_grant` gets a hint listing the usual causes).
/// - Anything else (429, 5xx, …) → [`StackError::Http`]: not a credential
///   problem, so the host can tell the user to retry.
fn map_token_response(
    status: reqwest::StatusCode,
    body: &str,
) -> Result<(String, i64), StackError> {
    if status.is_success() {
        let response: TokenResponse = serde_json::from_str(body)
            .map_err(|e| StackError::decode(format!("token response: {e}")))?;
        if response.access_token.trim().is_empty() {
            return Err(StackError::decode("token response has no access_token"));
        }
        let expires_in = response
            .expires_in
            .filter(|&secs| secs > 0)
            .unwrap_or(DEFAULT_EXPIRES_IN_SECS);
        return Ok((response.access_token, expires_in));
    }

    match status.as_u16() {
        400 | 401 | 403 => Err(StackError::auth(token_rejection_message(status, body))),
        other => Err(StackError::Http {
            status: other,
            message: body.to_string(),
        }),
    }
}

/// The `Auth` message for a rejected token exchange: Google's `error` code and
/// `error_description` when the body is an OAuth error, the raw body otherwise.
fn token_rejection_message(status: reqwest::StatusCode, body: &str) -> String {
    let parsed = serde_json::from_str::<OAuthErrorBody>(body).ok();
    let non_blank = |s: &Option<String>| -> Option<String> {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let Some(error) = parsed.as_ref().and_then(|e| non_blank(&e.error)) else {
        return format!(
            "Google OAuth token exchange failed ({status}): {}",
            body.trim()
        );
    };

    let mut message = match parsed
        .as_ref()
        .and_then(|e| non_blank(&e.error_description))
    {
        Some(description) => {
            format!("Google OAuth token exchange failed ({error}): {description}")
        }
        None => format!("Google OAuth token exchange failed ({error})"),
    };
    if error == "invalid_grant" {
        // Google's descriptions usually end with a period; avoid doubling it.
        message.truncate(message.trim_end_matches('.').len());
        message.push_str(
            ". Check that the private key ID matches this key, that the key has not \
             been deleted or disabled, and that the device clock is correct; a key \
             created in the last few minutes may not be active yet.",
        );
    }
    message
}

/// Exchanges signed service-account assertions for Google access tokens and
/// caches them until shortly before they expire.
///
/// The cache is a `tokio::sync::Mutex` held across the token exchange, so
/// concurrent callers share one request instead of racing several. Because the
/// lock spans a network call, the injected `http` client must carry timeouts
/// (the Google Play provider builds one with connect and request timeouts);
/// otherwise one stalled exchange would block every caller.
pub(crate) struct ServiceAccountAuthenticator {
    key: ServiceAccountKey,
    /// Space-separated OAuth scopes requested in every assertion.
    scope: &'static str,
    /// Where assertions are posted; also their `aud`. Injectable for tests.
    token_url: String,
    http: reqwest::Client,
    /// Optional HTTP tracing sink for the token exchange (see
    /// [`crate::http_debug::send_and_read`]).
    debug_logger: Option<Arc<dyn DebugLogger>>,
    cache: Mutex<Option<CachedToken>>,
}

impl ServiceAccountAuthenticator {
    /// An authenticator for `scope` against Google's token endpoint, sending
    /// through the shared `http` client.
    pub(crate) fn new(key: ServiceAccountKey, scope: &'static str, http: reqwest::Client) -> Self {
        Self::with_token_url(key, scope, http, GOOGLE_TOKEN_URL.to_string())
    }

    /// Same as [`Self::new`] but against `token_url`, so tests can point it at a
    /// mock server.
    pub(crate) fn with_token_url(
        key: ServiceAccountKey,
        scope: &'static str,
        http: reqwest::Client,
        token_url: String,
    ) -> Self {
        Self {
            key,
            scope,
            token_url,
            http,
            debug_logger: None,
            cache: Mutex::new(None),
        }
    }

    /// Attaches an optional HTTP tracing sink for the token exchange.
    /// Builder-style; `None` is a no-op (no logging).
    pub(crate) fn with_debug_logger(mut self, logger: Option<Arc<dyn DebugLogger>>) -> Self {
        self.debug_logger = logger;
        self
    }

    /// The service account's email, for error messages.
    pub(crate) fn client_email(&self) -> &str {
        self.key.client_email()
    }

    /// Returns a valid access token, exchanging a freshly signed assertion only
    /// when the cache is empty or within
    /// [`REFRESH_MARGIN_SECS`](super::token::REFRESH_MARGIN_SECS) of expiry.
    /// A failed exchange is not cached, so the next call retries.
    ///
    /// # Errors
    /// The [`map_token_response`] mapping (`Auth`, `Http`, `Decode`),
    /// [`StackError::Network`] on transport failure or timeout, or
    /// [`StackError::Auth`] if signing fails.
    pub(crate) async fn bearer_token(&self) -> Result<String, StackError> {
        let mut cache = self.cache.lock().await;
        // Read the clock only once the lock is held: a caller that queued behind
        // a slow exchange would otherwise sign with a stale `iat` and judge the
        // cache against a stale `now`.
        let now = unix_now();
        if let Some(cached) = cache.as_ref() {
            if !needs_refresh(cached.expires_at, now) {
                return Ok(cached.token.clone());
            }
        }

        let assertion = sign_assertion(&self.key, self.scope, &self.token_url, now)?;
        let request = self.http.post(&self.token_url).form(&[
            ("grant_type", JWT_BEARER_GRANT_TYPE),
            ("assertion", assertion.as_str()),
        ]);
        let (status, body) = send_and_read(request, self.debug_logger.as_deref()).await?;
        let (token, expires_in) = map_token_response(status, &body)?;

        // `now` predates the exchange, so the cached expiry errs early.
        *cache = Some(CachedToken {
            token: token.clone(),
            expires_at: now.saturating_add(expires_in),
        });
        Ok(token)
    }

    /// Drops the cached token so the next [`Self::bearer_token`] exchanges a new
    /// one. Google decides token validity server-side (a revoked token, a
    /// disabled service account, a wall-clock jump), so a cached token can die
    /// before its `expires_in`; callers invalidate on a 401.
    pub(crate) async fn invalidate(&self) {
        *self.cache.lock().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PKCS8_PEM: &str = include_str!("../../tests/fixtures/test_rsa_private_pkcs8.pem");
    const PKCS1_PEM: &str = include_str!("../../tests/fixtures/test_rsa_private_pkcs1.pem");
    const PUBLIC_PEM: &str = include_str!("../../tests/fixtures/test_rsa_public.pem");
    const RSA_1024_PEM: &str = include_str!("../../tests/fixtures/test_rsa_1024_private.pem");
    const EC_PEM: &str = include_str!("../../tests/fixtures/test_ec_private.p8");

    const EMAIL: &str = "play-bot@my-project.iam.gserviceaccount.com";
    const KID: &str = "0123456789abcdef0123456789abcdef01234567";
    /// A local scope: these tests exercise the generic auth module and must not
    /// depend on any provider's scopes.
    const TEST_SCOPE: &str =
        "https://www.googleapis.com/auth/test.read https://www.googleapis.com/auth/test.write";
    const AUD: &str = "https://oauth.example.test/token";
    const NOW: i64 = 1_700_000_000;

    #[derive(serde::Deserialize)]
    struct Decoded {
        iss: String,
        scope: String,
        aud: String,
        iat: i64,
        exp: i64,
    }

    fn key_from(pem: &str) -> ServiceAccountKey {
        ServiceAccountKey::parse(EMAIL, KID, pem).unwrap()
    }

    /// The `InvalidCredentials` message `parse` fails with. `ServiceAccountKey`
    /// is not `Debug`, so `unwrap_err` is unavailable.
    fn parse_err(email: &str, kid: &str, pem: &str) -> String {
        match ServiceAccountKey::parse(email, kid, pem) {
            Err(StackError::InvalidCredentials { message }) => message,
            Err(other) => panic!("expected InvalidCredentials, got {other:?}"),
            Ok(_) => panic!("expected InvalidCredentials, got a parsed key"),
        }
    }

    /// Asserts `pem` parses into a key whose assertion verifies against the
    /// fixture public key.
    fn assert_parses(pem: &str) {
        let key = key_from(pem);
        let jwt = sign_assertion(&key, TEST_SCOPE, AUD, NOW).unwrap();
        verify(&jwt, AUD);
    }

    /// Verifies `jwt` against the fixture public key and returns its claims.
    fn verify(jwt: &str, audience: &str) -> Decoded {
        let key = DecodingKey::from_rsa_pem(PUBLIC_PEM.as_bytes()).unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_exp = false; // fixed `now` is in the past
        validation.set_audience(&[audience]);
        decode::<Decoded>(jwt, &key, &validation).unwrap().claims
    }

    /// The bare base64 body of a PEM: armor lines and newlines removed.
    fn bare_body(pem: &str) -> String {
        pem.lines()
            .filter(|line| !line.starts_with("-----"))
            .collect::<String>()
    }

    /// `pem` as it appears inside the JSON key file: quoted, `\n`-escaped.
    fn json_escaped(pem: &str) -> String {
        serde_json::to_string(pem).unwrap()
    }

    fn authenticator(server: &MockServer) -> ServiceAccountAuthenticator {
        ServiceAccountAuthenticator::with_token_url(
            key_from(PKCS8_PEM),
            TEST_SCOPE,
            reqwest::Client::new(),
            format!("{}/token", server.uri()),
        )
    }

    fn token_json(token: &str, expires_in: i64) -> serde_json::Value {
        serde_json::json!({
            "access_token": token,
            "expires_in": expires_in,
            "token_type": "Bearer"
        })
    }

    /// The `assertion` form field of a captured token request. The JWT's
    /// base64url alphabet and `.` are not percent-encoded by the form encoder.
    fn assertion_from(body: &[u8]) -> String {
        std::str::from_utf8(body)
            .unwrap()
            .split('&')
            .find_map(|pair| pair.strip_prefix("assertion="))
            .expect("an assertion field")
            .to_string()
    }

    #[derive(Default)]
    struct CapturingLogger {
        messages: StdMutex<Vec<String>>,
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

    // -----------------------------------------------------------------------
    // Signing
    // -----------------------------------------------------------------------

    #[test]
    fn signs_verifiable_rs256_assertion_from_pkcs8() {
        let jwt = sign_assertion(&key_from(PKCS8_PEM), TEST_SCOPE, AUD, NOW).unwrap();

        let header = decode_header(&jwt).unwrap();
        assert_eq!(header.alg, Algorithm::RS256);
        assert_eq!(header.typ.as_deref(), Some("JWT"));
        assert_eq!(header.kid.as_deref(), Some(KID));

        let claims = verify(&jwt, AUD);
        assert_eq!(claims.iss, EMAIL);
        assert_eq!(claims.scope, TEST_SCOPE);
        assert_eq!(claims.aud, AUD);
        // Literals, not the constants, so a changed skew or TTL fails here.
        assert_eq!(claims.iat, NOW - 10);
        assert_eq!(claims.exp, claims.iat + 3600);
    }

    #[test]
    fn assertion_carries_exactly_the_rfc7523_claims() {
        let jwt = sign_assertion(&key_from(PKCS8_PEM), TEST_SCOPE, AUD, NOW).unwrap();
        let key = DecodingKey::from_rsa_pem(PUBLIC_PEM.as_bytes()).unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_exp = false;
        validation.set_audience(&[AUD]);
        let claims = decode::<serde_json::Map<String, serde_json::Value>>(&jwt, &key, &validation)
            .unwrap()
            .claims;

        // No `sub`: the service account acts as itself.
        let keys: BTreeSet<&str> = claims.keys().map(String::as_str).collect();
        assert_eq!(keys, BTreeSet::from(["aud", "exp", "iat", "iss", "scope"]));
    }

    #[test]
    fn signs_verifiable_assertion_from_pkcs1() {
        let jwt = sign_assertion(&key_from(PKCS1_PEM), TEST_SCOPE, AUD, NOW).unwrap();
        assert_eq!(decode_header(&jwt).unwrap().kid.as_deref(), Some(KID));
        assert_eq!(verify(&jwt, AUD).iss, EMAIL);
    }

    #[test]
    fn assertion_is_deterministic_for_fixed_now() {
        // RS256 is RSASSA-PKCS1-v1_5, which has no randomness.
        let key = key_from(PKCS8_PEM);
        assert_eq!(
            sign_assertion(&key, TEST_SCOPE, AUD, NOW).unwrap(),
            sign_assertion(&key, TEST_SCOPE, AUD, NOW).unwrap()
        );
    }

    // -----------------------------------------------------------------------
    // Parsing: accepted shapes
    // -----------------------------------------------------------------------

    #[test]
    fn parse_accepts_full_pkcs8_and_pkcs1_pem() {
        assert_parses(PKCS8_PEM);
        assert_parses(PKCS1_PEM);
    }

    #[test]
    fn parse_accepts_json_escaped_key_with_quotes_and_trailing_comma() {
        let pasted = format!("  {},\n", json_escaped(PKCS8_PEM));
        // Sanity: the input really carries literal `\n` escapes, not newlines.
        assert!(pasted.contains("-----\\nMII"));
        assert!(pasted.trim_start().starts_with('"'));
        assert_parses(&pasted);
    }

    #[test]
    fn parse_accepts_escaped_slashes() {
        let pasted = json_escaped(PKCS8_PEM).replace('/', "\\/");
        assert!(pasted.contains("\\/"));
        assert_parses(&pasted);
    }

    #[test]
    fn parse_accepts_unicode_escaped_padding() {
        // HTML-safe JSON serializers (e.g. Gson's default) write `=` as `\u003d`.
        let pasted = json_escaped(PKCS8_PEM).replace('=', "\\u003d");
        assert!(pasted.contains("\\u003d"));
        assert_parses(&pasted);
    }

    #[test]
    fn parse_accepts_double_escaped_newlines() {
        // Escaped twice (`\\n`): one JSON decode still leaves literal `\n`.
        let pasted = json_escaped(PKCS8_PEM).replace("\\n", "\\\\n");
        assert!(pasted.contains("\\\\n"));
        assert_parses(&pasted);
    }

    #[test]
    fn parse_accepts_escapes_mixed_with_real_newlines() {
        // Real line breaks next to literal escapes: JSON forbids raw control
        // characters, so they must not break the decode.
        let pasted = PKCS8_PEM.replace('\n', "\\n\n");
        assert_parses(&pasted);
    }

    #[test]
    fn parse_accepts_crlf_and_indented_pem() {
        let pasted: String = PKCS8_PEM
            .lines()
            .map(|line| format!("    {line}\r\n"))
            .collect();
        assert_parses(&pasted);
    }

    #[test]
    fn parse_accepts_single_line_armored_pem() {
        // Newlines collapsed to spaces, and removed entirely.
        assert_parses(&PKCS8_PEM.replace('\n', " "));
        assert_parses(&PKCS8_PEM.replace('\n', ""));
    }

    #[test]
    fn parse_accepts_pem_with_blank_lines() {
        // A blank line right after BEGIN (and between body lines) would make
        // pem 3.0.6 read the body as RFC 1421 headers if passed through.
        let pasted = PKCS8_PEM
            .replacen("-----\n", "-----\n\n", 1)
            .replace('\n', "\n\n");
        assert!(pasted.contains("\n\n"));
        assert_parses(&pasted);
    }

    #[test]
    fn parse_accepts_invisible_and_unicode_whitespace() {
        // BOM, zero-width spaces and NBSPs from rich-text pastes.
        let pasted = format!(
            "\u{FEFF}{}",
            PKCS8_PEM
                .replacen("MII", "MII\u{200B}", 1)
                .replace('\n', "\u{00A0}\n")
        );
        assert_parses(&pasted);
    }

    #[test]
    fn parse_accepts_bare_base64_pkcs8_body() {
        let body = bare_body(PKCS8_PEM);
        assert!(!body.contains("BEGIN") && !body.contains('\n'));
        assert_parses(&body);
    }

    #[test]
    fn parse_accepts_bare_base64_pkcs1_body() {
        // Falls through the PKCS#8 candidate to the PKCS#1 one.
        assert_parses(&bare_body(PKCS1_PEM));
    }

    #[test]
    fn parse_trims_email_and_key_id() {
        let key =
            ServiceAccountKey::parse(&format!("  {EMAIL}\n"), &format!("\t{KID} "), PKCS8_PEM)
                .unwrap();
        assert_eq!(key.client_email(), EMAIL);
        let jwt = sign_assertion(&key, TEST_SCOPE, AUD, NOW).unwrap();
        assert_eq!(decode_header(&jwt).unwrap().kid.as_deref(), Some(KID));
    }

    // -----------------------------------------------------------------------
    // Parsing: rejected inputs
    // -----------------------------------------------------------------------

    #[test]
    fn parse_rejects_blank_email() {
        assert!(parse_err("   ", KID, PKCS8_PEM).contains("email"));
    }

    #[test]
    fn parse_rejects_email_without_at() {
        assert!(parse_err("play-bot", KID, PKCS8_PEM).contains("email"));
    }

    #[test]
    fn parse_rejects_blank_key_id() {
        assert!(parse_err(EMAIL, " \n", PKCS8_PEM).contains("private key ID"));
    }

    #[test]
    fn parse_rejects_blank_key() {
        assert!(parse_err(EMAIL, KID, "  \n ").contains("empty"));
        assert!(parse_err(EMAIL, KID, "\"\",").contains("empty"));
    }

    #[test]
    fn parse_rejects_garbage_key() {
        let message = parse_err(EMAIL, KID, "not a key");
        assert!(message.contains("not a usable RSA private key"));
    }

    #[test]
    fn parse_rejects_ec_key() {
        // A valid PKCS#8 key, but EC: `from_rsa_pem` refuses it.
        assert!(parse_err(EMAIL, KID, EC_PEM).contains("not a usable RSA private key"));
    }

    #[test]
    fn parse_rejects_public_key() {
        let message = parse_err(EMAIL, KID, PUBLIC_PEM);
        assert!(message.contains("'PUBLIC KEY'"));
        // A bare public-key body decodes under the PKCS#8 label but cannot sign.
        assert!(
            parse_err(EMAIL, KID, &bare_body(PUBLIC_PEM)).contains("not a usable RSA private key")
        );
    }

    #[test]
    fn parse_rejects_encrypted_private_key() {
        let encrypted = format!(
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\n{}\n-----END ENCRYPTED PRIVATE KEY-----\n",
            bare_body(PKCS8_PEM)
        );
        assert!(parse_err(EMAIL, KID, &encrypted).contains("encrypted"));
    }

    #[test]
    fn parse_rejects_whole_json_file() {
        let file = serde_json::json!({
            "type": "service_account",
            "client_email": EMAIL,
            "private_key_id": KID,
            "private_key": PKCS8_PEM,
        })
        .to_string();
        assert!(parse_err(EMAIL, KID, &file).contains("not the whole file"));
    }

    #[test]
    fn parse_rejects_non_ascii_body() {
        let pasted = PKCS8_PEM.replacen("MII", "MIIé", 1);
        assert!(parse_err(EMAIL, KID, &pasted).contains("not base64"));
    }

    #[test]
    fn parse_rejects_malformed_armor() {
        let truncated = PKCS8_PEM.replace("-----END PRIVATE KEY-----", "");
        assert!(parse_err(EMAIL, KID, &truncated).contains("malformed PEM"));
        assert!(parse_err(EMAIL, KID, "-----BEGIN PRIVATE KEY").contains("malformed PEM"));
    }

    #[test]
    fn parse_echoes_only_plausible_labels() {
        let ec = PKCS8_PEM.replace("PRIVATE KEY", "EC PRIVATE KEY");
        assert!(parse_err(EMAIL, KID, &ec).contains("'EC PRIVATE KEY'"));

        // A mangled BEGIN line carrying key material must not be repeated.
        let mangled =
            PKCS8_PEM.replacen("PRIVATE KEY-----\n", "PRIVATE KEY MIIEvAIBADANBg-----\n", 1);
        let message = parse_err(EMAIL, KID, &mangled);
        assert!(message.contains("unrecognized PEM label"));
        assert!(!message.contains("MIIEvAIBADANBg"));

        let too_long = format!(
            "-----BEGIN {}-----\nAAAA\n-----END X-----",
            "A".repeat(MAX_ECHOED_LABEL_LEN + 1)
        );
        assert!(parse_err(EMAIL, KID, &too_long).contains("unrecognized PEM label"));
    }

    #[test]
    fn parse_rejects_1024_bit_rsa_key() {
        // `from_rsa_pem` decodes it; only the test signature (ring's 2048-bit
        // minimum) catches it.
        assert!(EncodingKey::from_rsa_pem(RSA_1024_PEM.as_bytes()).is_ok());
        let message = parse_err(EMAIL, KID, RSA_1024_PEM);
        assert!(message.contains("at least 2048 bits"));
    }

    #[test]
    fn parse_error_never_echoes_key_material() {
        // A real key with its last line corrupted: decodes as base64 garbage.
        let corrupted = PKCS8_PEM.replace("hX+79aEsEh", "hX+79a!!Eh");
        let message = parse_err(EMAIL, KID, &corrupted);
        assert!(!message.contains("MII"));
        assert!(!message.contains("hX+79a"));
    }

    // -----------------------------------------------------------------------
    // wrap_pem
    // -----------------------------------------------------------------------

    #[test]
    fn wrap_pem_lines_at_most_64() {
        let wrapped = wrap_pem(LABEL_PKCS8, &bare_body(PKCS8_PEM));
        assert!(wrapped.starts_with("-----BEGIN PRIVATE KEY-----\n"));
        assert!(wrapped.ends_with("\n-----END PRIVATE KEY-----\n"));
        for line in wrapped.lines() {
            assert!(line.len() <= 64, "line too long: {}", line.len());
        }
        assert!(EncodingKey::from_rsa_pem(wrapped.as_bytes()).is_ok());
    }

    #[test]
    fn wrap_pem_body_multiple_of_64_has_no_blank_line() {
        // Exactly two full lines: exactly one `\n` before END, never `\n\n`.
        let body = "A".repeat(128);
        let line = "A".repeat(64);
        assert_eq!(
            wrap_pem(LABEL_PKCS1, &body),
            format!(
                "-----BEGIN RSA PRIVATE KEY-----\n{line}\n{line}\n-----END RSA PRIVATE KEY-----\n"
            )
        );
        for len in [1, 63, 64, 65, 192] {
            assert!(
                !wrap_pem(LABEL_PKCS8, &"A".repeat(len)).contains("\n\n"),
                "len {len}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // map_token_response
    // -----------------------------------------------------------------------

    #[test]
    fn token_response_maps_success_and_defaults() {
        let ok = reqwest::StatusCode::OK;
        assert_eq!(
            map_token_response(ok, r#"{"access_token":"t","expires_in":120}"#).unwrap(),
            ("t".to_string(), 120)
        );
        // Missing or non-positive `expires_in` → one hour (literal on purpose).
        assert_eq!(
            map_token_response(ok, r#"{"access_token":"t"}"#).unwrap().1,
            3600
        );
        assert_eq!(
            map_token_response(ok, r#"{"access_token":"t","expires_in":0}"#)
                .unwrap()
                .1,
            3600
        );
        assert_eq!(
            map_token_response(ok, r#"{"access_token":"t","expires_in":-5}"#)
                .unwrap()
                .1,
            3600
        );
    }

    #[test]
    fn token_response_credential_statuses_map_to_auth() {
        for status in [400, 401, 403] {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            let err = map_token_response(
                status,
                r#"{"error":"unauthorized_client","error_description":"Client is unauthorized."}"#,
            )
            .unwrap_err();
            match err {
                StackError::Auth { message } => {
                    assert!(message.contains("(unauthorized_client): Client is unauthorized."));
                    // The invalid_grant hint is specific to invalid_grant.
                    assert!(!message.contains("private key ID"));
                }
                other => panic!("expected Auth for {status}, got {other:?}"),
            }
        }
    }

    #[test]
    fn token_response_non_oauth_error_body_uses_raw_text() {
        let err = map_token_response(reqwest::StatusCode::BAD_REQUEST, "  upstream says no \n")
            .unwrap_err();
        match err {
            StackError::Auth { message } => {
                assert!(message.contains("(400 Bad Request): upstream says no"));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Token endpoint (wiremock)
    // -----------------------------------------------------------------------

    #[test]
    fn new_targets_google_token_endpoint() {
        let auth = ServiceAccountAuthenticator::new(
            key_from(PKCS8_PEM),
            TEST_SCOPE,
            reqwest::Client::new(),
        );
        assert_eq!(auth.token_url, "https://oauth2.googleapis.com/token");
        assert_eq!(auth.client_email(), EMAIL);
    }

    #[tokio::test]
    async fn bearer_token_posts_jwt_bearer_form() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer",
            ))
            .and(body_string_contains("&assertion="))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json("tok-1", 3600)))
            .expect(1)
            .mount(&server)
            .await;

        let token = authenticator(&server).bearer_token().await.unwrap();
        assert_eq!(token, "tok-1");

        // The posted assertion is addressed to the endpoint it was posted to.
        let requests = server.received_requests().await.unwrap();
        let audience = format!("{}/token", server.uri());
        let claims = verify(&assertion_from(&requests[0].body), &audience);
        assert_eq!(claims.aud, audience);
        assert_eq!(claims.iss, EMAIL);
        assert_eq!(claims.scope, TEST_SCOPE);
        assert_eq!(claims.exp - claims.iat, 3600);
    }

    #[tokio::test]
    async fn bearer_token_cached_across_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json("tok-1", 3600)))
            .expect(1)
            .mount(&server)
            .await;

        let auth = authenticator(&server);
        assert_eq!(auth.bearer_token().await.unwrap(), "tok-1");
        assert_eq!(auth.bearer_token().await.unwrap(), "tok-1");
    }

    #[tokio::test]
    async fn bearer_token_refreshes_inside_margin() {
        let server = MockServer::start().await;
        // 30 s of validity is already inside the 60 s refresh margin.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json("tok-1", 30)))
            .expect(2)
            .mount(&server)
            .await;

        let auth = authenticator(&server);
        auth.bearer_token().await.unwrap();
        auth.bearer_token().await.unwrap();
    }

    #[tokio::test]
    async fn missing_expires_in_defaults_to_3600() {
        let server = MockServer::start().await;
        // Were the default inside the refresh margin, the second call would
        // exchange again and break `.expect(1)`.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "access_token": "tok-1" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let auth = authenticator(&server);
        auth.bearer_token().await.unwrap();
        auth.bearer_token().await.unwrap();
    }

    #[tokio::test]
    async fn failed_exchange_is_not_cached() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(503).set_body_string("try later"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json("tok-2", 3600)))
            .expect(1)
            .mount(&server)
            .await;

        let auth = authenticator(&server);
        assert!(matches!(
            auth.bearer_token().await,
            Err(StackError::Http { status: 503, .. })
        ));
        assert_eq!(auth.bearer_token().await.unwrap(), "tok-2");
    }

    #[tokio::test]
    async fn invalidate_forces_a_new_exchange() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json("tok-1", 3600)))
            .expect(2)
            .mount(&server)
            .await;

        let auth = authenticator(&server);
        auth.bearer_token().await.unwrap();
        auth.invalidate().await;
        auth.bearer_token().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_exchange() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(token_json("tok-1", 3600))
                    .set_delay(Duration::from_millis(200)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let auth = authenticator(&server);
        let (first, second) = tokio::join!(auth.bearer_token(), auth.bearer_token());
        assert_eq!(first.unwrap(), "tok-1");
        assert_eq!(second.unwrap(), "tok-1");
    }

    #[tokio::test]
    async fn token_invalid_grant_maps_to_auth_with_description() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "Invalid JWT Signature."
            })))
            .mount(&server)
            .await;

        match authenticator(&server).bearer_token().await.unwrap_err() {
            StackError::Auth { message } => {
                assert!(message.contains("(invalid_grant): Invalid JWT Signature. Check"));
                assert!(!message.contains(".."));
                assert!(message.contains("private key ID matches"));
                assert!(message.contains("deleted or disabled"));
                assert!(message.contains("device clock"));
                assert!(message.contains("may not be active yet"));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn token_429_maps_to_http() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
            .mount(&server)
            .await;

        match authenticator(&server).bearer_token().await.unwrap_err() {
            StackError::Http { status, message } => {
                assert_eq!(status, 429);
                assert_eq!(message, "slow down");
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn token_503_maps_to_http() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .mount(&server)
            .await;

        assert!(matches!(
            authenticator(&server).bearer_token().await,
            Err(StackError::Http { status: 503, .. })
        ));
    }

    #[tokio::test]
    async fn token_200_without_access_token_is_decode() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "access_token": "  ", "expires_in": 3600 })),
            )
            .mount(&server)
            .await;

        assert!(matches!(
            authenticator(&server).bearer_token().await,
            Err(StackError::Decode { .. })
        ));
    }

    #[tokio::test]
    async fn token_200_malformed_json_is_decode() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>oops</html>"))
            .mount(&server)
            .await;

        assert!(matches!(
            authenticator(&server).bearer_token().await,
            Err(StackError::Decode { .. })
        ));
    }

    #[tokio::test]
    async fn token_transport_failure_is_network() {
        // Nothing listens on port 1, so the connection is refused.
        let auth = ServiceAccountAuthenticator::with_token_url(
            key_from(PKCS8_PEM),
            TEST_SCOPE,
            reqwest::Client::new(),
            "http://127.0.0.1:1/token".to_string(),
        );
        assert!(matches!(
            auth.bearer_token().await,
            Err(StackError::Network { .. })
        ));
    }

    #[tokio::test]
    async fn token_exchange_times_out_as_network() {
        // The lock is held across the exchange, so the injected client's timeout
        // is what bounds a stalled token endpoint.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(token_json("tok-1", 3600))
                    .set_delay(Duration::from_secs(5)),
            )
            .mount(&server)
            .await;

        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let auth = ServiceAccountAuthenticator::with_token_url(
            key_from(PKCS8_PEM),
            TEST_SCOPE,
            http,
            format!("{}/token", server.uri()),
        );
        assert!(matches!(
            auth.bearer_token().await,
            Err(StackError::Network { .. })
        ));
    }

    #[tokio::test]
    async fn token_exchange_traced_by_debug_logger() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json("tok-1", 3600)))
            .mount(&server)
            .await;

        let logger = Arc::new(CapturingLogger::default());
        let auth = authenticator(&server).with_debug_logger(Some(logger.clone()));
        auth.bearer_token().await.unwrap();

        let messages = logger.messages();
        assert_eq!(messages.len(), 2);
        assert!(messages[0].starts_with("[RustCore] → request"));
        assert!(messages[0].contains(&format!("curl -X POST '{}/token'", server.uri())));
        assert!(messages[0].contains("-d 'grant_type=urn%3Aietf"));
        assert!(messages[1].starts_with("[RustCore] ← 200"));
        assert!(messages[1].contains("\"access_token\": \"tok-1\""));
    }
}
