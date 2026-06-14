//! Smoke tests over the public (UniFFI-exported) surface — the same entry points
//! Swift will call. No network: provider network paths are covered by the
//! in-crate wiremock tests.

use std::sync::Arc;
use std::sync::Mutex;

use stack_core::{
    available_services, connect, credential_schema, CredentialStore, ServiceKind, StackError,
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

    let result = connect(ServiceKind::AppStoreConnect, "acct-1".into(), store);
    assert!(matches!(result, Err(StackError::InvalidCredentials { .. })));

    let calls = recording.calls.lock().unwrap();
    assert_eq!(
        calls.first(),
        Some(&("acct-1".to_string(), "issuerId".to_string()))
    );
}
