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

    #[allow(dead_code)] // exercised once a provider reports a missing capability
    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported {
            message: message.into(),
        }
    }
}
