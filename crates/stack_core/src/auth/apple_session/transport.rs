//! HTTP plumbing for the Apple ID sign-in and session.
//!
//! Every request goes through [`crate::http_debug::send_unbuffered`], so the
//! host log sink gets its one line per request (method, host, path, status,
//! duration; never the query, headers or body), and **nothing** goes to the
//! opt-in debug cURL sink: these requests carry the `scnt` and session-id
//! headers, the two-factor code and Apple's SRP values, and their responses
//! carry session cookies. Cookies themselves are added below request building
//! by the session client's cookie provider, so they are never part of a
//! rendered request either.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use reqwest::header::{HeaderMap, CONTENT_ENCODING, CONTENT_TYPE, SET_COOKIE};
use reqwest::StatusCode;
use serde_json::Value;

use super::error::AppleSignInError;
use super::session::SessionJar;
use crate::error::StackError;
use crate::http_debug;
use crate::logging::redact;

/// The `User-Agent` spaceship sends (`"Spaceship #{Fastlane::VERSION}"`) in
/// the fastlane release the spike signed in with. Apple's sign-in is known to
/// work with it; a different value is an open question (see the Phase 1
/// notes), so it is one constant to change.
pub(crate) const USER_AGENT: &str = "Spaceship 2.232.2";

/// Bodies larger than this are refused (Apple's sign-in answers are a few
/// kilobytes; the cap bounds memory whatever the server sends).
pub(crate) const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

/// The `Accept-Encoding` Ruby's `Net::HTTP` adds to every request spaceship
/// sends (`Net::HTTPGenericRequest#initialize`), which it then decodes.
pub(crate) const NET_HTTP_ACCEPT_ENCODING: &str = "gzip;q=1.0,deflate;q=0.6,identity;q=0.3";

/// Leading body bytes quoted (as hex) when a body is not text: enough for a
/// format's magic (`1f8b` gzip, `bplist00`), too few to carry content.
const MAGIC_BYTES: usize = 8;

/// At most this many of Apple's error entries are quoted in a message.
const MAX_QUOTED_ERRORS: usize = 3;
/// At most this many characters of one Apple error message are quoted.
const MAX_ERROR_CHARS: usize = 200;

/// A finished exchange.
pub(crate) struct AppleReply {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: String,
}

impl AppleReply {
    /// The status as a number.
    pub(crate) fn code(&self) -> u16 {
        self.status.as_u16()
    }

    /// The body parsed as JSON, `None` when it is not JSON.
    pub(crate) fn json(&self) -> Option<Value> {
        serde_json::from_str(&self.body).ok()
    }

    /// A header as text (case-insensitive name), `None` when absent, empty or
    /// not visible ASCII.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    }

    /// Whether any `Set-Cookie` sets a cookie whose name contains `needle`.
    pub(crate) fn sets_cookie_named(&self, needle: &str) -> bool {
        self.headers.get_all(SET_COOKIE).iter().any(|value| {
            value
                .to_str()
                .ok()
                .and_then(|v| v.split_once('='))
                .is_some_and(|(name, _)| name.trim().contains(needle))
        })
    }
}

/// The client every sign-in and session request uses: the session's cookie
/// jar, no redirect following (spaceship's Faraday client follows none
/// either), spaceship's `User-Agent`, bounded timeouts.
///
/// # Errors
/// [`AppleSignInError::Network`] when the TLS backend cannot be set up.
pub(crate) fn session_client(jar: Arc<SessionJar>) -> Result<reqwest::Client, AppleSignInError> {
    reqwest::Client::builder()
        .cookie_provider(jar)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| AppleSignInError::Network {
            message: e.to_string(),
        })
}

/// A client with **no cookie store** and **no redirect following**, for the
/// `HEAD /logout` widget-key probe (see [`super::widget_key`]): sending the
/// session's cookies there, or following its redirect, signs the session out.
///
/// # Errors
/// As [`session_client`].
pub(crate) fn bare_client() -> Result<reqwest::Client, AppleSignInError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| AppleSignInError::Network {
            message: e.to_string(),
        })
}

/// Sends `builder` and reads at most [`MAX_BODY_BYTES`] of the body.
///
/// # Errors
/// [`AppleSignInError::Network`] on transport failure;
/// [`AppleSignInError::Unexpected`] for a body over the cap or not UTF-8.
pub(crate) async fn send(builder: reqwest::RequestBuilder) -> Result<AppleReply, AppleSignInError> {
    // The trace closure is deliberately empty: see the module docs.
    let mut sent = http_debug::send_unbuffered(builder, |_| {})
        .await
        .map_err(network)?;
    let status = sent.status();
    let headers = sent.headers().clone();

    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = sent.chunk().await.map_err(network)? {
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(AppleSignInError::unexpected(
                status.as_u16(),
                format!("the response is larger than {MAX_BODY_BYTES} bytes"),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let body = decode_content(&headers, body, status.as_u16())?;
    let body = String::from_utf8(body).map_err(|e| {
        AppleSignInError::unexpected(
            status.as_u16(),
            format!(
                "the response is not UTF-8 text ({})",
                body_shape(&headers, e.as_bytes())
            ),
        )
    })?;
    Ok(AppleReply {
        status,
        headers,
        body,
    })
}

/// The body with its `Content-Encoding` undone, the way Ruby's `Net::HTTP`
/// does for spaceship: `gzip` / `x-gzip` / `deflate` are inflated
/// (`Net::HTTP::Inflater` auto-detects gzip and zlib). A body that starts with
/// the gzip magic but has no (or an `identity`) encoding is inflated too: it
/// could not be UTF-8 text anyway (`0x8b` is never a UTF-8 lead byte). Any
/// other encoding (e.g. `br`) is left as is, so the UTF-8 check reports it.
/// The inflated size is capped at [`MAX_BODY_BYTES`].
///
/// # Errors
/// [`AppleSignInError::Unexpected`] for a corrupt or oversized compressed body.
fn decode_content(
    headers: &HeaderMap,
    raw: Vec<u8>,
    status: u16,
) -> Result<Vec<u8>, AppleSignInError> {
    inflate_body(headers, raw, MAX_BODY_BYTES).map_err(|err| match err {
        InflateError::Corrupt => {
            AppleSignInError::unexpected(status, "the compressed response is corrupt")
        }
        InflateError::TooLarge => AppleSignInError::unexpected(
            status,
            format!("the decompressed response is larger than {MAX_BODY_BYTES} bytes"),
        ),
    })
}

/// The decompressed-size cap of [`decode_session_body`]: far above any App
/// Store Connect or Developer Portal page, low enough to stop a gzip bomb.
pub(crate) const MAX_SESSION_BODY_BYTES: usize = 64 * 1024 * 1024;

/// [`decode_content`] for the App Store Connect client's web session requests
/// (iris and `services-account`): the Developer Portal gzips some answers
/// without being asked (spike, `listTeams.action`), and reqwest's own
/// decompression stays off so the API-key client is unchanged. Capped at
/// [`MAX_SESSION_BODY_BYTES`].
///
/// # Errors
/// [`StackError::Decode`] for a corrupt or oversized compressed body.
pub(crate) fn decode_session_body(
    headers: &HeaderMap,
    raw: Vec<u8>,
) -> Result<Vec<u8>, StackError> {
    inflate_body(headers, raw, MAX_SESSION_BODY_BYTES).map_err(|err| match err {
        InflateError::Corrupt => StackError::decode("the compressed response is corrupt"),
        InflateError::TooLarge => StackError::decode(format!(
            "the decompressed response is larger than {MAX_SESSION_BODY_BYTES} bytes"
        )),
    })
}

/// Why a body could not be inflated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InflateError {
    Corrupt,
    TooLarge,
}

/// See [`decode_content`]; `cap` bounds the inflated size.
fn inflate_body(headers: &HeaderMap, raw: Vec<u8>, cap: usize) -> Result<Vec<u8>, InflateError> {
    if raw.is_empty() {
        return Ok(raw);
    }
    let encoding = header_text(headers, CONTENT_ENCODING.as_str()).to_ascii_lowercase();
    let gzip_magic = raw.starts_with(&[0x1f, 0x8b]);
    match encoding.as_str() {
        "gzip" | "x-gzip" | "deflate" => inflate(&raw, cap),
        "" | "identity" if gzip_magic => inflate(&raw, cap),
        _ => Ok(raw),
    }
}

/// [`inflate_body`] for the network audit, which has the lowercase
/// `Content-Encoding` instead of the headers: the inflated body, or `None`
/// when `raw` is not compressed (or is corrupt, or inflates beyond `cap`).
pub(crate) fn inflate_by_encoding(encoding: &str, raw: &[u8], cap: usize) -> Option<Vec<u8>> {
    let gzip_magic = raw.starts_with(&[0x1f, 0x8b]);
    match encoding {
        "gzip" | "x-gzip" | "deflate" => inflate(raw, cap).ok(),
        "" | "identity" if gzip_magic => inflate(raw, cap).ok(),
        _ => None,
    }
}

/// gzip when the body has its magic, else zlib, else raw deflate (servers
/// disagree on what `deflate` means).
fn inflate(raw: &[u8], cap: usize) -> Result<Vec<u8>, InflateError> {
    if raw.starts_with(&[0x1f, 0x8b]) {
        return read_capped(GzDecoder::new(raw), cap);
    }
    read_capped(ZlibDecoder::new(raw), cap).or_else(|err| match err {
        InflateError::TooLarge => Err(err),
        InflateError::Corrupt => read_capped(DeflateDecoder::new(raw), cap),
    })
}

fn read_capped(reader: impl Read, cap: usize) -> Result<Vec<u8>, InflateError> {
    let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
    let mut out = Vec::new();
    reader
        .take(limit)
        .read_to_end(&mut out)
        .map_err(|_| InflateError::Corrupt)?;
    if out.len() > cap {
        return Err(InflateError::TooLarge);
    }
    Ok(out)
}

/// What a non-text body is, for an error message: its `Content-Type` and
/// `Content-Encoding` and its first [`MAGIC_BYTES`] bytes as hex. Nothing
/// else of the body or the headers.
pub(crate) fn body_shape(headers: &HeaderMap, body: &[u8]) -> String {
    let or_none = |name: &str| {
        let value = header_text(headers, name);
        if value.is_empty() {
            "none".to_string()
        } else {
            value
        }
    };
    let magic: String = body
        .iter()
        .take(MAGIC_BYTES)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "Content-Type: {}, Content-Encoding: {}, {} bytes, first bytes: {}",
        or_none(CONTENT_TYPE.as_str()),
        or_none(CONTENT_ENCODING.as_str()),
        body.len(),
        if magic.is_empty() { "none" } else { &magic }
    )
}

/// A header as trimmed text, bounded to 80 visible-ASCII characters (these are
/// media types and codings, never secrets); empty when absent.
pub(crate) fn header_text(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().chars().take(80).collect())
        .unwrap_or_default()
}

fn network(err: StackError) -> AppleSignInError {
    match err {
        StackError::Network { message } => AppleSignInError::Network { message },
        other => AppleSignInError::Network {
            message: other.to_string(),
        },
    }
}

/// The failures every endpoint maps the same way: 429 and 5xx.
pub(crate) fn common_status_error(status: StatusCode) -> Option<AppleSignInError> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        Some(AppleSignInError::RateLimited)
    } else if status.is_server_error() {
        Some(AppleSignInError::AppleUnavailable {
            status: status.as_u16(),
        })
    } else {
        None
    }
}

/// One entry of Apple's `serviceErrors` / `service_errors` /
/// `validationErrors` arrays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceError {
    pub(crate) code: String,
    pub(crate) message: String,
}

/// Every error entry in `body` (idmsa uses all three spellings; see the
/// comments in `two_step_or_factor_client.rb`).
pub(crate) fn service_errors(body: &Value) -> Vec<ServiceError> {
    ["serviceErrors", "service_errors", "validationErrors"]
        .iter()
        .filter_map(|key| body.get(*key).and_then(Value::as_array))
        .flatten()
        .map(|entry| ServiceError {
            code: text_field(entry, "code"),
            message: {
                let message = text_field(entry, "message");
                if message.is_empty() {
                    text_field(entry, "title")
                } else {
                    message
                }
            },
        })
        .collect()
}

fn text_field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `message (code); message (code)` for the first entries, scrubbed and cut,
/// or `fallback` when there are none.
pub(crate) fn describe_errors(errors: &[ServiceError], fallback: &str) -> String {
    if errors.is_empty() {
        return fallback.to_string();
    }
    errors
        .iter()
        .take(MAX_QUOTED_ERRORS)
        .map(|e| {
            let message: String = e.message.chars().take(MAX_ERROR_CHARS).collect();
            redact::scrub(&format!("{} ({})", message.trim(), e.code))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;
    use serde_json::json;

    fn reply(headers: HeaderMap, body: &str) -> AppleReply {
        AppleReply {
            status: StatusCode::OK,
            headers,
            body: body.to_string(),
        }
    }

    #[test]
    fn service_errors_reads_every_spelling() {
        let body = json!({
            "serviceErrors": [{"code": "-20101", "message": "Your Apple ID or password was incorrect."}],
            "service_errors": [{"code": "-21669", "title": "Incorrect Verification Code"}],
            "validationErrors": [{"code": -1, "message": "x"}]
        });
        let errors = service_errors(&body);
        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0].code, "-20101");
        assert_eq!(errors[1].message, "Incorrect Verification Code");
        assert_eq!(errors[2].code, "-1");
        assert!(service_errors(&json!({"ok": true})).is_empty());
    }

    #[test]
    fn describe_errors_quotes_scrubbed_and_bounded() {
        let errors = vec![
            ServiceError {
                code: "-20209".into(),
                message: "Apple ID dev@example.com is locked".into(),
            },
            ServiceError {
                code: "1".into(),
                message: "x".repeat(500),
            },
        ];
        let text = describe_errors(&errors, "none");
        assert!(text.starts_with("Apple ID d***@example.com is locked (-20209); "));
        assert!(text.len() < 300);
        assert_eq!(describe_errors(&[], "none"), "none");
    }

    #[test]
    fn header_and_cookie_helpers() {
        let mut headers = HeaderMap::new();
        headers.insert("scnt", HeaderValue::from_static(" abc "));
        headers.insert("empty", HeaderValue::from_static(""));
        headers.append(SET_COOKIE, HeaderValue::from_static("itctx=1; Path=/"));
        headers.append(SET_COOKIE, HeaderValue::from_static("myacinfo=2; Secure"));
        let reply = reply(headers, "{\"a\":1}");
        assert_eq!(reply.header("SCNT"), Some("abc"));
        assert_eq!(reply.header("empty"), None);
        assert_eq!(reply.header("missing"), None);
        assert!(reply.sets_cookie_named("itctx"));
        assert!(reply.sets_cookie_named("myac"));
        assert!(!reply.sets_cookie_named("dqsid"));
        assert_eq!(reply.json(), Some(json!({"a": 1})));
        assert_eq!(reply.code(), 200);
    }

    #[test]
    fn common_status_errors() {
        assert_eq!(
            common_status_error(StatusCode::TOO_MANY_REQUESTS),
            Some(AppleSignInError::RateLimited)
        );
        assert_eq!(
            common_status_error(StatusCode::BAD_GATEWAY),
            Some(AppleSignInError::AppleUnavailable { status: 502 })
        );
        assert_eq!(common_status_error(StatusCode::CONFLICT), None);
        assert_eq!(common_status_error(StatusCode::OK), None);
    }

    mod content {
        use super::*;
        use crate::auth::apple_session::test_support::session_client;
        use flate2::write::{GzEncoder, ZlibEncoder};
        use flate2::Compression;
        use std::io::Write as _;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const JSON: &str = r#"{"resultCode":0,"teams":[{"teamId":"T1"}]}"#;

        fn gzip(bytes: &[u8]) -> Vec<u8> {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }

        fn zlib(bytes: &[u8]) -> Vec<u8> {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }

        /// Serves `body` with `headers` and sends one request through [`send`].
        async fn fetch(
            body: Vec<u8>,
            headers: &[(&str, &str)],
        ) -> Result<AppleReply, AppleSignInError> {
            let server = MockServer::start().await;
            let mut response = ResponseTemplate::new(200).set_body_bytes(body);
            for (name, value) in headers {
                response = response.insert_header(*name, *value);
            }
            Mock::given(method("POST"))
                .and(path("/x"))
                .respond_with(response)
                .mount(&server)
                .await;
            send(session_client().post(format!("{}/x", server.uri()))).await
        }

        /// The error message of a fetch that must fail (`AppleReply` has no
        /// `Debug`, on purpose: its headers carry cookies).
        async fn fetch_err(body: Vec<u8>, headers: &[(&str, &str)]) -> String {
            match fetch(body, headers).await {
                Ok(_) => panic!("the fetch should have failed"),
                Err(err) => err.to_string(),
            }
        }

        #[tokio::test]
        async fn gzip_and_deflate_bodies_are_inflated() {
            let reply = fetch(gzip(JSON.as_bytes()), &[("content-encoding", "gzip")])
                .await
                .unwrap();
            assert_eq!(reply.body, JSON);
            let reply = fetch(zlib(JSON.as_bytes()), &[("content-encoding", "deflate")])
                .await
                .unwrap();
            assert_eq!(reply.body, JSON);
            // Apple compressing without saying so.
            let reply = fetch(
                gzip(JSON.as_bytes()),
                &[("content-type", "application/json")],
            )
            .await
            .unwrap();
            assert_eq!(reply.body, JSON);
        }

        #[tokio::test]
        async fn plain_bodies_are_untouched() {
            let reply = fetch(JSON.as_bytes().to_vec(), &[]).await.unwrap();
            assert_eq!(reply.body, JSON);
            let reply = fetch(Vec::new(), &[("content-encoding", "gzip")])
                .await
                .unwrap();
            assert_eq!(reply.body, "");
        }

        #[tokio::test]
        async fn a_non_text_body_reports_its_shape_and_magic_only() {
            let mut bplist = b"bplist00".to_vec();
            bplist.extend_from_slice(&[0xd1, 0x01, 0x02, 0xff, 0xfe, b's', b'e', b'c']);
            let len = bplist.len();
            let err = fetch_err(bplist, &[("content-type", "application/x-plist")]).await;
            assert!(
                err.contains(&format!(
                    "Content-Type: application/x-plist, Content-Encoding: none, {len} bytes, \
                     first bytes: 62706c6973743030"
                )),
                "{err}"
            );
            assert!(!err.contains("d101"), "{err}");

            let err = fetch_err(vec![0xce, 0xb2, 0xff, 0x00], &[("content-encoding", "br")]).await;
            assert!(
                err.contains(
                    "Content-Type: none, Content-Encoding: br, 4 bytes, first bytes: ceb2ff00"
                ),
                "{err}"
            );
        }

        #[tokio::test]
        async fn a_corrupt_or_oversized_compressed_body_is_refused() {
            let err = fetch_err(
                vec![0x1f, 0x8b, 0x08, 0x00, 0x01],
                &[("content-encoding", "gzip")],
            )
            .await;
            assert!(err.contains("corrupt"), "{err}");

            let bomb = gzip(&vec![b'a'; MAX_BODY_BYTES + 1]);
            assert!(bomb.len() < MAX_BODY_BYTES);
            let err = fetch_err(bomb, &[("content-encoding", "gzip")]).await;
            assert!(err.contains("decompressed response is larger"), "{err}");
        }
    }
}
