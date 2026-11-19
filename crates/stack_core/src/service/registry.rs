use std::sync::Arc;

use crate::auth::apple_session::session::{AppleSession, SessionBlob};
use crate::auth::apple_session::AppleEndpoints;
use crate::error::StackError;
use crate::ports::{AccessTokenProvider, CredentialStore, DebugLogger, SessionStore};
use crate::providers::{appstore, googleplay};
use crate::service::kind::{CredentialField, ServiceKind};
use crate::service::provider::ProviderImpl;

/// Every service the core can connect today, in declaration order. Drives the
/// host's service picker.
pub(crate) fn available_services() -> Vec<ServiceKind> {
    vec![ServiceKind::AppStoreConnect, ServiceKind::GooglePlay]
}

/// The credential form a service requires, for the host to render.
pub(crate) fn credential_schema(kind: ServiceKind) -> Vec<CredentialField> {
    match kind {
        ServiceKind::AppStoreConnect => appstore::credential_schema(),
        ServiceKind::GooglePlay => googleplay::credential_schema(),
    }
}

/// Reads the secrets for `(kind, account_id)` from the host store and builds the
/// matching provider. Required keys are read in a fixed order and the first one
/// missing yields a deterministic [`StackError::InvalidCredentials`].
///
/// App Store Connect defers parsing its key to the first request. Google Play
/// also rejects blank fields and parses its key eagerly, so a malformed key is
/// an `InvalidCredentials` here rather than an error on the first request.
pub(crate) fn build(
    kind: ServiceKind,
    account_id: &str,
    store: &Arc<dyn CredentialStore>,
    debug_logger: Option<Arc<dyn DebugLogger>>,
) -> Result<Box<dyn ProviderImpl>, StackError> {
    match kind {
        ServiceKind::AppStoreConnect => {
            let issuer_id = require(store, account_id, appstore::KEY_ISSUER_ID)?;
            let key_id = require(store, account_id, appstore::KEY_KEY_ID)?;
            let private_key_p8 = require(store, account_id, appstore::KEY_PRIVATE_KEY_P8)?;
            Ok(Box::new(appstore::AppStoreProvider::new(
                issuer_id,
                key_id,
                private_key_p8.into_bytes(),
                debug_logger,
            )))
        }
        ServiceKind::GooglePlay => {
            let client_email = require_non_blank(store, account_id, googleplay::KEY_CLIENT_EMAIL)?;
            let private_key_id =
                require_non_blank(store, account_id, googleplay::KEY_PRIVATE_KEY_ID)?;
            let private_key = require_non_blank(store, account_id, googleplay::KEY_PRIVATE_KEY)?;
            Ok(Box::new(googleplay::GooglePlayProvider::try_new(
                &client_email,
                &private_key_id,
                &private_key,
                debug_logger,
            )?))
        }
    }
}

/// Builds the provider for an account that authorizes with an access token the
/// host supplies (the user's own OAuth sign-in) instead of stored secrets.
/// Offline: the host is first asked for a token on the first request.
///
/// # Errors
/// [`StackError::Unsupported`] for a service without a host-token auth path
/// (App Store Connect only takes its API key);
/// [`StackError::InvalidCredentials`] for a blank `account_id`.
pub(crate) fn build_with_access_token(
    kind: ServiceKind,
    account_id: &str,
    token_provider: Arc<dyn AccessTokenProvider>,
    debug_logger: Option<Arc<dyn DebugLogger>>,
) -> Result<Box<dyn ProviderImpl>, StackError> {
    match kind {
        ServiceKind::AppStoreConnect => Err(StackError::unsupported(
            "App Store Connect cannot authorize with an access token; connect it \
             with its API key (`connect`)",
        )),
        ServiceKind::GooglePlay => Ok(Box::new(googleplay::GooglePlayProvider::with_access_token(
            account_id,
            token_provider,
            debug_logger,
        )?)),
    }
}

/// Builds the App Store Connect provider for an account added with "Sign in
/// with Apple ID": restores the session `session_store` holds for
/// `account_id` and acts for the team `team_id` (an App Store Connect
/// provider id). Offline: the team is checked on the first request.
///
/// # Errors
/// [`StackError::InvalidCredentials`] for a blank `account_id` or when the
/// store has no session for it; [`StackError::Decode`] for a blob this core
/// did not write (or a newer format).
pub(crate) fn build_with_apple_session(
    account_id: &str,
    team_id: u64,
    session_store: Arc<dyn SessionStore>,
    debug_logger: Option<Arc<dyn DebugLogger>>,
) -> Result<Box<dyn ProviderImpl>, StackError> {
    build_with_apple_session_on(
        AppleEndpoints::default(),
        account_id,
        team_id,
        session_store,
        debug_logger,
    )
}

/// [`build_with_apple_session`] against `endpoints` (tests: a mock server).
pub(crate) fn build_with_apple_session_on(
    endpoints: AppleEndpoints,
    account_id: &str,
    team_id: u64,
    session_store: Arc<dyn SessionStore>,
    debug_logger: Option<Arc<dyn DebugLogger>>,
) -> Result<Box<dyn ProviderImpl>, StackError> {
    if account_id.trim().is_empty() {
        return Err(StackError::invalid_credentials(
            "empty account id for an Apple ID session",
        ));
    }
    let blob = session_store.load(account_id.to_string()).ok_or_else(|| {
        StackError::invalid_credentials(
            "no saved Apple ID session for this account; sign in with the Apple ID",
        )
    })?;
    let session = AppleSession::restore(&SessionBlob::from_bytes(blob), endpoints)?;
    let auth = appstore::SessionAuth::new(account_id.to_string(), team_id, session, session_store);
    Ok(Box::new(appstore::AppStoreProvider::with_session(
        auth,
        debug_logger,
    )))
}

/// Reads a required secret or fails with a deterministic error naming the key.
fn require(
    store: &Arc<dyn CredentialStore>,
    account_id: &str,
    key: &str,
) -> Result<String, StackError> {
    store
        .secret(account_id.to_string(), key.to_string())
        .ok_or_else(|| StackError::invalid_credentials(format!("missing credential: {key}")))
}

/// [`require`], also rejecting a blank value: a whitespace-only secret is as
/// unusable as a missing one, so it fails the same deterministic way.
fn require_non_blank(
    store: &Arc<dyn CredentialStore>,
    account_id: &str,
    key: &str,
) -> Result<String, StackError> {
    let value = require(store, account_id, key)?;
    if value.trim().is_empty() {
        return Err(StackError::invalid_credentials(format!(
            "empty credential: {key}"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    mod apple_session {
        use super::*;
        use crate::auth::apple_session::test_support::session_on;
        use crate::service::provider::Capability;
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        /// Hands out one blob and records saves.
        struct OneBlob {
            blob: Option<Vec<u8>>,
            saves: Mutex<Vec<String>>,
        }

        impl SessionStore for OneBlob {
            fn load(&self, account_id: String) -> Option<Vec<u8>> {
                (account_id == "acct").then(|| self.blob.clone()).flatten()
            }
            fn save(&self, account_id: String, _blob: Vec<u8>) {
                self.saves.lock().unwrap().push(account_id);
            }
        }

        fn store(blob: Option<Vec<u8>>) -> Arc<OneBlob> {
            Arc::new(OneBlob {
                blob,
                saves: Mutex::new(Vec::new()),
            })
        }

        fn blob_for(endpoints: &AppleEndpoints, team: Option<u64>) -> Vec<u8> {
            let session = session_on(endpoints, Some("widget".into()));
            let session = if let Some(team) = team {
                let (endpoints, jar, http, widget) = session.into_parts();
                AppleSession::from_parts(endpoints, jar, http, widget, Some(team))
            } else {
                session
            };
            session.to_blob().unwrap().as_bytes().to_vec()
        }

        fn build(
            endpoints: AppleEndpoints,
            account: &str,
            store: Arc<OneBlob>,
        ) -> Result<Box<dyn ProviderImpl>, StackError> {
            build_with_apple_session_on(endpoints, account, 7, store, None)
        }

        #[test]
        fn refuses_a_blank_account_a_missing_session_and_a_foreign_blob() {
            let endpoints = AppleEndpoints::default();
            let blob = blob_for(&endpoints, Some(7));
            assert!(matches!(
                build(endpoints.clone(), " ", store(Some(blob))).err(),
                Some(StackError::InvalidCredentials { .. })
            ));
            assert!(matches!(
                build(endpoints.clone(), "acct", store(None)).err(),
                Some(StackError::InvalidCredentials { message }) if message.contains("sign in")
            ));
            assert!(matches!(
                build(endpoints, "acct", store(Some(b"{}".to_vec()))).err(),
                Some(StackError::Decode { .. })
            ));
        }

        #[tokio::test]
        async fn a_session_provider_drops_analytics_and_talks_to_iris() {
            let server = MockServer::start().await;
            let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
            Mock::given(method("GET"))
                .and(path("/iris/v1/apps"))
                .and(header("x-csrf-itc", "[asc-ui]"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "data": [], "links": {}
                })))
                .expect(2)
                .mount(&server)
                .await;
            let provider = build(
                endpoints.clone(),
                "acct",
                store(Some(blob_for(&endpoints, Some(7)))),
            )
            .unwrap();

            assert_eq!(provider.kind(), ServiceKind::AppStoreConnect);
            let capabilities = provider.capabilities();
            assert!(!capabilities.contains(&Capability::Analytics));
            for kept in [
                Capability::Apps,
                Capability::Reviews,
                Capability::BetaGroups,
                Capability::Users,
                Capability::Devices,
                Capability::BundleIds,
                Capability::Certificates,
                Capability::Profiles,
            ] {
                assert!(capabilities.contains(&kept), "{kept:?}");
            }
            provider.validate().await.unwrap();
            assert!(provider.fetch_apps().await.unwrap().is_empty());

            // The API-key provider keeps every capability.
            let key = super::super::build(
                ServiceKind::AppStoreConnect,
                "acct",
                &(MapStore::new(&[("issuerId", "i"), ("keyId", "k"), ("privateKeyP8", "p")])
                    as Arc<dyn CredentialStore>),
                None,
            )
            .unwrap();
            assert_eq!(key.capabilities().len(), capabilities.len() + 1);
        }

        #[tokio::test]
        async fn a_session_provider_lists_devices_on_the_developer_portal() {
            let server = MockServer::start().await;
            let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri())
                .with_developer_base_url(&server.uri());
            Mock::given(method("GET"))
                .and(path("/olympus/v1/session"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "provider": {"providerId": 7, "name": "Seven"},
                    "availableProviders": [{"providerId": 7, "name": "Seven"}]
                })))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/services-account/QH65B2/account/listTeams.action"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "resultCode": 0,
                    "teams": [{"teamId": "SEVEN77777", "name": "Seven", "type": "Individual"}]
                })))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/services-account/v1/devices"))
                .and(header("x-http-method-override", "GET"))
                .and(wiremock::matchers::body_json(serde_json::json!({
                    "urlEncodedQueryParams": "limit=200&sort=name",
                    "teamId": "SEVEN77777"
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "data": [{"type": "devices", "id": "D1", "attributes": {"name": "Phone"}}],
                    "links": {}
                })))
                .expect(1)
                .mount(&server)
                .await;
            let sessions = store(Some(blob_for(&endpoints, Some(7))));
            let provider = build(endpoints, "acct", Arc::clone(&sessions)).unwrap();

            let devices = provider.devices().unwrap().fetch_devices().await.unwrap();
            assert_eq!(devices[0].id, "D1");
            // The resolved portal team is saved with the session.
            assert_eq!(sessions.saves.lock().unwrap().as_slice(), ["acct"]);
        }
    }

    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[test]
    fn available_services_lists_every_kind_in_order() {
        assert_eq!(
            available_services(),
            vec![ServiceKind::AppStoreConnect, ServiceKind::GooglePlay]
        );
    }

    #[test]
    fn appstore_schema_has_three_keys_in_order() {
        let schema = credential_schema(ServiceKind::AppStoreConnect);
        let keys: Vec<&str> = schema.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, vec!["issuerId", "keyId", "privateKeyP8"]);
        // The .p8 field is the only multiline one.
        let p8 = schema.iter().find(|f| f.key == "privateKeyP8").unwrap();
        assert!(p8.secret && p8.multiline);
    }

    /// Records every `(account_id, key)` lookup so tests can assert call order.
    struct RecordingStore {
        calls: Mutex<Vec<(String, String)>>,
    }

    impl RecordingStore {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl CredentialStore for RecordingStore {
        fn secret(&self, account_id: String, key: String) -> Option<String> {
            self.calls.lock().unwrap().push((account_id, key));
            None
        }
        fn set_secret(&self, _account_id: String, _key: String, _value: String) {}
        fn delete(&self, _account_id: String) {}
    }

    #[test]
    fn build_reads_issuer_id_first_and_fails_fast() {
        // Keep a concrete handle to inspect recorded calls; pass a trait-object
        // clone to `build`. Both point at the same `RecordingStore`.
        let recording = Arc::new(RecordingStore::new());
        let store: Arc<dyn CredentialStore> = recording.clone();

        // Avoid `unwrap_err`: the Ok type `Box<dyn ProviderImpl>` is not `Debug`.
        let result = build(ServiceKind::AppStoreConnect, "acct-1", &store, None);
        assert!(matches!(result, Err(StackError::InvalidCredentials { .. })));

        let calls = recording.calls.lock().unwrap();
        assert_eq!(
            calls.first(),
            Some(&("acct-1".to_string(), "issuerId".to_string()))
        );
    }

    const PLAY_PKCS8_PEM: &str = include_str!("../../tests/fixtures/test_rsa_private_pkcs8.pem");
    const PLAY_EMAIL: &str = "play-bot@my-project.iam.gserviceaccount.com";
    const PLAY_KID: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Serves fixed secrets and records every `(account_id, key)` lookup.
    struct MapStore {
        secrets: HashMap<String, String>,
        calls: Mutex<Vec<String>>,
    }

    impl MapStore {
        fn new(secrets: &[(&str, &str)]) -> Arc<Self> {
            Arc::new(Self {
                secrets: secrets
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn valid_play() -> Arc<Self> {
            Self::new(&[
                ("clientEmail", PLAY_EMAIL),
                ("privateKeyId", PLAY_KID),
                ("privateKey", PLAY_PKCS8_PEM),
            ])
        }

        fn keys_read(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CredentialStore for MapStore {
        fn secret(&self, _account_id: String, key: String) -> Option<String> {
            self.calls.lock().unwrap().push(key.clone());
            self.secrets.get(&key).cloned()
        }
        fn set_secret(&self, _account_id: String, _key: String, _value: String) {}
        fn delete(&self, _account_id: String) {}
    }

    /// The `InvalidCredentials` message `build` fails with. The Ok type
    /// `Box<dyn ProviderImpl>` is not `Debug`, so `unwrap_err` is unavailable.
    fn build_err(kind: ServiceKind, store: Arc<MapStore>) -> String {
        let store: Arc<dyn CredentialStore> = store;
        match build(kind, "acct-1", &store, None) {
            Err(StackError::InvalidCredentials { message }) => message,
            Err(other) => panic!("expected InvalidCredentials, got {other:?}"),
            Ok(_) => panic!("expected InvalidCredentials, got a provider"),
        }
    }

    #[test]
    fn google_play_schema_has_three_keys_in_order() {
        let schema = credential_schema(ServiceKind::GooglePlay);
        let keys: Vec<&str> = schema.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, vec!["clientEmail", "privateKeyId", "privateKey"]);
        // The email is the only non-secret field; the PEM the only multiline one.
        let email = schema.iter().find(|f| f.key == "clientEmail").unwrap();
        assert!(!email.secret && !email.multiline);
        let pem = schema.iter().find(|f| f.key == "privateKey").unwrap();
        assert!(pem.secret && pem.multiline);
    }

    #[test]
    fn build_google_play_reads_client_email_first_and_fails_fast() {
        let recording = Arc::new(RecordingStore::new());
        let store: Arc<dyn CredentialStore> = recording.clone();

        let result = build(ServiceKind::GooglePlay, "acct-1", &store, None);
        assert!(matches!(result, Err(StackError::InvalidCredentials { .. })));

        // Fails on the first missing key: exactly one lookup.
        let calls = recording.calls.lock().unwrap();
        assert_eq!(
            *calls,
            vec![("acct-1".to_string(), "clientEmail".to_string())]
        );
    }

    #[test]
    fn build_reads_keys_in_schema_order() {
        let asc = MapStore::new(&[
            ("issuerId", "issuer"),
            ("keyId", "kid"),
            ("privateKeyP8", "p8"),
        ]);
        let play = MapStore::valid_play();

        for (kind, store) in [
            (ServiceKind::AppStoreConnect, asc),
            (ServiceKind::GooglePlay, play),
        ] {
            let dyn_store: Arc<dyn CredentialStore> = store.clone();
            assert!(build(kind, "acct-1", &dyn_store, None).is_ok());
            let schema_keys: Vec<String> =
                credential_schema(kind).into_iter().map(|f| f.key).collect();
            assert_eq!(store.keys_read(), schema_keys, "{kind:?}");
        }
    }

    #[test]
    fn build_google_play_rejects_blank_field() {
        for blank_key in ["clientEmail", "privateKeyId", "privateKey"] {
            let mut secrets = vec![
                ("clientEmail", PLAY_EMAIL),
                ("privateKeyId", PLAY_KID),
                ("privateKey", PLAY_PKCS8_PEM),
            ];
            for entry in &mut secrets {
                if entry.0 == blank_key {
                    entry.1 = " \n\t ";
                }
            }
            let message = build_err(ServiceKind::GooglePlay, MapStore::new(&secrets));
            assert_eq!(message, format!("empty credential: {blank_key}"));
        }
    }

    #[test]
    fn build_google_play_rejects_malformed_key() {
        let store = MapStore::new(&[
            ("clientEmail", PLAY_EMAIL),
            ("privateKeyId", PLAY_KID),
            ("privateKey", "not a key"),
        ]);
        let message = build_err(ServiceKind::GooglePlay, store);
        assert!(message.contains("not a usable RSA private key"));
    }

    #[test]
    fn build_google_play_accepts_valid_credentials() {
        let store: Arc<dyn CredentialStore> = MapStore::valid_play();
        let provider = match build(ServiceKind::GooglePlay, "acct-1", &store, None) {
            Ok(provider) => provider,
            Err(err) => panic!("expected a provider, got {err:?}"),
        };
        assert_eq!(provider.kind(), ServiceKind::GooglePlay);
    }

    // -----------------------------------------------------------------------
    // build_with_access_token (host-supplied token)
    // -----------------------------------------------------------------------

    use crate::auth::host_token::testing::ScriptedTokenProvider;

    fn token_provider() -> Arc<dyn AccessTokenProvider> {
        ScriptedTokenProvider::token("ya29.tok")
    }

    #[test]
    fn build_with_access_token_google_play_matches_the_service_account_provider() {
        let provider = match build_with_access_token(
            ServiceKind::GooglePlay,
            "login-1",
            token_provider(),
            None,
        ) {
            Ok(provider) => provider,
            Err(err) => panic!("expected a provider, got {err:?}"),
        };
        assert_eq!(provider.kind(), ServiceKind::GooglePlay);

        let store: Arc<dyn CredentialStore> = MapStore::valid_play();
        let service_account = match build(ServiceKind::GooglePlay, "acct-1", &store, None) {
            Ok(provider) => provider,
            Err(err) => panic!("expected a provider, got {err:?}"),
        };
        assert_eq!(provider.capabilities(), service_account.capabilities());
    }

    #[test]
    fn build_with_access_token_is_offline() {
        // Building asks the host for nothing: the first token request happens
        // on the first API call.
        let scripted = ScriptedTokenProvider::token("ya29.tok");
        let dyn_provider: Arc<dyn AccessTokenProvider> = scripted.clone();
        assert!(
            build_with_access_token(ServiceKind::GooglePlay, "login-1", dyn_provider, None).is_ok()
        );
        assert!(scripted.calls().is_empty());
    }

    #[test]
    fn build_with_access_token_rejects_a_blank_account_id() {
        for blank in ["", "  \n"] {
            match build_with_access_token(ServiceKind::GooglePlay, blank, token_provider(), None) {
                Err(StackError::InvalidCredentials { message }) => {
                    assert_eq!(message, "account id is empty");
                }
                Err(other) => panic!("expected InvalidCredentials, got {other:?}"),
                Ok(_) => panic!("expected InvalidCredentials, got a provider"),
            }
        }
    }

    #[test]
    fn build_with_access_token_app_store_connect_is_unsupported() {
        match build_with_access_token(
            ServiceKind::AppStoreConnect,
            "acct-1",
            token_provider(),
            None,
        ) {
            Err(StackError::Unsupported { message }) => {
                assert!(message.contains("App Store Connect"), "{message}");
            }
            Err(other) => panic!("expected Unsupported, got {other:?}"),
            Ok(_) => panic!("expected Unsupported, got a provider"),
        }
    }
}
