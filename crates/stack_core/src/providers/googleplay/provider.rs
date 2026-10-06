use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::client::GooglePlayClient;
use super::PLAY_SCOPES;
use crate::auth::oauth_jwt::{ServiceAccountAuthenticator, ServiceAccountKey};
use crate::domain::AppInfo;
use crate::error::StackError;
use crate::ports::DebugLogger;
use crate::service::kind::ServiceKind;
use crate::service::provider::{Capability, ProviderImpl};

/// Bound on establishing a connection to Google.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on a whole request. The authenticator holds its token lock across the
/// token exchange, so an unbounded request would stall every caller.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Google Play implementation of the internal [`ProviderImpl`] contract. First
/// slice: connect and list apps ([`Capability::Apps`]); every capability
/// accessor keeps the trait's `None` default.
///
/// The [`GooglePlayClient`] is held behind an `Arc` so future capability
/// sub-objects share the same client — and therefore the same authenticator and
/// token cache — as `validate`/`fetch_apps`.
pub(crate) struct GooglePlayProvider {
    client: Arc<GooglePlayClient>,
}

impl GooglePlayProvider {
    /// Builds the provider from the three service-account fields.
    ///
    /// Unlike App Store Connect, the key is parsed eagerly — including one test
    /// signature (see [`ServiceAccountKey::parse`]) — so a malformed key fails
    /// at connect. One `reqwest::Client` with connect and request timeouts is
    /// shared by the authenticator and the API client. When `debug_logger` is
    /// `Some`, both the token exchange and every API call are traced through it
    /// (see [`crate::ports::DebugLogger`]).
    ///
    /// # Errors
    /// [`StackError::InvalidCredentials`] if a field is blank or the key is not
    /// a usable RSA private key; [`StackError::Network`] if the HTTP client
    /// cannot be built.
    pub(crate) fn try_new(
        client_email: &str,
        private_key_id: &str,
        private_key: &str,
        debug_logger: Option<Arc<dyn DebugLogger>>,
    ) -> Result<Self, StackError> {
        let key = ServiceAccountKey::parse(client_email, private_key_id, private_key)?;
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| StackError::network(format!("HTTP client setup failed: {e}")))?;

        // `reqwest::Client` is an `Arc` internally: the clone shares one pool.
        let auth = ServiceAccountAuthenticator::new(key, PLAY_SCOPES, http.clone());
        Ok(Self {
            client: Arc::new(GooglePlayClient::new(auth, http).with_debug_logger(debug_logger)),
        })
    }
}

#[async_trait]
impl ProviderImpl for GooglePlayProvider {
    fn kind(&self) -> ServiceKind {
        ServiceKind::GooglePlay
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Apps]
    }

    async fn validate(&self) -> Result<(), StackError> {
        self.client.validate().await
    }

    async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        self.client.fetch_apps().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::provider::Provider;

    const PKCS8_PEM: &str = include_str!("../../../tests/fixtures/test_rsa_private_pkcs8.pem");
    const EMAIL: &str = "play-bot@my-project.iam.gserviceaccount.com";
    const KID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn provider() -> GooglePlayProvider {
        // `unwrap` (not `unwrap_err`) is fine: the error type is `Debug`.
        GooglePlayProvider::try_new(EMAIL, KID, PKCS8_PEM, None).unwrap()
    }

    #[test]
    fn reports_kind_and_capabilities() {
        let p = provider();
        assert_eq!(p.kind(), ServiceKind::GooglePlay);
        assert_eq!(p.capabilities(), vec![Capability::Apps]);
    }

    #[test]
    fn all_capability_accessors_are_none() {
        // The first provider that lacks capabilities: every accessor keeps the
        // trait's `None` default.
        let p = provider();
        assert!(p.reviews().is_none());
        assert!(p.app_store_versions().is_none());
        assert!(p.builds().is_none());
        assert!(p.beta_groups().is_none());
        assert!(p.beta_build_localizations().is_none());
        assert!(p.beta_app_localizations().is_none());
        assert!(p.beta_app_review_detail().is_none());
        assert!(p.app_metadata().is_none());
        assert!(p.accessibility_declarations().is_none());
        assert!(p.users().is_none());
        assert!(p.devices().is_none());
        assert!(p.bundle_ids().is_none());
        assert!(p.certificates().is_none());
        assert!(p.profiles().is_none());
        assert!(p.analytics().is_none());
    }

    #[test]
    fn provider_wrapper_exposes_no_handles() {
        // The same through the exported `Provider` the hosts hold.
        let p = Provider::new(Box::new(provider()));
        assert_eq!(p.kind(), ServiceKind::GooglePlay);
        assert_eq!(p.capabilities(), vec![Capability::Apps]);
        assert!(p.reviews().is_none());
        assert!(p.app_store_versions().is_none());
        assert!(p.builds().is_none());
        assert!(p.beta_groups().is_none());
        assert!(p.beta_build_localizations().is_none());
        assert!(p.beta_app_localizations().is_none());
        assert!(p.beta_app_review_detail().is_none());
        assert!(p.app_metadata().is_none());
        assert!(p.accessibility_declarations().is_none());
        assert!(p.users().is_none());
        assert!(p.devices().is_none());
        assert!(p.bundle_ids().is_none());
        assert!(p.certificates().is_none());
        assert!(p.profiles().is_none());
        assert!(p.analytics().is_none());
    }

    #[test]
    fn try_new_rejects_malformed_key() {
        // `GooglePlayProvider` is not `Debug`, so no `unwrap_err`.
        let result = GooglePlayProvider::try_new(EMAIL, KID, "not a key", None);
        assert!(matches!(result, Err(StackError::InvalidCredentials { .. })));
    }

    #[test]
    fn try_new_rejects_invalid_identity_fields() {
        assert!(matches!(
            GooglePlayProvider::try_new("not-an-email", KID, PKCS8_PEM, None),
            Err(StackError::InvalidCredentials { .. })
        ));
        assert!(matches!(
            GooglePlayProvider::try_new(EMAIL, "  ", PKCS8_PEM, None),
            Err(StackError::InvalidCredentials { .. })
        ));
    }

    #[test]
    fn http_timeouts_are_bounded() {
        // Literals, so a changed bound fails here.
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(30));
    }
}
