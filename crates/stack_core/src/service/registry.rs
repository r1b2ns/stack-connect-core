use std::sync::Arc;

use crate::error::StackError;
use crate::ports::{CredentialStore, DebugLogger};
use crate::providers::appstore;
use crate::service::kind::{CredentialField, ServiceKind};
use crate::service::provider::ProviderImpl;

/// Every service the core can connect today. Drives the host's service picker.
pub(crate) fn available_services() -> Vec<ServiceKind> {
    vec![ServiceKind::AppStoreConnect]
}

/// The credential form a service requires, for the host to render.
pub(crate) fn credential_schema(kind: ServiceKind) -> Vec<CredentialField> {
    match kind {
        ServiceKind::AppStoreConnect => appstore::credential_schema(),
    }
}

/// Reads the secrets for `(kind, account_id)` from the host store and builds the
/// matching provider. Required keys are read in a fixed order and the first one
/// missing yields a deterministic [`StackError::InvalidCredentials`].
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn available_services_contains_appstore() {
        assert!(available_services().contains(&ServiceKind::AppStoreConnect));
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
}
