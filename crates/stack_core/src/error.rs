use thiserror::Error;

/// Errors that cross the FFI boundary. Every variant carries only FFI-safe fields
/// (strings, integers) so UniFFI can marshal the associated values to Swift.
#[derive(Debug, Error, uniffi::Error)]
pub enum StackError {
    #[error("invalid credentials: {message}")]
    InvalidCredentials { message: String },

    #[error("authentication failed: {message}")]
    Auth { message: String },

    #[error("pending agreements: {message}")]
    PendingAgreements { message: String },

    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },

    #[error("failed to decode response: {message}")]
    Decode { message: String },

    #[error("network error: {message}")]
    Network { message: String },

    #[error("unsupported capability: {message}")]
    Unsupported { message: String },

    /// A review submission cannot be removed through the public App Store Connect
    /// API. This happens for an empty `READY_FOR_REVIEW` draft (zero
    /// `reviewSubmissionItems`): the `reviewSubmissions` resource forbids
    /// `DELETE`, rejects `canceled: true` in this state, and has no items to
    /// remove — so there is no API path to clear it. The host must surface this
    /// honestly (e.g. point the user at the App Store Connect website).
    #[error("submission not removable: {message}")]
    SubmissionNotRemovable { message: String },
}

impl StackError {
    pub(crate) fn invalid_credentials(message: impl Into<String>) -> Self {
        Self::InvalidCredentials {
            message: message.into(),
        }
    }

    pub(crate) fn auth(message: impl Into<String>) -> Self {
        Self::Auth {
            message: message.into(),
        }
    }

    pub(crate) fn pending_agreements(message: impl Into<String>) -> Self {
        Self::PendingAgreements {
            message: message.into(),
        }
    }

    pub(crate) fn decode(message: impl Into<String>) -> Self {
        Self::Decode {
            message: message.into(),
        }
    }

    pub(crate) fn network(message: impl Into<String>) -> Self {
        Self::Network {
            message: message.into(),
        }
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported {
            message: message.into(),
        }
    }

    pub(crate) fn submission_not_removable(message: impl Into<String>) -> Self {
        Self::SubmissionNotRemovable {
            message: message.into(),
        }
    }
}
