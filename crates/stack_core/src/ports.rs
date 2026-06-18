/// Secure-credential storage implemented natively (Keychain on iOS) and injected
/// across the FFI boundary as a foreign trait. Each provider declares the keys it
/// reads via its `credential_schema` (see `service::registry`).
#[uniffi::export(with_foreign)]
pub trait CredentialStore: Send + Sync {
    /// Returns the secret stored for `(account_id, key)`, if present.
    fn secret(&self, account_id: String, key: String) -> Option<String>;
    /// Stores or replaces a secret.
    fn set_secret(&self, account_id: String, key: String, value: String);
    /// Removes every secret associated with `account_id`.
    fn delete(&self, account_id: String);
}

/// Durable blob storage implemented natively (SwiftData on iOS, mirroring the
/// host's `PersistentStorable`) and injected across the FFI boundary as a foreign
/// trait. The core stays stateless: [`crate::service::sync::SyncService`] pulls
/// entities from a [`crate::service::provider::Provider`] and persists each as an
/// opaque JSON blob keyed by `(type_name, id)` — the host owns where and how those
/// blobs live. `type_name` is a stable, core-defined string (e.g.
/// [`crate::service::sync::BLOB_TYPE_APP`]) that the host maps to its own entity.
#[uniffi::export(with_foreign)]
pub trait BlobStore: Send + Sync {
    /// Inserts or replaces the JSON blob for `(type_name, id)`.
    fn save(&self, type_name: String, id: String, json: String);
    /// Returns the JSON blob for `(type_name, id)`, if present.
    fn fetch(&self, type_name: String, id: String) -> Option<String>;
    /// Returns every stored JSON blob of `type_name`.
    fn fetch_all(&self, type_name: String) -> Vec<String>;
    /// Removes the blob for `(type_name, id)`.
    fn delete(&self, type_name: String, id: String);
}

/// Optional debug sink for HTTP tracing. When the host injects one (via
/// `connect`), the App Store Connect client logs every request as a runnable
/// cURL (headers + pretty-printed JSON body) and the response (status line +
/// pretty-printed JSON). Off by default — the host only passes a logger when its
/// debug launch flag is set. Implemented natively (the iOS app prints to the
/// Xcode console) and injected across the FFI as a foreign trait.
#[uniffi::export(with_foreign)]
pub trait DebugLogger: Send + Sync {
    /// Emits one already-formatted, possibly multi-line debug message.
    fn log(&self, message: String);
}
