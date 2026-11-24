//! Smoke tests over the public (UniFFI-exported) surface — the same entry points
//! Swift will call. No network: provider network paths are covered by the
//! in-crate wiremock tests.

use std::sync::Arc;
use std::sync::Mutex;

use std::collections::HashMap;

use stack_core::{
    available_services, connect, connect_with_access_token, connect_with_apple_session,
    credential_schema, set_core_logger, AccessTokenProvider, AppleIdSignIn, Capability, CoreLogger,
    CredentialStore, LogLevel, Provider, ServiceKind, SessionStore, StackError,
};

#[test]
fn available_services_is_not_empty() {
    let services = available_services();
    assert!(services.contains(&ServiceKind::AppStoreConnect));
}

#[test]
fn appstore_schema_exposes_three_fields() {
    let schema = credential_schema(ServiceKind::AppStoreConnect);
    let keys: Vec<&str> = schema.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, vec!["issuerId", "keyId", "privateKeyP8"]);
}

/// A `CredentialStore` that always returns `None` and records its lookups, so the
/// test can assert the facade consulted it (and in which order).
struct EmptyStore {
    calls: Mutex<Vec<(String, String)>>,
}

impl EmptyStore {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl CredentialStore for EmptyStore {
    fn secret(&self, account_id: String, key: String) -> Option<String> {
        self.calls.lock().unwrap().push((account_id, key));
        None
    }
    fn set_secret(&self, _account_id: String, _key: String, _value: String) {}
    fn delete(&self, _account_id: String) {}
}

#[test]
fn connect_errors_and_queries_store_in_order() {
    let recording = Arc::new(EmptyStore::new());
    let store: Arc<dyn CredentialStore> = recording.clone();

    let result = connect(ServiceKind::AppStoreConnect, "acct-1".into(), store, None);
    assert!(matches!(result, Err(StackError::InvalidCredentials { .. })));

    let calls = recording.calls.lock().unwrap();
    assert_eq!(
        calls.first(),
        Some(&("acct-1".to_string(), "issuerId".to_string()))
    );
}

// ---------------------------------------------------------------------------
// Google Play
// ---------------------------------------------------------------------------

const PLAY_PKCS8_PEM: &str = include_str!("fixtures/test_rsa_private_pkcs8.pem");
const PLAY_EMAIL: &str = "play-bot@my-project.iam.gserviceaccount.com";
const PLAY_KID: &str = "0123456789abcdef0123456789abcdef01234567";

/// A `CredentialStore` serving fixed secrets for any account.
struct MapStore {
    secrets: HashMap<String, String>,
}

impl MapStore {
    fn new(secrets: &[(&str, &str)]) -> Arc<Self> {
        Arc::new(Self {
            secrets: secrets
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        })
    }
}

impl CredentialStore for MapStore {
    fn secret(&self, _account_id: String, key: String) -> Option<String> {
        self.secrets.get(&key).cloned()
    }
    fn set_secret(&self, _account_id: String, _key: String, _value: String) {}
    fn delete(&self, _account_id: String) {}
}

/// Connects a Google Play account. `Provider` is not `Debug`, so the result is
/// matched rather than unwrapped.
fn connect_play(private_key: &str) -> Result<Arc<Provider>, StackError> {
    let store: Arc<dyn CredentialStore> = MapStore::new(&[
        ("clientEmail", PLAY_EMAIL),
        ("privateKeyId", PLAY_KID),
        ("privateKey", private_key),
    ]);
    connect(ServiceKind::GooglePlay, "acct-1".into(), store, None)
}

#[test]
fn available_services_contains_google_play() {
    assert!(available_services().contains(&ServiceKind::GooglePlay));
}

#[test]
fn google_play_schema_exposes_three_fields() {
    let schema = credential_schema(ServiceKind::GooglePlay);
    let keys: Vec<&str> = schema.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, vec!["clientEmail", "privateKeyId", "privateKey"]);
}

#[test]
fn connect_google_play_queries_client_email_first() {
    let recording = Arc::new(EmptyStore::new());
    let store: Arc<dyn CredentialStore> = recording.clone();

    let result = connect(ServiceKind::GooglePlay, "acct-1".into(), store, None);
    assert!(matches!(result, Err(StackError::InvalidCredentials { .. })));

    // Fails fast on the first missing key: exactly one lookup.
    let calls = recording.calls.lock().unwrap();
    assert_eq!(
        *calls,
        vec![("acct-1".to_string(), "clientEmail".to_string())]
    );
}

#[test]
fn connect_google_play_rejects_malformed_key() {
    // Google Play parses its key at connect, unlike App Store Connect.
    let result = connect_play("-----BEGIN PRIVATE KEY-----\nnot a key\n-----END PRIVATE KEY-----");
    assert!(matches!(result, Err(StackError::InvalidCredentials { .. })));
}

#[test]
fn connect_google_play_reports_kind_and_capabilities() {
    let provider = match connect_play(PLAY_PKCS8_PEM) {
        Ok(provider) => provider,
        Err(err) => panic!("expected a Google Play provider, got {err:?}"),
    };

    assert_eq!(provider.kind(), ServiceKind::GooglePlay);
    assert_eq!(
        provider.capabilities(),
        vec![
            Capability::Apps,
            Capability::Reviews,
            Capability::AppDetails,
            Capability::StoreListings,
            Capability::Tracks,
            Capability::AppIcons,
            Capability::ReleaseManagement,
            Capability::PlayUsers,
        ]
    );
    // The Play handles are present ...
    assert!(provider.reviews().is_some());
    assert!(provider.app_details().is_some());
    assert!(provider.store_listings().is_some());
    assert!(provider.tracks().is_some());
    assert!(provider.app_icons().is_some());
    assert!(provider.play_users().is_some());
    // ... and every App Store Connect-only one is absent.
    assert!(provider.app_store_versions().is_none());
    assert!(provider.builds().is_none());
    assert!(provider.beta_groups().is_none());
    assert!(provider.beta_build_localizations().is_none());
    assert!(provider.beta_app_localizations().is_none());
    assert!(provider.beta_app_review_detail().is_none());
    assert!(provider.app_metadata().is_none());
    assert!(provider.accessibility_declarations().is_none());
    assert!(provider.users().is_none());
    assert!(provider.devices().is_none());
    assert!(provider.bundle_ids().is_none());
    assert!(provider.certificates().is_none());
    assert!(provider.profiles().is_none());
    assert!(provider.analytics().is_none());
}

#[tokio::test]
async fn google_play_app_icons_answer_an_invalid_package_offline() {
    // A name no Play app can have has no store page: `None`, decided locally,
    // so this crosses the public `AppIcons` handle without any network.
    let provider = match connect_play(PLAY_PKCS8_PEM) {
        Ok(provider) => provider,
        Err(err) => panic!("expected a Google Play provider, got {err:?}"),
    };
    let icons = match provider.app_icons() {
        Some(icons) => icons,
        None => panic!("Google Play should expose AppIcons"),
    };
    let icon = icons.fetch_icon_url("not a package".to_string()).await;
    assert!(matches!(icon, Ok(None)), "{icon:?}");
}

/// A host log sink that keeps every line.
#[derive(Default)]
struct RecordingLogger {
    lines: Mutex<Vec<(LogLevel, String, String)>>,
}

impl CoreLogger for RecordingLogger {
    fn log(&self, level: LogLevel, target: String, message: String) {
        self.lines.lock().unwrap().push((level, target, message));
    }
}

#[tokio::test]
async fn core_logger_receives_a_local_rejection_through_the_public_api() {
    // The only test in this binary that installs the process-wide sink.
    let logger = Arc::new(RecordingLogger::default());
    set_core_logger(Some(logger.clone()), LogLevel::Debug);

    let provider = match connect_play(PLAY_PKCS8_PEM) {
        Ok(provider) => provider,
        Err(err) => panic!("expected a Google Play provider, got {err:?}"),
    };
    let users = match provider.play_users() {
        Some(users) => users,
        None => panic!("Google Play should expose PlayUsers"),
    };
    // Not a number: rejected locally, so no network is needed.
    let err = users.fetch_users("abc".to_string()).await.unwrap_err();
    set_core_logger(None, LogLevel::Info);

    let expected = "local validation failed: developer ID \"abc\" is not numeric \
                    (3 chars: letters); it is the number after /developers/ in a Play \
                    Console URL; request not sent to Google";
    match err {
        StackError::Http { status, message } => {
            assert_eq!(status, 404);
            assert_eq!(message, expected);
        }
        other => panic!("expected the local Http 404, got {other:?}"),
    }
    let lines = logger.lines.lock().unwrap().clone();
    assert!(
        lines.contains(&(
            LogLevel::Warn,
            "googleplay.users".to_string(),
            expected.to_string()
        )),
        "{lines:?}"
    );
    // Nothing reached the network, so there is no HTTP line, and the key never
    // shows up anywhere.
    assert!(
        lines.iter().all(|(_, target, _)| target != "http"),
        "{lines:?}"
    );
    assert!(lines
        .iter()
        .all(|(_, _, message)| !message.contains("PRIVATE KEY")));
}

// ---------------------------------------------------------------------------
// Google Play with a host-supplied access token ("Sign in with Google")
// ---------------------------------------------------------------------------

/// A host token source that fails with a fixed error and records the account
/// ids it was asked for, so these tests stay offline: the provider fails
/// before any request is sent.
struct FailingTokens {
    error: fn() -> StackError,
    calls: Mutex<Vec<String>>,
}

impl FailingTokens {
    fn new(error: fn() -> StackError) -> Arc<Self> {
        Arc::new(Self {
            error,
            calls: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl AccessTokenProvider for FailingTokens {
    async fn access_token(&self, account_id: String) -> Result<String, StackError> {
        self.calls.lock().unwrap().push(account_id);
        Err((self.error)())
    }
}

fn connect_play_with_tokens(tokens: Arc<FailingTokens>) -> Arc<Provider> {
    match connect_with_access_token(ServiceKind::GooglePlay, "login-1".into(), tokens, None) {
        Ok(provider) => provider,
        Err(err) => panic!("expected a Google Play provider, got {err:?}"),
    }
}

#[test]
fn connect_with_access_token_google_play_has_the_service_account_capabilities() {
    let tokens = FailingTokens::new(|| StackError::Auth {
        message: "unused".into(),
    });
    let provider = connect_play_with_tokens(tokens.clone());
    let service_account = match connect_play(PLAY_PKCS8_PEM) {
        Ok(provider) => provider,
        Err(err) => panic!("expected a Google Play provider, got {err:?}"),
    };

    assert_eq!(provider.kind(), ServiceKind::GooglePlay);
    assert_eq!(provider.capabilities(), service_account.capabilities());
    assert!(provider.play_users().is_some());
    // Connecting is offline: the host is not asked for a token yet.
    assert!(tokens.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn connect_with_access_token_provider_failure_is_auth() {
    let tokens = FailingTokens::new(|| StackError::Auth {
        message: "invalid_grant".into(),
    });
    let provider = connect_play_with_tokens(tokens.clone());

    match provider.validate().await {
        Err(StackError::Auth { message }) => assert_eq!(message, "invalid_grant"),
        Err(other) => panic!("expected Auth, got {other:?}"),
        Ok(()) => panic!("expected Auth, got Ok"),
    }
    assert_eq!(*tokens.calls.lock().unwrap(), vec!["login-1".to_string()]);
}

#[tokio::test]
async fn connect_with_access_token_offline_host_is_network() {
    let tokens = FailingTokens::new(|| StackError::Network {
        message: "offline".into(),
    });
    let provider = connect_play_with_tokens(tokens);
    assert!(matches!(
        provider.validate().await,
        Err(StackError::Network { .. })
    ));
}

#[test]
fn connect_with_access_token_rejects_app_store_connect_and_blank_ids() {
    let tokens = || -> Arc<dyn AccessTokenProvider> {
        FailingTokens::new(|| StackError::Auth {
            message: "unused".into(),
        })
    };
    assert!(matches!(
        connect_with_access_token(
            ServiceKind::AppStoreConnect,
            "acct-1".into(),
            tokens(),
            None
        ),
        Err(StackError::Unsupported { .. })
    ));
    assert!(matches!(
        connect_with_access_token(ServiceKind::GooglePlay, " ".into(), tokens(), None),
        Err(StackError::InvalidCredentials { .. })
    ));
}

/// A `SessionStore` holding at most one blob.
struct OneSession(Option<Vec<u8>>);

impl SessionStore for OneSession {
    fn load(&self, _account_id: String) -> Option<Vec<u8>> {
        self.0.clone()
    }
    fn save(&self, _account_id: String, _blob: Vec<u8>) {}
}

#[test]
fn connect_with_apple_session_needs_a_saved_session_the_core_wrote() {
    let missing = connect_with_apple_session("acct".into(), 1, Arc::new(OneSession(None)), None);
    assert!(matches!(
        missing,
        Err(StackError::InvalidCredentials { .. })
    ));
    let foreign = connect_with_apple_session(
        "acct".into(),
        1,
        Arc::new(OneSession(Some(b"{}".to_vec()))),
        None,
    );
    assert!(matches!(foreign, Err(StackError::Decode { .. })));
}

#[tokio::test]
async fn apple_id_sign_in_is_constructible_offline_and_not_finished() {
    let sign_in = AppleIdSignIn::new().unwrap();
    assert!(sign_in.teams().await.is_empty());
    assert!(matches!(
        sign_in.finish().await,
        Err(StackError::Auth { .. })
    ));
}
