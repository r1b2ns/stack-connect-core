//! Smoke tests over the public (UniFFI-exported) surface — the same entry points
//! Swift will call. No network: provider network paths are covered by the
//! in-crate wiremock tests.

use std::sync::Arc;
use std::sync::Mutex;

use std::collections::HashMap;

use stack_core::{
    available_services, connect, credential_schema, Capability, CredentialStore, Provider,
    ServiceKind, StackError,
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
        ]
    );
    // The Play handles are present ...
    assert!(provider.reviews().is_some());
    assert!(provider.app_details().is_some());
    assert!(provider.store_listings().is_some());
    assert!(provider.tracks().is_some());
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
