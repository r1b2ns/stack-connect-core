//! Shared test fixtures for the Apple ID session tests.
//!
//! Response shapes are taken from fastlane's spaceship code and spec fixtures
//! (`client_appleauth_auth_2fa_response.json`,
//! `appleauth_2fa_no_trusted_devices*.json`,
//! `appleauth_2fa_sms_cannot_be_sent.json`) and trimmed to the fields the
//! client reads; every value is made up.

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use num_bigint::BigUint;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::session::{AppleSession, SessionJar};
use super::{transport, AppleEndpoints};

/// A client over a fresh jar.
pub(crate) fn session_client() -> reqwest::Client {
    transport::session_client(Arc::new(SessionJar::default())).unwrap()
}

/// A session with an empty jar on `endpoints`.
pub(crate) fn session_on(endpoints: &AppleEndpoints, widget_key: Option<String>) -> AppleSession {
    let jar = Arc::new(SessionJar::default());
    let http = transport::session_client(Arc::clone(&jar)).unwrap();
    AppleSession::from_parts(endpoints.clone(), jar, http, widget_key, None)
}

/// `GET olympus/v1/session` for an Apple ID in two teams, acting for the
/// first.
pub(crate) fn olympus_session_body() -> Value {
    json!({
        "user": {"emailAddress": "dev@example.com", "fullName": "Dev Example"},
        "provider": {"providerId": 118_291_181, "name": "Main Team", "contentTypes": ["SOFTWARE"]},
        "availableProviders": [
            {"providerId": 118_291_181, "name": "Main Team", "contentTypes": ["SOFTWARE"], "publicProviderId": "11111111-2222-3333-4444-555555555555"},
            {"providerId": 222, "name": "Second Team", "contentTypes": ["SOFTWARE"]}
        ],
        "permittedActivities": {"VIEW": ["ITC"]}
    })
}

/// The `s2k` vector of `srp.rs` (made with the `fastlane-sirp` gem; see the
/// comment on `srp::tests::Vector`).
pub(crate) mod vector {
    use super::*;

    pub(crate) const EMAIL: &str = "dev@example.com";
    pub(crate) const PASSWORD: &str = "correct horse battery staple";
    pub(crate) const ITERATIONS: u32 = 1000;
    pub(crate) const SALT_B64: &str = "AQIDBAUGBwgJCgsMDQ4PEA==";
    pub(crate) const B_B64: &str = "KFq55tNyVFx8RnY4IbByCQrE3bpnt0HDIoQQFXrHcP8lfXTZhNJE2jmqrxVwAWXgTJlhc1WwsupIaEidXafy5WfPeXiK68peE2h/TK020w0P+/BRTEweYo6sCAvN8ynQL4l96tYKSGz1OHdM/kSmOWAmyNZzbQGo19CbfxgflDycLy2cAjR4vvOkZ3ZqVvR5KYZX6lehmSrEJcguHWDZhBh7EswWYWVZvgLCpaZ9v3x2RXIpg2grpiVpWt0fF6fvoyGkR9mAsXWG2fkgZzcarqIqE93b6s0wA8413QdFoRz6sgzyvp2p70KlWMZKDFpcUsXwzOqvKqd8wtt4AConng==";
    pub(crate) const A_B64: &str = "lmtIypRf5cgVPs3QQQ6zJmpmcGkFpCmJftOtl5kKV5qjzOrqdHZHx0L1P3E2Gu9qC4ElCpROrz6FyLceiKemLL+rkdF6Ej36aOBYqd+JnJrsu4VXQPHdUKVfX+anrZQbXSppYMyWdsC+9YiBT8bj8qmomT9UTFyDDtO7i02A7JnsLGTsvz6Luf8iEb5a9OtHwf+9EEcWLezQpzykTLdOfldqMmowCn1xSrxNKN+p93Mulfw32YB56bSQ3132LwMug61LfTl2ZLmsOVObcpGmuSbrlXzRhhdZCDj/lpnEM37YO+hBr49g5orHK6096qFRP9tnmWTvx20uc5HBVMTNNw==";
    pub(crate) const M1_B64: &str = "eoQOtn2nXrNUAUEJSukyyhe/VH7KIv4f0xmMOd/ggUw=";
    pub(crate) const M2_B64: &str = "Mya8Xp1XbUXpF9+iShVBVPR6ke1qYtT9M5FTNOHxYtQ=";

    /// The private ephemeral of the vector: hex(SHA256("a-seed-1")) × 8.
    pub(crate) fn private_a() -> Vec<u8> {
        let digest: String = Sha256::digest(b"a-seed-1")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        BigUint::parse_bytes(digest.repeat(8).as_bytes(), 16)
            .unwrap()
            .to_bytes_be()
    }

    /// `signin/init`'s answer for the vector.
    pub(crate) fn init_body(protocol: &str) -> Value {
        json!({
            "iteration": ITERATIONS,
            "salt": SALT_B64,
            "protocol": protocol,
            "b": B_B64,
            "c": "d-74-8a7f2c1e-0b0b-11f0-9c6b-0123456789ab:PRN"
        })
    }

    pub(crate) fn decoded(text: &str) -> Vec<u8> {
        STANDARD.decode(text).unwrap()
    }
}

/// `GET appleauth/auth` for an hsa2 account with trusted devices and three
/// phones (`client_appleauth_auth_2fa_response.json`).
pub(crate) fn auth_options_trusted_devices() -> Value {
    json!({
        "trustedPhoneNumbers": [
            {"numberWithDialCode": "+49 •••• •••••85", "pushMode": "sms", "obfuscatedNumber": "•••• •••••85", "id": 1},
            {"numberWithDialCode": "+49 ••••• •••••81", "pushMode": "sms", "obfuscatedNumber": "••••• •••••81", "id": 2},
            {"numberWithDialCode": "+49 ••••• •••••80", "pushMode": "voice", "obfuscatedNumber": "••••• •••••80", "id": 3}
        ],
        "securityCode": {"length": 6, "tooManyCodesSent": false, "tooManyCodesValidated": false, "securityCodeLocked": false},
        "authenticationType": "hsa2",
        "hsa2Account": true,
        "supportsRecovery": true
    })
}

/// No trusted device and `phones` phones (`appleauth_2fa_no_trusted_devices*.json`).
pub(crate) fn auth_options_no_trusted_devices(phones: usize) -> Value {
    let numbers: Vec<Value> = (1..=phones)
        .map(|id| json!({"numberWithDialCode": format!("+1 (•••) •••-••{id:02}"), "pushMode": "sms", "id": id}))
        .collect();
    json!({
        "trustedPhoneNumbers": numbers,
        "securityCode": {"length": 6, "securityCodeLocked": false, "securityCodeCooldown": false},
        "noTrustedDevices": true,
        "authenticationType": "hsa2",
        "hsa2Account": true
    })
}

/// `PUT verify/phone` when Apple cannot text the number
/// (`appleauth_2fa_sms_cannot_be_sent.json`).
pub(crate) fn sms_cannot_be_sent() -> Value {
    json!({
        "success": 0,
        "phoneNumber": {"numberWithDialCode": "+1 (•••) •••-••00", "pushMode": "sms", "id": 1},
        "noTrustedDevices": true,
        "serviceErrors": [{
            "code": "-28248",
            "title": "Verification Failed",
            "message": "Verification codes can’t be sent to this phone number at this time. Please try again later."
        }]
    })
}

/// A wrong code (`handle_two_factor`'s comment).
pub(crate) fn wrong_code_body() -> Value {
    json!({
        "service_errors": [{"code": "-21669", "title": "Incorrect Verification Code", "message": "Incorrect verification code."}],
        "hasError": true
    })
}
