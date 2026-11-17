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
/// `connect`), every provider logs each request as a runnable cURL (headers +
/// body, pretty-printed when JSON) and the response (status line +
/// pretty-printed JSON): App Store Connect API calls, and for Google Play both
/// the OAuth token exchange and every API call. Off by default — the host only
/// passes a logger when its debug launch flag is set. Implemented natively (the
/// iOS app prints to the Xcode console) and injected across the FFI as a foreign
/// trait.
///
/// Nothing is redacted, so the log is sensitive: `Authorization` bearer tokens
/// appear verbatim, and so does Google's signed JWT assertion in the token
/// request body. Each grants API access until it expires (an assertion can be
/// exchanged again for a fresh one-hour token until its own `exp`). Private keys
/// never appear in any request.
#[uniffi::export(with_foreign)]
pub trait DebugLogger: Send + Sync {
    /// Emits one already-formatted, possibly multi-line debug message.
    fn log(&self, message: String);
}

/// Process-wide diagnostic log sink, installed with
/// [`crate::facade::set_core_logger`]. Unlike [`DebugLogger`], it is meant to
/// stay on in release builds: every message is **redacted** before it gets
/// here (no access token, `Authorization` header, private key, JSON key or
/// JWT; user e-mails masked as `d***@agency.example`), and lines are leveled so
/// the host can filter and route them (iOS: one `os.Logger` per `target`).
///
/// Called synchronously on whichever thread the core is running; implement it
/// as a cheap, non-blocking hand-off (e.g. `Logger.log`).
#[uniffi::export(with_foreign)]
pub trait CoreLogger: Send + Sync {
    /// Emits one single-line message. `target` names the area (`http`,
    /// `auth`, `googleplay.users`, …; see the README's *Logging* section).
    fn log(&self, level: crate::logging::LogLevel, target: String, message: String);
}
