//! Smoke tests over the public (UniFFI-exported) surface — the same entry points
//! Swift will call. Network paths are covered by the in-crate wiremock tests.

use std::sync::Arc;

use stack_core::{CredentialStore, PlayProvider, StackError};

#[test]
fn new_rejects_invalid_json() {
    // Avoid unwrap_err(): the Ok type Arc<PlayProvider> is not Debug.
    let result = PlayProvider::new("not json".into());
    assert!(matches!(result, Err(StackError::InvalidCredentials { .. })));
}

struct EmptyStore;

impl CredentialStore for EmptyStore {
    fn secret(&self, _account_id: String, _key: String) -> Option<String> {
        None
    }
    fn set_secret(&self, _account_id: String, _key: String, _value: String) {}
    fn delete(&self, _account_id: String) {}
}

#[test]
fn with_credentials_errors_when_secret_missing() {
    let store: Arc<dyn CredentialStore> = Arc::new(EmptyStore);
    let result = PlayProvider::with_credentials(store, "acct-1".into());
    assert!(matches!(result, Err(StackError::InvalidCredentials { .. })));
}
