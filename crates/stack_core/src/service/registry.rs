use std::sync::Arc;

use crate::error::StackError;
use crate::ports::{CredentialStore, DebugLogger};
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
}
