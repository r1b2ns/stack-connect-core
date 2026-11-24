//! The App Store Connect widget key (`X-Apple-Widget-Key`, spaceship's
//! `itc_service_key`).
//!
//! Sources, in order (`itc_service_key` in fastlane master):
//! 1. `HEAD appstoreconnect.apple.com/logout`, which answers a redirect whose
//!    `Location` carries `widgetKey=…` ([`fetch_from_signout`]);
//! 2. a key the caller already knows (the one a saved session signed in with);
//! 3. `GET olympus/v1/app/config?hostname=itunesconnect.apple.com`
//!    (`authServiceKey`), which Apple removed in September 2026 and answers
//!    404 since; kept as the last resort in case it comes back
//!    ([`fetch_from_olympus_config`]).
//!
//! The sign-out probe must use a **bare** client: no cookie store and no
//! redirect following. The redirect points at a sign-out with
//! `asop=destroy-session`; sending the session's cookies, or following it,
//! would end the very session being established.

use reqwest::Url;
use serde_json::Value;

use super::error::AppleSignInError;
use super::transport;
use super::AppleEndpoints;
use crate::logging::{self, target};

/// Widget keys longer than this are not believed (Apple's are 64 hex chars).
const MAX_KEY_LEN: usize = 256;

/// The `widgetKey` query parameter of a sign-out redirect, resolved against
/// `base` when `location` is relative. `None` when there is none, it is
/// empty or too long, or it is not visible ASCII (it goes into a header).
pub(crate) fn parse_location(location: &str, base: &str) -> Option<String> {
    let base = Url::parse(base).ok()?;
    let url = base.join(location.trim()).ok()?;
    let key = url
        .query_pairs()
        .find(|(name, _)| name == "widgetKey")
        .map(|(_, value)| value.into_owned())?;
    valid_key(&key).then_some(key)
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY_LEN && key.bytes().all(|b| b.is_ascii_graphic())
}

/// `fetch_service_key_from_signout`: the key from the `HEAD /logout`
/// redirect, or `None` on any failure (logged), so the caller falls back.
pub(crate) async fn fetch_from_signout(endpoints: &AppleEndpoints) -> Option<String> {
    let client = match transport::bare_client() {
        Ok(client) => client,
        Err(err) => {
            logging::warn(target::AUTH, || {
                format!("widget key: no HTTP client: {err}")
            });
            return None;
        }
    };
    // The bare client sends no session cookie.
    let probe = crate::audit::scope::credential(
        crate::audit::NetworkAuditCredentialKind::Unauthenticated,
        transport::send(client.head(endpoints.app_store_connect("logout"))),
    );
    let reply = match probe.await {
        Ok(reply) => reply,
        Err(err) => {
            logging::warn(target::AUTH, || {
                format!("widget key: the sign-out redirect failed ({err}); falling back")
            });
            return None;
        }
    };
    let key = reply
        .header("location")
        .and_then(|location| parse_location(location, endpoints.app_store_connect_base()));
    if key.is_none() {
        logging::warn(target::AUTH, || {
            format!(
                "widget key: HTTP {} from the sign-out redirect carried no widgetKey; falling back",
                reply.code()
            )
        });
    }
    key
}

/// `fetch_service_key_from_olympus` + `extract_service_key`.
///
/// # Errors
/// [`AppleSignInError::WidgetKeyUnavailable`] for a non-2xx answer or a
/// body without `authServiceKey`; [`AppleSignInError::Network`] on
/// transport failure.
pub(crate) async fn fetch_from_olympus_config(
    http: &reqwest::Client,
    endpoints: &AppleEndpoints,
) -> Result<String, AppleSignInError> {
    let reply = transport::send(
        http.get(
            endpoints.app_store_connect("olympus/v1/app/config?hostname=itunesconnect.apple.com"),
        )
        .header("Accept", "application/json"),
    )
    .await?;
    if !reply.status.is_success() {
        logging::warn(target::AUTH, || {
            format!(
                "widget key: HTTP {} from olympus/v1/app/config",
                reply.code()
            )
        });
        return Err(AppleSignInError::WidgetKeyUnavailable);
    }
    reply
        .json()
        .as_ref()
        .and_then(|body| body.get("authServiceKey"))
        .and_then(Value::as_str)
        .filter(|key| valid_key(key))
        .map(str::to_string)
        .ok_or(AppleSignInError::WidgetKeyUnavailable)
}

/// The key from the first source that has one (see the module docs).
///
/// # Errors
/// [`AppleSignInError::WidgetKeyUnavailable`] when no source yields a key.
pub(crate) async fn resolve(
    http: &reqwest::Client,
    endpoints: &AppleEndpoints,
    known: Option<&str>,
) -> Result<String, AppleSignInError> {
    if let Some(key) = fetch_from_signout(endpoints).await {
        logging::debug(target::AUTH, || {
            "widget key: read from the sign-out redirect".to_string()
        });
        return Ok(key);
    }
    if let Some(key) = known.filter(|key| valid_key(key)) {
        logging::warn(target::AUTH, || {
            "widget key: using the key the saved session signed in with".to_string()
        });
        return Ok(key.to_string());
    }
    fetch_from_olympus_config(http, endpoints).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::apple_session::test_support::session_client;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const KEY: &str = "e0b80c3bf78523bfe80974d320935bfa30add02e1bff88ec2166c6bd5a706c42";

    #[test]
    fn parse_location_reads_widget_key_from_absolute_and_relative_redirects() {
        let base = "https://appstoreconnect.apple.com";
        assert_eq!(
            parse_location(
                &format!("https://idmsa.apple.com/IDMSWebAuth/signout?path=%2F&widgetKey={KEY}&asop=destroy-session"),
                base
            ),
            Some(KEY.to_string())
        );
        assert_eq!(
            parse_location(&format!("/signout?widgetKey={KEY}"), base),
            Some(KEY.to_string())
        );
        for location in [
            "",
            "https://idmsa.apple.com/signout",
            "https://idmsa.apple.com/signout?widgetKey=",
            "https://idmsa.apple.com/signout?widgetkey=abc",
            "https://idmsa.apple.com/signout?widgetKey=a%20b",
            "http://[::1",
        ] {
            assert_eq!(parse_location(location, base), None, "{location:?}");
        }
        let long = "k".repeat(MAX_KEY_LEN + 1);
        assert_eq!(parse_location(&format!("/s?widgetKey={long}"), base), None);
    }

    #[tokio::test]
    async fn signout_probe_sends_no_cookies_and_does_not_follow_the_redirect() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/logout"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header(
                        "Location",
                        format!(
                            "{}/signout?widgetKey={KEY}&asop=destroy-session",
                            server.uri()
                        )
                        .as_str(),
                    )
                    .insert_header("Set-Cookie", "probe=1; Path=/"),
            )
            .expect(2)
            .mount(&server)
            .await;
        // Following the redirect would sign the session out.
        Mock::given(path("/signout"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());

        assert_eq!(fetch_from_signout(&endpoints).await, Some(KEY.to_string()));
        assert_eq!(fetch_from_signout(&endpoints).await, Some(KEY.to_string()));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        // The second probe would carry `probe=1` if the client had a jar.
        assert!(requests.iter().all(|r| r.headers.get("cookie").is_none()));
    }

    #[tokio::test]
    async fn signout_probe_failures_fall_back_to_none() {
        for response in [
            ResponseTemplate::new(200),
            ResponseTemplate::new(302).insert_header("Location", "/signout"),
            ResponseTemplate::new(503),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("HEAD"))
                .and(path("/logout"))
                .respond_with(response)
                .mount(&server)
                .await;
            let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
            assert_eq!(fetch_from_signout(&endpoints).await, None);
        }
        // Nothing listening at all.
        let endpoints = AppleEndpoints::with_base_urls("http://127.0.0.1:1", "http://127.0.0.1:1");
        assert_eq!(fetch_from_signout(&endpoints).await, None);
    }

    #[tokio::test]
    async fn resolve_prefers_the_signout_redirect() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/logout"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("/signout?widgetKey={KEY}").as_str()),
            )
            .mount(&server)
            .await;
        Mock::given(path("/olympus/v1/app/config"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
        let key = resolve(&session_client(), &endpoints, Some("known"))
            .await
            .unwrap();
        assert_eq!(key, KEY);
    }

    #[tokio::test]
    async fn resolve_falls_back_to_the_known_key_then_olympus() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/logout"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/olympus/v1/app/config"))
            .and(query_param("hostname", "itunesconnect.apple.com"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"authServiceKey": "from-olympus"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
        let http = session_client();
        assert_eq!(
            resolve(&http, &endpoints, Some("known-key")).await.unwrap(),
            "known-key"
        );
        assert_eq!(
            resolve(&http, &endpoints, None).await.unwrap(),
            "from-olympus"
        );
    }

    #[tokio::test]
    async fn resolve_fails_when_every_source_fails() {
        for config in [
            ResponseTemplate::new(404),
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"authServiceKey": ""})),
            ResponseTemplate::new(200).set_body_string("<html>"),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("HEAD"))
                .and(path("/logout"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/olympus/v1/app/config"))
                .respond_with(config)
                .mount(&server)
                .await;
            let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
            assert_eq!(
                resolve(&session_client(), &endpoints, None)
                    .await
                    .unwrap_err(),
                AppleSignInError::WidgetKeyUnavailable
            );
        }
    }
}
