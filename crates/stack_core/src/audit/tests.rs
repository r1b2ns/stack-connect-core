//! The network audit at the HTTP choke point: one entry per exchange, raw
//! request and response, the scope's operation / provider / account /
//! credential hints, body capture on and off, and nothing without a sink.

use std::io::Write as _;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::scope::{self, AuditBinding};
use super::testing::{PanickingSink, RecordingSink};
use super::types::*;
use crate::error::StackError;
use crate::http_debug;
use crate::ports::NetworkAuditSink;

const ACCOUNT: &str = "acct-1";

fn header<'a>(headers: &'a [NetworkAuditHeader], name: &str) -> Vec<&'a str> {
    headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
        .collect()
}

fn jwt(exp: i64) -> String {
    format!(
        "{}.{}.signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(format!(r#"{{"iss":"issuer","exp":{exp}}}"#))
    )
}

#[tokio::test]
async fn a_successful_exchange_is_one_raw_entry_with_every_field() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/things"))
        .respond_with(
            ResponseTemplate::new(201)
                .append_header("Set-Cookie", "a=1; Path=/")
                .append_header("Set-Cookie", "b=2; Path=/")
                .insert_header("x-request-id", "req-7")
                .set_body_raw(r#"{"id":"42"}"#, "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let sink = RecordingSink::new(true);
    let target = sink.target(Some(ACCOUNT), NetworkAuditProvider::AppleApi);
    let token = jwt(1_764_000_000);
    let url = format!("{}/v1/things?limit=5&cursor=c%2F1", server.uri());
    let builder = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("x-custom", "v")
        .json(&serde_json::json!({"name": "x"}));
    let reply = scope::operation(
        Some(&target),
        "things.create",
        http_debug::send(builder, None),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, reqwest::StatusCode::CREATED);
    assert_eq!(reply.body, r#"{"id":"42"}"#);

    let entries = sink.entries();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let e = &entries[0];
    assert_eq!(uuid::Uuid::parse_str(&e.id).unwrap().get_version_num(), 4);
    assert!(e.started_at_ms > 1_700_000_000_000, "{}", e.started_at_ms);
    assert_eq!(e.attempt, 1);
    assert_eq!(e.operation, "things.create");
    assert_eq!(e.parent_operation, None);
    assert_eq!(e.provider, NetworkAuditProvider::AppleApi);
    assert_eq!(e.account_id.as_deref(), Some(ACCOUNT));
    assert_eq!(e.credential_kind, NetworkAuditCredentialKind::AscJwt);
    assert_eq!(e.token_expires_at_ms, Some(1_764_000_000_000));
    assert_eq!(e.method, "POST");
    assert_eq!(e.url, url);
    assert_eq!(e.http_version.as_deref(), Some("HTTP/1.1"));
    // Raw: the bearer is in the entry verbatim (the host masks it).
    assert_eq!(
        header(&e.request_headers, "authorization"),
        [format!("Bearer {token}").as_str()]
    );
    assert_eq!(header(&e.request_headers, "x-custom"), ["v"]);
    assert_eq!(
        header(&e.request_headers, "content-type"),
        ["application/json"]
    );
    assert_eq!(e.request_body.as_deref(), Some(r#"{"name":"x"}"#));
    assert_eq!(e.request_body_state, NetworkAuditBodyState::Captured);
    assert_eq!(e.request_size_bytes, 12);
    assert_eq!(e.response_status, Some(201));
    assert_eq!(
        header(&e.response_headers, "set-cookie"),
        ["a=1; Path=/", "b=2; Path=/"]
    );
    assert_eq!(header(&e.response_headers, "x-request-id"), ["req-7"]);
    assert_eq!(e.response_body.as_deref(), Some(r#"{"id":"42"}"#));
    assert_eq!(e.response_body_state, NetworkAuditBodyState::Captured);
    assert_eq!(e.response_size_bytes, 11);
    assert_eq!(e.error, None);
}

#[tokio::test]
async fn every_exchange_gets_its_own_entry_and_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let sink = RecordingSink::new(true);
    let target = sink.target(Some(ACCOUNT), NetworkAuditProvider::GooglePlay);
    let client = reqwest::Client::new();
    scope::operation(Some(&target), "apps.page", async {
        for _ in 0..2 {
            http_debug::send(client.get(server.uri()), None)
                .await
                .unwrap();
        }
    })
    .await;

    let entries = sink.entries();
    assert_eq!(entries.len(), 2);
    assert_ne!(entries[0].id, entries[1].id);
    // No body either way: empty, not "not captured".
    assert_eq!(entries[0].request_body_state, NetworkAuditBodyState::Empty);
    assert_eq!(entries[0].response_body_state, NetworkAuditBodyState::Empty);
    assert_eq!(
        entries[0].credential_kind,
        NetworkAuditCredentialKind::Unauthenticated
    );
}

#[tokio::test]
async fn an_http_error_is_an_entry_with_its_status_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(503).set_body_raw("upstream down", "text/plain; charset=utf-8"),
        )
        .mount(&server)
        .await;
    let sink = RecordingSink::new(true);
    let target = sink.target(Some(ACCOUNT), NetworkAuditProvider::GooglePlay);
    let (status, _) = scope::operation(
        Some(&target),
        "reviews.fetch_customer_reviews",
        http_debug::send_and_read(reqwest::Client::new().get(server.uri()), None),
    )
    .await
    .unwrap();
    assert_eq!(status.as_u16(), 503);

    let e = &sink.entries()[0];
    assert_eq!(e.response_status, Some(503));
    assert_eq!(e.response_body.as_deref(), Some("upstream down"));
    assert_eq!(e.response_body_state, NetworkAuditBodyState::Captured);
    // An HTTP error is a completed exchange, not a failure.
    assert_eq!(e.error, None);
}

#[tokio::test]
async fn a_transport_failure_is_an_entry_without_a_status() {
    let sink = RecordingSink::new(true);
    let target = sink.target(Some(ACCOUNT), NetworkAuditProvider::AppleApi);
    let result = scope::operation(
        Some(&target),
        "provider.validate",
        http_debug::send(
            reqwest::Client::new().get("http://127.0.0.1:1/unreachable?token=abc"),
            None,
        ),
    )
    .await;
    assert!(matches!(result, Err(StackError::Network { .. })));

    let entries = sink.entries();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.url, "http://127.0.0.1:1/unreachable?token=abc");
    assert_eq!(e.response_status, None);
    assert_eq!(e.http_version, None);
    assert!(e.response_headers.is_empty());
    assert_eq!(e.response_body, None);
    assert_eq!(e.response_body_state, NetworkAuditBodyState::Unavailable);
    assert_eq!(e.response_size_bytes, 0);
    let error = e.error.as_deref().unwrap();
    assert!(error.contains("error sending request"), "{error}");
}

#[tokio::test]
async fn without_body_capture_bodies_are_omitted_and_sizes_kept() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200).set_body_string("secret answer"))
        .mount(&server)
        .await;
    let sink = RecordingSink::new(false);
    let target = sink.target(Some(ACCOUNT), NetworkAuditProvider::GooglePlay);
    scope::operation(
        Some(&target),
        "tracks.apply_release_change",
        http_debug::send(
            reqwest::Client::new()
                .put(server.uri())
                .body("secret request"),
            None,
        ),
    )
    .await
    .unwrap();

    let e = &sink.entries()[0];
    assert_eq!(e.request_body, None);
    assert_eq!(e.request_body_state, NetworkAuditBodyState::NotCaptured);
    assert_eq!(e.request_size_bytes, 14);
    assert_eq!(e.response_body, None);
    assert_eq!(e.response_body_state, NetworkAuditBodyState::NotCaptured);
    assert_eq!(e.response_size_bytes, 13);
    // Headers are always kept.
    assert_eq!(e.response_status, Some(200));
}

#[tokio::test]
async fn binary_bodies_are_omitted_with_their_size() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(vec![0x89, b'P', b'N', b'G', 0, 1], "image/png"),
        )
        .mount(&server)
        .await;
    let sink = RecordingSink::new(true);
    let target = sink.target(Some(ACCOUNT), NetworkAuditProvider::AppleApi);
    scope::operation(
        Some(&target),
        "app_metadata.upload_screenshot",
        http_debug::send(
            reqwest::Client::new()
                .put(server.uri())
                .header("content-type", "image/png")
                .body(vec![0x89u8, b'P', b'N', b'G']),
            None,
        ),
    )
    .await
    .unwrap();

    let e = &sink.entries()[0];
    assert_eq!(e.request_body, None);
    assert_eq!(e.request_body_state, NetworkAuditBodyState::Binary);
    assert_eq!(e.request_size_bytes, 4);
    assert_eq!(e.response_body, None);
    assert_eq!(e.response_body_state, NetworkAuditBodyState::Binary);
    assert_eq!(e.response_size_bytes, 6);
}

#[tokio::test]
async fn nothing_is_recorded_without_a_sink() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    let sink = RecordingSink::new(true);
    let unbound = AuditBinding::default();
    let client = reqwest::Client::new();

    // No scope at all, an unbound handle, and the overrides on their own:
    // none of them enters a scope, so nothing reaches any sink.
    http_debug::send(client.get(server.uri()), None)
        .await
        .unwrap();
    unbound
        .run("reviews.fetch_customer_reviews", async {
            assert!(scope::current().is_none());
            http_debug::send(client.get(server.uri()), None)
                .await
                .unwrap();
        })
        .await;
    scope::attempt(2, async {
        scope::credential(NetworkAuditCredentialKind::Unauthenticated, async {
            assert!(scope::current().is_none());
            http_debug::send(client.get(server.uri()), None)
                .await
                .unwrap();
        })
        .await;
    })
    .await;
    assert!(sink.entries().is_empty());

    // A scope ends with its future: a later call is not audited.
    let target = sink.target(Some(ACCOUNT), NetworkAuditProvider::AppleApi);
    scope::operation(Some(&target), "provider.validate", async {
        http_debug::send(client.get(server.uri()), None)
            .await
            .unwrap();
    })
    .await;
    http_debug::send(client.get(server.uri()), None)
        .await
        .unwrap();
    assert_eq!(sink.entries().len(), 1);
}

#[tokio::test]
async fn a_panicking_sink_never_breaks_the_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("still fine"))
        .mount(&server)
        .await;
    let sink: Arc<dyn NetworkAuditSink> = Arc::new(PanickingSink);
    let target = super::AuditTarget::new(Some(sink), Some(ACCOUNT), NetworkAuditProvider::AppleApi);
    let logs = crate::logging::testing::capture_logs();
    let (status, body) = scope::operation(
        target.as_ref(),
        "provider.validate",
        http_debug::send_and_read(reqwest::Client::new().get(server.uri()), None),
    )
    .await
    .unwrap();
    assert_eq!((status.as_u16(), body.as_str()), (200, "still fine"));
    assert_eq!(
        logs.messages(crate::logging::LogLevel::Warn, "http"),
        ["the network audit sink panicked; one entry was dropped"]
    );
}

#[tokio::test]
async fn nested_operations_attempts_and_overrides_shape_the_hints() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let sink = RecordingSink::new(true);
    let target = sink.target(Some(ACCOUNT), NetworkAuditProvider::GooglePlay);
    let client = reqwest::Client::new();
    let assertion = jwt(1_764_003_600);
    let token_request = || {
        client.post(server.uri()).form(&[
            ("grant_type", "jwt-bearer"),
            ("assertion", assertion.as_str()),
        ])
    };

    scope::operation(Some(&target), "sync.sync_apps", async {
        scope::operation(None, "provider.fetch_apps", async {
            scope::attempt(2, async {
                // The token exchange restarts the attempt count.
                scope::sub_operation(
                    super::ops::GOOGLE_OAUTH_TOKEN,
                    NetworkAuditProvider::GoogleOauth,
                    NetworkAuditCredentialKind::OauthAssertion,
                    http_debug::send(token_request(), None),
                )
                .await
                .unwrap();
                http_debug::send(client.post(server.uri()).bearer_auth("ya29.t"), None)
                    .await
                    .unwrap();
            })
            .await;
        })
        .await;
    })
    .await;

    let entries = sink.entries();
    let hints: Vec<_> = entries
        .iter()
        .map(|e| {
            (
                e.operation.as_str(),
                e.parent_operation.as_deref(),
                e.attempt,
                e.provider,
                e.credential_kind,
                e.token_expires_at_ms,
            )
        })
        .collect();
    assert_eq!(
        hints,
        [
            (
                "google.oauth.token",
                Some("provider.fetch_apps"),
                1,
                NetworkAuditProvider::GoogleOauth,
                NetworkAuditCredentialKind::OauthAssertion,
                Some(1_764_003_600_000),
            ),
            (
                "provider.fetch_apps",
                Some("sync.sync_apps"),
                2,
                NetworkAuditProvider::GooglePlay,
                NetworkAuditCredentialKind::GoogleAccessToken,
                None,
            ),
        ]
    );
    assert!(entries
        .iter()
        .all(|e| e.account_id.as_deref() == Some(ACCOUNT)));
}

#[tokio::test]
async fn a_web_session_request_is_tagged_with_the_session_cookie_unless_overridden() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let sink = RecordingSink::new(true);
    let target = sink.target(None, NetworkAuditProvider::AppleWebSession);
    let client = reqwest::Client::new();
    scope::operation(Some(&target), "apple_id.start", async {
        http_debug::send(client.get(server.uri()), None)
            .await
            .unwrap();
        scope::credential(
            NetworkAuditCredentialKind::Unauthenticated,
            http_debug::send(client.get(server.uri()), None),
        )
        .await
        .unwrap();
    })
    .await;

    let kinds: Vec<_> = sink
        .entries()
        .iter()
        .map(|e| (e.credential_kind, e.account_id.clone()))
        .collect();
    assert_eq!(
        kinds,
        [
            (NetworkAuditCredentialKind::AppleSessionCookie, None),
            (NetworkAuditCredentialKind::Unauthenticated, None),
        ]
    );
}

#[tokio::test]
async fn a_streamed_body_is_recorded_once_read_or_dropped() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/whole"))
        .respond_with(ResponseTemplate::new(200).set_body_string("whole body"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_string("a".repeat(10_000)))
        .mount(&server)
        .await;
    let sink = RecordingSink::new(true);
    let target = sink.target(Some(ACCOUNT), NetworkAuditProvider::GooglePlay);
    let client = reqwest::Client::new();

    scope::operation(Some(&target), "app_icons.fetch_icon_url", async {
        // Read in one go.
        let sent =
            http_debug::send_unbuffered(client.get(format!("{}/whole", server.uri())), |_| {})
                .await
                .unwrap();
        assert_eq!(sent.text().await.unwrap(), "whole body");
        // Read up to a cap, the rest left unread.
        let (_, body) = http_debug::send_and_read_capped(
            client.get(format!("{}/big", server.uri())),
            None,
            100,
        )
        .await
        .unwrap();
        assert_eq!(body.len(), 100);
    })
    .await;

    let entries = sink.entries();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[0].response_body.as_deref(), Some("whole body"));
    assert_eq!(
        entries[0].response_body_state,
        NetworkAuditBodyState::Captured
    );
    assert_eq!(entries[0].response_size_bytes, 10);
    assert_eq!(
        entries[1].response_body_state,
        NetworkAuditBodyState::Truncated
    );
    let kept = entries[1].response_body.as_deref().unwrap();
    assert!(
        kept.len() >= 100 && kept.bytes().all(|b| b == b'a'),
        "{}",
        kept.len()
    );
    assert_eq!(
        entries[1].response_size_bytes,
        u64::try_from(kept.len()).unwrap()
    );
}

fn gzip(text: &str) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(text.as_bytes()).unwrap();
    encoder.finish().unwrap()
}

#[tokio::test]
async fn compressed_bodies_are_recorded_decompressed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-encoding", "gzip")
                .set_body_raw(gzip(r#"{"teams":[]}"#), "application/json"),
        )
        .mount(&server)
        .await;
    let sink = RecordingSink::new(true);
    let target = sink.target(Some(ACCOUNT), NetworkAuditProvider::AppleWebSession);
    let client = reqwest::Client::new();

    scope::operation(Some(&target), "devices.fetch_devices", async {
        // The App Store Connect session path decodes the body itself…
        http_debug::send_decoding(client.get(server.uri()), None, |headers, raw| {
            crate::auth::apple_session::transport::decode_session_body(headers, raw)
        })
        .await
        .unwrap();
        // …and so does the Apple ID sign-in transport, over a streamed body.
        crate::auth::apple_session::transport::send(client.get(server.uri()))
            .await
            .unwrap();
    })
    .await;

    let entries = sink.entries();
    assert_eq!(entries.len(), 2, "{entries:?}");
    for e in &entries {
        assert_eq!(e.response_body.as_deref(), Some(r#"{"teams":[]}"#), "{e:?}");
        assert_eq!(e.response_body_state, NetworkAuditBodyState::Captured);
        assert_eq!(e.response_size_bytes, 12);
        assert_eq!(header(&e.response_headers, "content-encoding"), ["gzip"]);
    }
}

/// Every operation name in the code is listed in docs/NETWORK_AUDIT.md (the
/// host keys on them, so the set is documented and kept stable).
#[test]
fn every_operation_name_is_documented() {
    let doc = include_str!("../../../../docs/NETWORK_AUDIT.md");
    let sources = [
        include_str!("../service/provider.rs"),
        include_str!("../service/sync.rs"),
        include_str!("../apple_id.rs"),
        include_str!("mod.rs"),
        include_str!("../service/capabilities/accessibility_declarations.rs"),
        include_str!("../service/capabilities/analytics.rs"),
        include_str!("../service/capabilities/app_details.rs"),
        include_str!("../service/capabilities/app_icons.rs"),
        include_str!("../service/capabilities/app_metadata.rs"),
        include_str!("../service/capabilities/app_store_versions.rs"),
        include_str!("../service/capabilities/beta_app_localizations.rs"),
        include_str!("../service/capabilities/beta_app_review_detail.rs"),
        include_str!("../service/capabilities/beta_build_localizations.rs"),
        include_str!("../service/capabilities/beta_groups.rs"),
        include_str!("../service/capabilities/builds.rs"),
        include_str!("../service/capabilities/bundle_ids.rs"),
        include_str!("../service/capabilities/certificates.rs"),
        include_str!("../service/capabilities/devices.rs"),
        include_str!("../service/capabilities/play_users.rs"),
        include_str!("../service/capabilities/profiles.rs"),
        include_str!("../service/capabilities/reviews.rs"),
        include_str!("../service/capabilities/store_listings.rs"),
        include_str!("../service/capabilities/tracks.rs"),
        include_str!("../service/capabilities/users.rs"),
    ];
    let mut names = Vec::new();
    for source in sources {
        // `.run("x.y"`, `"x.y",` (scope::operation) and `const X: &str = "x.y"`.
        for (index, _) in source.match_indices('"') {
            let rest = &source[index + 1..];
            let Some(end) = rest.find('"') else { continue };
            let candidate = &rest[..end];
            let is_name = candidate.split_once('.').is_some_and(|(handle, method)| {
                !handle.is_empty()
                    && !method.is_empty()
                    && candidate
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'.')
            });
            let context = &source[index.saturating_sub(40)..index];
            if is_name
                && (context.contains("run(")
                    || context.contains("operation(")
                    || context.contains("&str =")
                    || context.trim_end().ends_with("Some(target),")
                    || context.contains("audit_target(),"))
            {
                names.push(candidate);
            }
        }
    }
    names.sort_unstable();
    names.dedup();
    // 115 capability methods + provider (2) + sync (1) + sign-in (3) + 3 internal steps.
    assert_eq!(names.len(), 124, "{names:?}");
    let missing: Vec<_> = names
        .iter()
        .filter(|name| !doc.contains(&format!("`{name}`")))
        .collect();
    assert!(missing.is_empty(), "undocumented operations: {missing:?}");
}
