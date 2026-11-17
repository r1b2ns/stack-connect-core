/// Which external service a connected account talks to. Exported across the FFI;
/// designed to grow as new plugins land (Firebase, AWS, GitHub, …).
///
/// Both bindings encode variants by position, so new variants are only ever
/// appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum ServiceKind {
    /// Apple App Store Connect, authenticated with a team API key (`.p8`).
    AppStoreConnect,
    /// Google Play Console, authenticated with a Google Cloud service account
    /// key.
    GooglePlay,
}

/// A single credential field a service requires. Drives the host's "connect
/// account" form: `label` is shown to the user, `secret` hides the input, and
/// `multiline` signals a textarea (e.g. a PEM-encoded private key).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CredentialField {
    pub key: String,
    pub label: String,
    pub secret: bool,
    pub multiline: bool,
}

impl CredentialField {
    /// Convenience constructor used by each provider's schema declaration.
    pub(crate) fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        secret: bool,
        multiline: bool,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            secret,
            multiline,
        }
    }
}
