/// Secure-credential storage implemented natively (Keychain on iOS) and injected
/// across the FFI boundary as a foreign trait. Phase 0 uses it to read the Google
/// service-account JSON; later phases add the per-provider secrets.
#[uniffi::export(with_foreign)]
pub trait CredentialStore: Send + Sync {
    /// Returns the secret stored for `(account_id, key)`, if present.
    fn secret(&self, account_id: String, key: String) -> Option<String>;
    /// Stores or replaces a secret.
    fn set_secret(&self, account_id: String, key: String, value: String);
    /// Removes every secret associated with `account_id`.
    fn delete(&self, account_id: String);
}

/// Conventional key under which the Google service-account JSON is stored.
pub(crate) const SERVICE_ACCOUNT_KEY: &str = "serviceAccountJson";
