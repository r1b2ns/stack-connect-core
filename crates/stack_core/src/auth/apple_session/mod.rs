//! Apple ID web sign-in (email, password, two-factor code), the way fastlane's
//! `spaceship` signs in, and the cookie session it produces.
//!
//! The wire protocol follows fastlane master (`spaceship/lib/spaceship/`):
//! `client.rb` (`do_sirp`, `send_shared_login_request`, `fetch_hashcash`,
//! `itc_service_key`, `fetch_olympus_session`, `team_id=`),
//! `two_step_or_factor_client.rb` and `hashcash.rb`, plus the `fastlane-sirp`
//! 1.0.0 gem for SRP. Where Apple's behavior is only known from that code, the
//! module says so next to the rule.
//!
//! Pieces:
//! - [`widget_key`]: the `X-Apple-Widget-Key`, read from the `HEAD /logout`
//!   redirect with a bare client (no cookies, no redirect following);
//! - [`hashcash`]: the `X-Apple-HC` proof of work;
//! - [`srp`]: the SRP-6a client and Apple's `s2k` / `s2k_fo` password
//!   derivation;
//! - [`flow`]: the sign-in state machine ([`flow::AppleSignIn`]);
//! - [`session`]: the cookie jar, the versioned session blob, the olympus
//!   session and team switching ([`session::AppleSession`]);
//! - [`portal`]: the Developer Portal teams and spaceship's proxy form of the
//!   provisioning requests on `services-account`;
//! - [`transport`]: the HTTP plumbing shared by the above.
//!
//! Secrets: the password is held in a [`srp::Password`] (zeroized on drop, never
//! `Display`ed, `Debug` redacted) and only its derived key ever leaves it; the
//! password itself never goes on the wire (SRP). Requests made here never go
//! to the opt-in debug cURL sink, and their host log lines carry only the
//! method, host, path, status and duration (see [`transport`]).

pub(crate) mod error;
pub(crate) mod flow;
pub(crate) mod hashcash;
pub(crate) mod portal;
#[cfg(any(feature = "apple-signin-probe", test))]
pub(crate) mod probe;
pub(crate) mod session;
pub(crate) mod srp;
pub(crate) mod transport;
pub(crate) mod widget_key;

/// Apple's sign-in service (SRP, two-factor, trust).
pub(crate) const DEFAULT_IDMSA_BASE_URL: &str = "https://idmsa.apple.com";
/// App Store Connect (widget key, olympus session, iris).
pub(crate) const DEFAULT_APP_STORE_CONNECT_BASE_URL: &str = "https://appstoreconnect.apple.com";
/// The Apple Developer site (`services-account`: the Developer Portal teams
/// and provisioning with a session).
pub(crate) const DEFAULT_DEVELOPER_BASE_URL: &str = "https://developer.apple.com";

/// The hosts the sign-in and the session talk to. [`Default`] is Apple's
/// production hosts; tests point them at a mock server with
/// [`Self::with_base_urls`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppleEndpoints {
    idmsa: String,
    app_store_connect: String,
    developer: String,
}

impl Default for AppleEndpoints {
    fn default() -> Self {
        Self::with_base_urls(DEFAULT_IDMSA_BASE_URL, DEFAULT_APP_STORE_CONNECT_BASE_URL)
    }
}

impl AppleEndpoints {
    /// Endpoints rooted at the given base URLs (scheme and host, optionally a
    /// port; a trailing `/` is ignored).
    pub fn with_base_urls(idmsa: &str, app_store_connect: &str) -> Self {
        Self {
            idmsa: idmsa.trim_end_matches('/').to_string(),
            app_store_connect: app_store_connect.trim_end_matches('/').to_string(),
            developer: DEFAULT_DEVELOPER_BASE_URL.to_string(),
        }
    }

    /// The same endpoints with the Apple Developer site at `developer`.
    #[cfg(any(feature = "apple-signin-probe", test))]
    pub fn with_developer_base_url(mut self, developer: &str) -> Self {
        self.developer = developer.trim_end_matches('/').to_string();
        self
    }

    /// `{developer}/{path}` (`path` without a leading `/`).
    pub(crate) fn developer(&self, path: &str) -> String {
        format!("{}/{path}", self.developer)
    }

    /// `{idmsa}/{path}` (`path` without a leading `/`).
    pub(crate) fn idmsa(&self, path: &str) -> String {
        format!("{}/{path}", self.idmsa)
    }

    /// `{app_store_connect}/{path}` (`path` without a leading `/`).
    pub(crate) fn app_store_connect(&self, path: &str) -> String {
        format!("{}/{path}", self.app_store_connect)
    }

    /// The App Store Connect base URL itself.
    pub(crate) fn app_store_connect_base(&self) -> &str {
        &self.app_store_connect
    }
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_default_to_apple_production_hosts() {
        let endpoints = AppleEndpoints::default();
        assert_eq!(
            endpoints.idmsa("appleauth/auth"),
            "https://idmsa.apple.com/appleauth/auth"
        );
        assert_eq!(
            endpoints.app_store_connect("olympus/v1/session"),
            "https://appstoreconnect.apple.com/olympus/v1/session"
        );
        assert_eq!(
            endpoints.app_store_connect_base(),
            "https://appstoreconnect.apple.com"
        );
    }

    #[test]
    fn endpoints_ignore_a_trailing_slash() {
        let endpoints =
            AppleEndpoints::with_base_urls("http://127.0.0.1:1/", "http://127.0.0.1:2//");
        assert_eq!(endpoints.idmsa("x"), "http://127.0.0.1:1/x");
        assert_eq!(endpoints.app_store_connect("y"), "http://127.0.0.1:2/y");
    }
}
