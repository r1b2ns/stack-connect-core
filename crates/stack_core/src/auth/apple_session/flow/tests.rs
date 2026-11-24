//! The sign-in state machine against a mock Apple (idmsa and App Store Connect
//! on one wiremock server). The SRP private ephemeral and the clock are fixed,
//! so `signin/complete` must carry exactly the `fastlane-sirp` vector's `m1` /
//! `m2` and spaceship's hashcash for the fixed date.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::cookie::CookieStore as _;
use reqwest::header::HeaderValue;
use serde_json::json;
use wiremock::matchers::{
    body_json, body_partial_json, header, headers, method, path, path_regex, query_param,
};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::auth::apple_session::session::SessionBlob;
use crate::auth::apple_session::test_support::{
    auth_options_no_trusted_devices, auth_options_trusted_devices, olympus_session_body,
    session_on, sms_cannot_be_sent, vector, wrong_code_body,
};
use crate::error::StackError;
use crate::logging::testing::capture_logs;

const WIDGET_KEY: &str = "e0b80c3bf78523bfe80974d320935bfa30add02e1bff88ec2166c6bd5a706c42";
const SCNT: &str = "AAAA-scnt-0123456789abcdef";
const SESSION_ID: &str = "sid-fedcba9876543210";
/// The challenge of spaceship's `hashcash_spec.rb` and its solution at the
/// fixed clock.
const HC_CHALLENGE: &str = "4d74fb15eb23f465f1f6fcbf534e5877";
const HC_STAMP: &str = "1:11:20230223170600:4d74fb15eb23f465f1f6fcbf534e5877::6373";
const MYACINFO: &str = "myacinfo-session-secret-value";
const TRUST_COOKIE: &str = "HSARM-trust-secret-value==SRVT";

/// 2023-02-23T17:06:00Z, the date of spaceship's hashcash spec.
fn fixed_clock() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_677_171_960)
}

struct MockApple {
    server: MockServer,
}

impl MockApple {
    /// Widget key, `signin/init` (answering `s2k`) and the hashcash challenge.
    async fn new() -> Self {
        Self::with_init("s2k", true).await
    }

    async fn with_init(protocol: &str, hashcash: bool) -> Self {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/logout"))
            .respond_with(ResponseTemplate::new(302).insert_header(
                "Location",
                format!("/signout?widgetKey={WIDGET_KEY}&asop=destroy-session").as_str(),
            ))
            .mount(&server)
            .await;
        let apple = Self { server };
        apple
            .mount_init(ResponseTemplate::new(200).set_body_json(vector::init_body(protocol)))
            .await;
        let challenge = if hashcash {
            ResponseTemplate::new(200)
                .insert_header("X-Apple-HC-Bits", "11")
                .insert_header("X-Apple-HC-Challenge", HC_CHALLENGE)
        } else {
            ResponseTemplate::new(200)
        };
        Mock::given(method("GET"))
            .and(path("/appleauth/auth/signin"))
            .and(query_param("widgetKey", WIDGET_KEY))
            .respond_with(challenge.set_body_string("<html></html>"))
            .mount(&apple.server)
            .await;
        apple
    }

    fn endpoints(&self) -> AppleEndpoints {
        AppleEndpoints::with_base_urls(&self.server.uri(), &self.server.uri())
    }

    fn sign_in(&self) -> AppleSignIn {
        AppleSignIn::new(self.endpoints())
            .unwrap()
            .with_srp_private(vector::private_a())
            .with_clock(fixed_clock)
    }

    async fn mount_init(&self, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/appleauth/auth/signin/init"))
            .and(header("x-apple-widget-key", WIDGET_KEY))
            .and(header("x-requested-with", "XMLHttpRequest"))
            .and(header("content-type", "application/json"))
            .and(headers(
                "accept",
                vec!["application/json", "text/javascript"],
            ))
            .and(body_json(json!({
                "a": vector::A_B64,
                "accountName": vector::EMAIL,
                "protocols": ["s2k", "s2k_fo"],
            })))
            .respond_with(response)
            .mount(&self.server)
            .await;
    }

    /// `signin/complete`, matched on the exact SRP proof and hashcash.
    async fn mount_complete(&self, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/appleauth/auth/signin/complete"))
            .and(query_param("isRememberMeEnabled", "false"))
            .and(header("x-apple-widget-key", WIDGET_KEY))
            .and(header("x-apple-hc", HC_STAMP))
            .and(body_json(json!({
                "accountName": vector::EMAIL,
                "c": vector::init_body("s2k")["c"],
                "m1": vector::M1_B64,
                "m2": vector::M2_B64,
                "rememberMe": false,
            })))
            .respond_with(response)
            .expect(1)
            .mount(&self.server)
            .await;
    }

    /// The 409 that starts the two-factor step.
    async fn mount_complete_409(&self) {
        self.mount_complete(
            ResponseTemplate::new(409)
                .insert_header("scnt", SCNT)
                .insert_header("X-Apple-ID-Session-Id", SESSION_ID)
                .insert_header(
                    "Set-Cookie",
                    "aasp=pre-2fa-cookie; Path=/; Secure; HttpOnly",
                )
                .set_body_json(json!({"authType": "hsa2"})),
        )
        .await;
    }

    /// A two-factor request, with the headers `update_request_headers` sets.
    fn two_factor(method_name: &str, route: &str) -> wiremock::MockBuilder {
        Mock::given(method(method_name))
            .and(path(route))
            .and(header("scnt", SCNT))
            .and(header("x-apple-id-session-id", SESSION_ID))
            .and(header("x-apple-widget-key", WIDGET_KEY))
            .and(header("accept", "application/json"))
    }

    async fn mount_auth_options(&self, body: Value) {
        Self::two_factor("GET", "/appleauth/auth")
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    async fn mount_trust(&self) {
        Self::two_factor("GET", "/appleauth/auth/2sv/trust")
            .respond_with(
                ResponseTemplate::new(204)
                    .append_header(
                        "Set-Cookie",
                        format!("myacinfo={MYACINFO}; Path=/; Secure; HttpOnly").as_str(),
                    )
                    .append_header(
                        "Set-Cookie",
                        format!(
                            "DES5c148586dfd451e55afb0175f62418f91=\"{TRUST_COOKIE}\"; Path=/; Max-Age=2592000; Secure; HttpOnly"
                        )
                        .as_str(),
                    ),
            )
            .expect(1)
            .mount(&self.server)
            .await;
    }

    async fn mount_olympus(&self) {
        Mock::given(method("GET"))
            .and(path("/olympus/v1/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(olympus_session_body()))
            .mount(&self.server)
            .await;
    }

    async fn requests(&self) -> Vec<Request> {
        self.server.received_requests().await.unwrap()
    }
}

fn password() -> Password {
    Password::new(vector::PASSWORD.to_string())
}

fn team_ids(step: &AppleSignInStep) -> Vec<u64> {
    match step {
        AppleSignInStep::Done { teams } => teams.iter().map(|t| t.provider_id).collect(),
        other => panic!("not done: {other:?}"),
    }
}

/// Every request's URL, headers and body as text, for "never sent" checks.
fn wire_text(requests: &[Request]) -> String {
    requests
        .iter()
        .map(|r| {
            let headers: Vec<String> = r
                .headers
                .iter()
                .map(|(k, v)| format!("{k}: {}", v.to_str().unwrap_or("")))
                .collect();
            format!(
                "{} {}\n{}\n{}",
                r.method,
                r.url,
                headers.join("\n"),
                String::from_utf8_lossy(&r.body)
            )
        })
        .collect::<Vec<_>>()
        .join("\n---\n")
}

#[tokio::test]
async fn signs_in_directly_on_200_with_the_exact_srp_proof_and_hashcash() {
    let apple = MockApple::new().await;
    apple
        .mount_complete(ResponseTemplate::new(200).set_body_json(json!({"authType": "sa"})))
        .await;
    apple.mount_olympus().await;

    let mut sign_in = apple.sign_in();
    let step = sign_in.start(vector::EMAIL, password()).await.unwrap();
    assert_eq!(team_ids(&step), [118_291_181, 222]);
    assert_eq!(sign_in.teams().map(<[AppleTeam]>::len), Some(2));

    let session = sign_in.into_session().unwrap();
    assert_eq!(
        session.team_id(),
        Some(118_291_181),
        "acts for olympus's current team"
    );

    let requests = apple.requests().await;
    let routes: Vec<String> = requests
        .iter()
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect();
    assert_eq!(
        routes,
        [
            "HEAD /logout",
            "POST /appleauth/auth/signin/init",
            "GET /appleauth/auth/signin",
            "POST /appleauth/auth/signin/complete",
            "GET /olympus/v1/session",
        ]
    );
    assert!(
        !wire_text(&requests).contains(vector::PASSWORD),
        "the password never goes on the wire"
    );
}

#[tokio::test]
async fn two_factor_with_a_trusted_device_code_then_trust_then_teams() {
    let apple = MockApple::new().await;
    apple.mount_complete_409().await;
    apple
        .mount_auth_options(auth_options_trusted_devices())
        .await;
    MockApple::two_factor("POST", "/appleauth/auth/verify/trusteddevice/securitycode")
        .and(header("content-type", "application/json"))
        .and(body_json(json!({"securityCode": {"code": "123456"}})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&apple.server)
        .await;
    apple.mount_trust().await;
    apple.mount_olympus().await;

    let mut sign_in = apple.sign_in();
    let step = sign_in.start(vector::EMAIL, password()).await.unwrap();
    let AppleSignInStep::NeedsCode {
        methods,
        phones,
        code_length,
        delivery,
    } = step
    else {
        panic!("expected a code step, got {step:?}");
    };
    assert_eq!(code_length, 6);
    assert_eq!(delivery, AppleCodeDelivery::TrustedDevices);
    assert_eq!(
        methods,
        [
            AppleCodeMethod::TrustedDevice,
            AppleCodeMethod::Phone { phone_id: 1 },
            AppleCodeMethod::Phone { phone_id: 2 },
            AppleCodeMethod::Phone { phone_id: 3 },
        ]
    );
    assert_eq!(phones[0].masked_number, "+49 •••• •••••85");
    assert_eq!(phones[2].mode, "voice");
    assert!(sign_in.teams().is_none());

    let step = sign_in
        .submit_code(" 123456 ", AppleCodeMethod::TrustedDevice)
        .await
        .unwrap();
    assert_eq!(team_ids(&step), [118_291_181, 222]);

    // The trust cookie and the refreshed session cookie are in the blob.
    let session = sign_in.into_session().unwrap();
    let mut names = session.cookie_names();
    names.sort();
    assert_eq!(
        names,
        ["DES5c148586dfd451e55afb0175f62418f91", "aasp", "myacinfo"]
    );
    let restored = AppleSession::restore(&session.to_blob().unwrap(), apple.endpoints()).unwrap();
    assert_eq!(restored.cookie_names().len(), 3);
}

#[tokio::test]
async fn sms_path_with_several_phones_requests_then_submits_a_phone_code() {
    let apple = MockApple::new().await;
    apple.mount_complete_409().await;
    apple
        .mount_auth_options(auth_options_no_trusted_devices(2))
        .await;
    MockApple::two_factor("PUT", "/appleauth/auth/verify/phone")
        .and(header("content-type", "application/json"))
        .and(body_json(json!({"phoneNumber": {"id": 2}, "mode": "sms"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"trustedPhoneNumber": {"id": 2}})),
        )
        .expect(1)
        .mount(&apple.server)
        .await;
    MockApple::two_factor("POST", "/appleauth/auth/verify/phone/securitycode")
        .and(body_json(json!({
            "securityCode": {"code": "654321"},
            "phoneNumber": {"id": 2},
            "mode": "sms",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&apple.server)
        .await;
    apple.mount_trust().await;
    apple.mount_olympus().await;

    let mut sign_in = apple.sign_in();
    let step = sign_in.start(vector::EMAIL, password()).await.unwrap();
    let AppleSignInStep::NeedsCode {
        methods, delivery, ..
    } = &step
    else {
        panic!("{step:?}");
    };
    assert_eq!(*delivery, AppleCodeDelivery::NotSent);
    assert_eq!(
        *methods,
        [
            AppleCodeMethod::Phone { phone_id: 1 },
            AppleCodeMethod::Phone { phone_id: 2 }
        ],
        "no trusted device to show a code"
    );
    assert!(matches!(
        sign_in
            .submit_code("654321", AppleCodeMethod::TrustedDevice)
            .await,
        Err(AppleSignInError::InvalidInput { .. })
    ));

    let step = sign_in.request_sms(2).await.unwrap();
    assert!(matches!(
        step,
        AppleSignInStep::NeedsCode {
            delivery: AppleCodeDelivery::Phone { phone_id: 2 },
            ..
        }
    ));
    let step = sign_in
        .submit_code("654321", AppleCodeMethod::Phone { phone_id: 2 })
        .await
        .unwrap();
    assert_eq!(team_ids(&step), [118_291_181, 222]);
}

#[tokio::test]
async fn a_single_phone_without_trusted_devices_gets_the_code_automatically() {
    let apple = MockApple::new().await;
    apple.mount_complete_409().await;
    apple
        .mount_auth_options(auth_options_no_trusted_devices(1))
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&apple.server)
        .await;

    let mut sign_in = apple.sign_in();
    let step = sign_in.start(vector::EMAIL, password()).await.unwrap();
    assert_eq!(
        step,
        AppleSignInStep::NeedsCode {
            methods: vec![AppleCodeMethod::Phone { phone_id: 1 }],
            phones: vec![AppleTrustedPhone {
                id: 1,
                masked_number: "+1 (•••) •••-••01".to_string(),
                mode: "sms".to_string(),
            }],
            code_length: 6,
            delivery: AppleCodeDelivery::Phone { phone_id: 1 },
        }
    );
}

#[tokio::test]
async fn wrong_password_is_wrong_credentials_and_the_sign_in_can_restart() {
    let apple = MockApple::new().await;
    apple
        .mount_complete(ResponseTemplate::new(401).set_body_json(json!({
            "serviceErrors": [{"code": "-20101", "message": "Your Apple ID or password was incorrect."}]
        })))
        .await;

    let mut sign_in = apple.sign_in();
    let err = sign_in.start(vector::EMAIL, password()).await.unwrap_err();
    assert_eq!(err, AppleSignInError::WrongCredentials);
    assert!(matches!(
        StackError::from(err),
        StackError::InvalidCredentials { .. }
    ));
    // Back to the start: no code is expected.
    let err = sign_in
        .submit_code("123456", AppleCodeMethod::TrustedDevice)
        .await
        .unwrap_err();
    assert!(
        matches!(err, AppleSignInError::WrongStep { ref message } if message.contains("not started"))
    );
}

#[tokio::test]
async fn complete_failures_map_to_specific_errors() {
    let cases: Vec<(ResponseTemplate, AppleSignInError)> = vec![
        (
            ResponseTemplate::new(503),
            AppleSignInError::AppleUnavailable { status: 503 },
        ),
        (ResponseTemplate::new(429), AppleSignInError::RateLimited),
        (
            ResponseTemplate::new(400).set_body_string("<form invalid=\"true\"></form>"),
            AppleSignInError::WrongCredentials,
        ),
        (
            ResponseTemplate::new(403).set_body_json(json!({
                "serviceErrors": [{"code": "-20209", "message": "This Apple ID has been locked for security reasons."}]
            })),
            AppleSignInError::Refused {
                status: 403,
                message: "This Apple ID has been locked for security reasons. (-20209)".to_string(),
            },
        ),
        (
            ResponseTemplate::new(400).insert_header("Set-Cookie", "itctx=abc; Path=/"),
            AppleSignInError::NotEnabledForAppStoreConnect,
        ),
        (
            ResponseTemplate::new(412).set_body_json(json!({"authType": "unknown"})),
            AppleSignInError::unexpected(412, "signin/complete did not sign in"),
        ),
        (
            ResponseTemplate::new(409).set_body_json(json!({"authType": "hsa2"})),
            AppleSignInError::unexpected(
                409,
                "two-factor is required but Apple sent no session headers",
            ),
        ),
    ];
    for (response, expected) in cases {
        let apple = MockApple::new().await;
        apple.mount_complete(response).await;
        let err = apple
            .sign_in()
            .start(vector::EMAIL, password())
            .await
            .unwrap_err();
        assert_eq!(err, expected);
    }
}

#[tokio::test]
async fn http_412_with_an_auth_type_needs_acknowledgement() {
    let apple = MockApple::new().await;
    apple
        .mount_complete(ResponseTemplate::new(412).set_body_json(json!({"authType": "hsa2"})))
        .await;
    let mut sign_in = apple.sign_in();
    assert_eq!(
        sign_in.start(vector::EMAIL, password()).await.unwrap(),
        AppleSignInStep::NeedsAcknowledgement
    );
    assert!(matches!(
        sign_in.start(vector::EMAIL, password()).await,
        Err(AppleSignInError::WrongStep { .. })
    ));
    assert!(matches!(
        sign_in.into_session(),
        Err(AppleSignInError::WrongStep { .. })
    ));
}

#[tokio::test]
async fn init_failures_map_to_specific_errors_before_complete() {
    let cases: Vec<(ResponseTemplate, AppleSignInError)> = vec![
        (ResponseTemplate::new(401), AppleSignInError::WrongCredentials),
        (
            ResponseTemplate::new(502),
            AppleSignInError::AppleUnavailable { status: 502 },
        ),
        (
            ResponseTemplate::new(200).set_body_string("<html>"),
            AppleSignInError::unexpected(200, "signin/init did not answer JSON"),
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({"iteration": 1000, "protocol": "s2k"})),
            AppleSignInError::unexpected(200, "signin/init has no valid \"salt\""),
        ),
        (
            ResponseTemplate::new(200).set_body_json(vector::init_body("s2k_xx")),
            AppleSignInError::srp("unsupported password protocol \"s2k_xx\""),
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({
                "iteration": 2_000_000, "salt": vector::SALT_B64, "protocol": "s2k", "b": vector::B_B64, "c": "c"
            })),
            AppleSignInError::srp("the iteration count 2000000 is out of range"),
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({
                "serviceErrors": [{"code": "-20283", "message": "Your account information was entered incorrectly."}]
            })),
            AppleSignInError::Refused {
                status: 200,
                message: "Your account information was entered incorrectly. (-20283)".to_string(),
            },
        ),
    ];
    for (response, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/logout"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("/s?widgetKey={WIDGET_KEY}").as_str()),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/appleauth/auth/signin/init"))
            .respond_with(response)
            .mount(&server)
            .await;
        Mock::given(path("/appleauth/auth/signin/complete"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
        let err = AppleSignIn::new(endpoints)
            .unwrap()
            .start(vector::EMAIL, password())
            .await
            .unwrap_err();
        assert_eq!(err, expected);
    }
}

#[tokio::test]
async fn s2k_fo_is_derived_when_apple_picks_it_and_no_challenge_means_no_hashcash() {
    let apple = MockApple::with_init("s2k_fo", false).await;
    // The s2k_fo key differs from the s2k vector's, so `m1` does too: this
    // mock checks the request shape only.
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/signin/complete"))
        .and(body_partial_json(
            json!({"accountName": vector::EMAIL, "rememberMe": false}),
        ))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&apple.server)
        .await;
    let err = apple
        .sign_in()
        .start(vector::EMAIL, password())
        .await
        .unwrap_err();
    assert_eq!(err, AppleSignInError::WrongCredentials);

    let requests = apple.requests().await;
    let complete = requests
        .iter()
        .find(|r| r.url.path() == "/appleauth/auth/signin/complete")
        .unwrap();
    assert!(complete.headers.get("x-apple-hc").is_none());
    let body: Value = serde_json::from_slice(&complete.body).unwrap();
    assert_ne!(
        body["m1"],
        json!(vector::M1_B64),
        "s2k_fo derives another key"
    );
    assert_eq!(vector::decoded(body["m1"].as_str().unwrap()).len(), 32);
}

#[tokio::test]
async fn wrong_code_keeps_waiting_then_the_right_code_signs_in() {
    let apple = MockApple::new().await;
    apple.mount_complete_409().await;
    apple
        .mount_auth_options(auth_options_trusted_devices())
        .await;
    MockApple::two_factor("POST", "/appleauth/auth/verify/trusteddevice/securitycode")
        .and(body_json(json!({"securityCode": {"code": "908172"}})))
        .respond_with(ResponseTemplate::new(400).set_body_json(wrong_code_body()))
        .expect(1)
        .mount(&apple.server)
        .await;
    MockApple::two_factor("POST", "/appleauth/auth/verify/trusteddevice/securitycode")
        .and(body_json(json!({"securityCode": {"code": "123456"}})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&apple.server)
        .await;
    apple.mount_trust().await;
    apple.mount_olympus().await;

    let mut sign_in = apple.sign_in();
    sign_in.start(vector::EMAIL, password()).await.unwrap();
    assert_eq!(
        sign_in
            .submit_code("908172", AppleCodeMethod::TrustedDevice)
            .await
            .unwrap_err(),
        AppleSignInError::WrongCode
    );
    let step = sign_in
        .submit_code("123456", AppleCodeMethod::TrustedDevice)
        .await
        .unwrap();
    assert!(matches!(step, AppleSignInStep::Done { .. }));
}

/// The exported object ([`crate::apple_id::AppleIdSignIn`]) over the same
/// mock: the password, a wrong code (`InvalidCredentials`, still waiting), the
/// right code, the teams and a blob that restores; then it is finished.
#[tokio::test]
async fn the_exported_sign_in_object_runs_the_flow_to_a_restorable_blob() {
    let apple = MockApple::new().await;
    apple.mount_complete_409().await;
    apple
        .mount_auth_options(auth_options_trusted_devices())
        .await;
    MockApple::two_factor("POST", "/appleauth/auth/verify/trusteddevice/securitycode")
        .and(body_json(json!({"securityCode": {"code": "908172"}})))
        .respond_with(ResponseTemplate::new(400).set_body_json(wrong_code_body()))
        .expect(1)
        .mount(&apple.server)
        .await;
    MockApple::two_factor("POST", "/appleauth/auth/verify/trusteddevice/securitycode")
        .and(body_json(json!({"securityCode": {"code": "123456"}})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&apple.server)
        .await;
    apple.mount_trust().await;
    apple.mount_olympus().await;

    // Audited, which must change nothing about the flow.
    let sink = crate::audit::testing::RecordingSink::new(true);
    let audit_sink: std::sync::Arc<dyn crate::ports::NetworkAuditSink> = sink.clone();
    let object = crate::apple_id::AppleIdSignIn::from_flow(apple.sign_in(), Some(audit_sink));
    let step = object
        .start(vector::EMAIL.to_string(), vector::PASSWORD.to_string())
        .await
        .unwrap();
    assert!(matches!(step, AppleSignInStep::NeedsCode { .. }));
    assert!(matches!(
        object
            .submit_code("908172".into(), AppleCodeMethod::TrustedDevice)
            .await,
        Err(StackError::InvalidCredentials { message }) if message.contains("verification code")
    ));
    let step = object
        .submit_code("123456".into(), AppleCodeMethod::TrustedDevice)
        .await
        .unwrap();
    assert_eq!(team_ids(&step), [118_291_181, 222]);
    assert_eq!(object.teams().await.len(), 2);

    let blob = object.finish().await.unwrap();
    let restored =
        AppleSession::restore(&SessionBlob::from_bytes(blob), apple.endpoints()).unwrap();
    assert_eq!(restored.team_id(), Some(118_291_181));
    let mut names = restored.cookie_names();
    names.sort();
    assert!(names.contains(&"myacinfo".to_string()));

    assert!(matches!(
        object.finish().await,
        Err(StackError::Auth { message }) if message.contains("finished")
    ));
    assert!(object.teams().await.is_empty());
    assert!(
        !wire_text(&apple.requests().await).contains(vector::PASSWORD),
        "the password never goes on the wire"
    );

    // One entry per request Apple saw, each tagged with the step that sent
    // it, the web session provider and no account (none exists yet).
    let entries = sink.entries();
    assert_eq!(entries.len(), apple.requests().await.len());
    let mut ops: Vec<&str> = entries.iter().map(|e| e.operation.as_str()).collect();
    ops.dedup();
    assert_eq!(ops, ["apple_id.start", "apple_id.submit_code"]);
    assert!(entries.iter().all(|e| {
        e.provider == crate::audit::NetworkAuditProvider::AppleWebSession
            && e.account_id.is_none()
            && e.attempt == 1
    }));
    // The password itself is never in an entry either (SRP sends a proof).
    assert!(entries.iter().all(|e| !e
        .request_body
        .as_deref()
        .unwrap_or("")
        .contains(vector::PASSWORD)));
}

#[tokio::test]
async fn local_code_checks_send_nothing() {
    let apple = MockApple::new().await;
    apple.mount_complete_409().await;
    apple
        .mount_auth_options(auth_options_trusted_devices())
        .await;
    Mock::given(path_regex("^/appleauth/auth/verify/"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&apple.server)
        .await;

    let mut sign_in = apple.sign_in();
    sign_in.start(vector::EMAIL, password()).await.unwrap();
    for code in ["", "12345", "1234567", "12a456", "１２３４５６"] {
        let err = sign_in
            .submit_code(code, AppleCodeMethod::TrustedDevice)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            AppleSignInError::invalid_input("the code must be 6 digits"),
            "{code:?}"
        );
    }
    assert!(matches!(
        sign_in.submit_code("123456", AppleCodeMethod::Phone { phone_id: 99 }).await,
        Err(AppleSignInError::InvalidInput { ref message }) if message.contains("99")
    ));
    assert!(matches!(
        sign_in.request_sms(99).await,
        Err(AppleSignInError::InvalidInput { .. })
    ));
}

#[tokio::test]
async fn an_expired_two_factor_session_resets_the_sign_in() {
    let apple = MockApple::new().await;
    apple.mount_complete_409().await;
    apple
        .mount_auth_options(auth_options_trusted_devices())
        .await;
    MockApple::two_factor("POST", "/appleauth/auth/verify/trusteddevice/securitycode")
        .respond_with(ResponseTemplate::new(401))
        .mount(&apple.server)
        .await;

    let mut sign_in = apple.sign_in();
    sign_in.start(vector::EMAIL, password()).await.unwrap();
    assert_eq!(
        sign_in
            .submit_code("123456", AppleCodeMethod::TrustedDevice)
            .await
            .unwrap_err(),
        AppleSignInError::SignInExpired
    );
    assert!(matches!(
        sign_in.request_sms(1).await,
        Err(AppleSignInError::WrongStep { ref message }) if message.contains("not started")
    ));
}

#[tokio::test]
async fn code_answers_map_to_specific_outcomes() {
    let cases: Vec<(ResponseTemplate, Result<AppleSignInStep, AppleSignInError>)> = vec![
        (
            ResponseTemplate::new(412),
            Ok(AppleSignInStep::NeedsAcknowledgement),
        ),
        (
            ResponseTemplate::new(400).set_body_json(json!({"securityCodeLocked": true})),
            Err(AppleSignInError::TwoFactorUnavailable {
                message: "too many incorrect codes were entered; try again later".to_string(),
            }),
        ),
        (
            ResponseTemplate::new(503),
            Err(AppleSignInError::AppleUnavailable { status: 503 }),
        ),
        (
            ResponseTemplate::new(400).set_body_json(
                json!({"serviceErrors": [{"code": "-1", "message": "Something else"}]}),
            ),
            Err(AppleSignInError::Refused {
                status: 400,
                message: "Something else (-1)".to_string(),
            }),
        ),
    ];
    for (response, expected) in cases {
        let apple = MockApple::new().await;
        apple.mount_complete_409().await;
        apple
            .mount_auth_options(auth_options_trusted_devices())
            .await;
        MockApple::two_factor("POST", "/appleauth/auth/verify/trusteddevice/securitycode")
            .respond_with(response)
            .mount(&apple.server)
            .await;
        let mut sign_in = apple.sign_in();
        sign_in.start(vector::EMAIL, password()).await.unwrap();
        assert_eq!(
            sign_in
                .submit_code("123456", AppleCodeMethod::TrustedDevice)
                .await,
            expected
        );
    }
}

#[tokio::test]
async fn sms_that_cannot_be_sent_is_two_factor_unavailable() {
    let apple = MockApple::new().await;
    apple.mount_complete_409().await;
    apple
        .mount_auth_options(auth_options_no_trusted_devices(2))
        .await;
    MockApple::two_factor("PUT", "/appleauth/auth/verify/phone")
        .respond_with(ResponseTemplate::new(200).set_body_json(sms_cannot_be_sent()))
        .mount(&apple.server)
        .await;

    let mut sign_in = apple.sign_in();
    sign_in.start(vector::EMAIL, password()).await.unwrap();
    let err = sign_in.request_sms(1).await.unwrap_err();
    assert!(
        matches!(&err, AppleSignInError::TwoFactorUnavailable { message } if message.contains("(-28248)")),
        "{err}"
    );
    // Still waiting for a code: another phone may work.
    assert!(matches!(
        sign_in
            .submit_code("123", AppleCodeMethod::Phone { phone_id: 1 })
            .await,
        Err(AppleSignInError::InvalidInput { .. })
    ));
}

#[tokio::test]
async fn unsupported_second_factors_and_odd_options_are_named() {
    let cases = [
        (
            json!({"fsaChallenge": {"challenge": "abc", "keyHandles": ["k"]}, "keyNames": ["YubiKey"], "trustedPhoneNumbers": [{"id": 1}]}),
            AppleSignInError::SecurityKeyRequired,
        ),
        (
            json!({"keyNames": ["Key 1"]}),
            AppleSignInError::SecurityKeyRequired,
        ),
        (
            json!({"trustedDevices": [{"id": "d1", "name": "iPhone"}]}),
            AppleSignInError::TwoStepUnsupported,
        ),
        (
            json!({"noTrustedDevices": true, "serviceErrors": [{"code": "-28248", "message": "Codes can't be sent."}]}),
            AppleSignInError::TwoFactorUnavailable {
                message: "Codes can't be sent. (-28248). This Apple ID has no trusted devices: signing in to it on an Apple device adds one".to_string(),
            },
        ),
        (
            json!({"authType": "hsa2"}),
            AppleSignInError::unexpected(
                200,
                "Apple asked for two-factor authentication without a way to verify",
            ),
        ),
        (
            json!({"trustedPhoneNumbers": [{"id": 1}], "securityCode": {"length": 99}}),
            AppleSignInError::unexpected(200, "the code length is out of range"),
        ),
        (
            json!({"trustedPhoneNumbers": [{"numberWithDialCode": "+1"}]}),
            AppleSignInError::unexpected(200, "Apple listed trusted phones without ids"),
        ),
        (
            json!({"trustedPhoneNumbers": [{"id": 1}], "securityCode": {"length": 6, "tooManyCodesLock": true}}),
            AppleSignInError::TwoFactorUnavailable {
                message: "too many codes were requested or entered; try again later".to_string(),
            },
        ),
    ];
    for (body, expected) in cases {
        let apple = MockApple::new().await;
        apple.mount_complete_409().await;
        apple.mount_auth_options(body).await;
        let err = apple
            .sign_in()
            .start(vector::EMAIL, password())
            .await
            .unwrap_err();
        assert_eq!(err, expected);
    }
}

#[tokio::test]
async fn steps_out_of_order_and_empty_input_are_refused_locally() {
    // Nothing listens here: a request would fail with a network error.
    let endpoints = AppleEndpoints::with_base_urls("http://127.0.0.1:1", "http://127.0.0.1:1");
    let mut sign_in = AppleSignIn::new(endpoints).unwrap();
    assert!(matches!(
        sign_in.request_sms(1).await,
        Err(AppleSignInError::WrongStep { .. })
    ));
    assert!(matches!(
        sign_in
            .submit_code("123456", AppleCodeMethod::TrustedDevice)
            .await,
        Err(AppleSignInError::WrongStep { .. })
    ));
    assert!(sign_in.teams().is_none());
    assert_eq!(
        sign_in.start("  ", password()).await.unwrap_err(),
        AppleSignInError::invalid_input("the Apple ID is empty")
    );
    assert_eq!(
        sign_in
            .start(vector::EMAIL, Password::new(String::new()))
            .await
            .unwrap_err(),
        AppleSignInError::invalid_input("the password is empty")
    );
    assert!(matches!(
        sign_in.into_session(),
        Err(AppleSignInError::WrongStep { .. })
    ));
}

#[tokio::test]
async fn signing_in_again_over_a_saved_session_sends_the_trust_cookie_quoted() {
    let apple = MockApple::new().await;
    apple.mount_complete(ResponseTemplate::new(200)).await;
    apple.mount_olympus().await;

    // A saved session whose jar holds the trust cookie unquoted.
    let saved = session_on(&apple.endpoints(), Some("old-widget-key".to_string()));
    let url = reqwest::Url::parse(&apple.server.uri()).unwrap();
    let cookies = [HeaderValue::from_static(
        "DES5c148586=HSARMtrust==SRVT; Path=/; Max-Age=2592000",
    )];
    saved.jar().set_cookies(&mut cookies.iter(), &url);

    let mut sign_in = AppleSignIn::from_session(saved)
        .with_srp_private(vector::private_a())
        .with_clock(fixed_clock);
    let step = sign_in.start(vector::EMAIL, password()).await.unwrap();
    assert!(
        matches!(step, AppleSignInStep::Done { .. }),
        "the trust cookie skipped the code"
    );

    let requests = apple.requests().await;
    for route in [
        "/appleauth/auth/signin/init",
        "/appleauth/auth/signin/complete",
    ] {
        let request = requests.iter().find(|r| r.url.path() == route).unwrap();
        assert_eq!(
            request.headers.get("cookie").unwrap().to_str().unwrap(),
            "DES5c148586=\"HSARMtrust==SRVT\"",
            "{route}"
        );
    }
    let logout = requests.iter().find(|r| r.url.path() == "/logout").unwrap();
    assert!(
        logout.headers.get("cookie").is_none(),
        "the sign-out probe never carries the session"
    );
}

#[tokio::test]
async fn olympus_refusal_after_sign_in_resets_the_flow() {
    let apple = MockApple::new().await;
    apple.mount_complete(ResponseTemplate::new(200)).await;
    Mock::given(method("GET"))
        .and(path("/olympus/v1/session"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&apple.server)
        .await;
    let mut sign_in = apple.sign_in();
    assert_eq!(
        sign_in.start(vector::EMAIL, password()).await.unwrap_err(),
        AppleSignInError::SessionRejected { status: 401 }
    );
    assert!(matches!(
        sign_in.into_session(),
        Err(AppleSignInError::WrongStep { .. })
    ));
}

/// The password, the codes, the session headers, the cookie values and the
/// SRP values never reach the host log sink, an error message or a `Debug`
/// rendering, on the success path and on the failure paths.
#[tokio::test]
async fn secrets_never_reach_logs_errors_or_debug_output() {
    let logs = capture_logs();
    let mut rendered: Vec<String> = Vec::new();

    // Full two-factor sign-in with one wrong code, whose answer echoes it.
    {
        let apple = MockApple::new().await;
        apple.mount_complete_409().await;
        apple
            .mount_auth_options(auth_options_trusted_devices())
            .await;
        MockApple::two_factor("POST", "/appleauth/auth/verify/trusteddevice/securitycode")
            .and(body_json(json!({"securityCode": {"code": "908172"}})))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "securityCode": {"code": "908172"},
                "service_errors": [{"code": "-21669", "message": "Incorrect verification code."}]
            })))
            .mount(&apple.server)
            .await;
        MockApple::two_factor("POST", "/appleauth/auth/verify/trusteddevice/securitycode")
            .and(body_json(json!({"securityCode": {"code": "123456"}})))
            .respond_with(ResponseTemplate::new(204))
            .mount(&apple.server)
            .await;
        apple.mount_trust().await;
        apple.mount_olympus().await;

        let mut sign_in = apple.sign_in();
        let step = sign_in.start(vector::EMAIL, password()).await.unwrap();
        rendered.push(format!("{step:?} {sign_in:?}"));
        let err = sign_in
            .submit_code("908172", AppleCodeMethod::TrustedDevice)
            .await
            .unwrap_err();
        rendered.push(format!("{err} {err:?} {}", StackError::from(err.clone())));
        let step = sign_in
            .submit_code("123456", AppleCodeMethod::TrustedDevice)
            .await
            .unwrap();
        rendered.push(format!("{step:?} {sign_in:?}"));
        let session = sign_in.into_session().unwrap();
        let blob = session.to_blob().unwrap();
        rendered.push(format!("{session:?} {blob:?} {:?}", session.cookie_names()));
        assert!(
            String::from_utf8_lossy(blob.as_bytes()).contains(TRUST_COOKIE),
            "the blob itself does carry the cookies"
        );
        assert!(!wire_text(&apple.requests().await).contains(vector::PASSWORD));
    }

    // Wrong password.
    {
        let apple = MockApple::new().await;
        apple
            .mount_complete(ResponseTemplate::new(401).set_body_json(json!({
                "serviceErrors": [{"code": "-20101", "message": "Your Apple ID or password was incorrect."}]
            })))
            .await;
        let err = apple
            .sign_in()
            .start(vector::EMAIL, password())
            .await
            .unwrap_err();
        rendered.push(format!("{err} {err:?} {}", StackError::from(err.clone())));
    }

    rendered.push(format!("{:?}", password()));
    let text = format!("{}\n{}", logs.all_text(), rendered.join("\n"));
    assert!(!logs.records().is_empty(), "the flow does log");
    for secret in [
        vector::PASSWORD,
        "123456",
        "908172",
        SCNT,
        SESSION_ID,
        MYACINFO,
        TRUST_COOKIE,
        "pre-2fa-cookie",
        HC_STAMP,
        vector::M1_B64,
        vector::EMAIL,
    ] {
        assert!(!text.contains(secret), "{secret:?} leaked into:\n{text}");
    }
}

/// The futures are `Send`, so a multi-threaded runtime (and the UniFFI async
/// export of Phase 2) can drive them.
#[test]
fn sign_in_futures_are_send() {
    fn assert_send<T: Send>(_: &T) {}
    let endpoints = AppleEndpoints::with_base_urls("http://127.0.0.1:1", "http://127.0.0.1:1");
    let mut sign_in = AppleSignIn::new(endpoints.clone()).unwrap();
    assert_send(&sign_in.start(vector::EMAIL, password()));
    assert_send(&sign_in.request_sms(1));
    assert_send(&sign_in.submit_code("123456", AppleCodeMethod::TrustedDevice));
    let mut session = session_on(&endpoints, None);
    assert_send(&session.olympus_session());
    assert_send(&session.select_team(1));
    assert_send(&session.app_store_connect_get("iris/v1/apps"));
}
