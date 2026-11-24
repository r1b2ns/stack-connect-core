//! Failures of the Apple ID sign-in and session, and how they cross into
//! [`StackError`].
//!
//! No message carries the Apple ID, the password, a code, a cookie or a
//! session header: messages name what failed and, for Apple's own refusals,
//! carry Apple's error codes and texts (scrubbed by
//! [`crate::logging::redact::scrub`]).

use thiserror::Error;

use crate::error::StackError;

/// What went wrong while signing in with an Apple ID or using its session.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AppleSignInError {
    /// Apple rejected the email and password combination (HTTP 401 on
    /// `signin/init` or `signin/complete`, or an `invalid="true"` body).
    #[error("the Apple ID or password is incorrect")]
    WrongCredentials,

    /// Apple rejected the two-factor code (error -21669, "Incorrect
    /// verification code"). The sign-in still waits for a code.
    #[error("the verification code is incorrect")]
    WrongCode,

    /// HTTP 412 with an `authType`: the Apple ID must first accept Apple's
    /// privacy statement or upgrade to two-factor authentication, on
    /// appleid.apple.com or appstoreconnect.apple.com in a browser.
    #[error(
        "this Apple ID needs attention before it can sign in here: sign in once at \
         https://appleid.apple.com to accept Apple's terms or upgrade to two-factor \
         authentication, then try again"
    )]
    AcknowledgementRequired,

    /// The Apple ID verifies with a security key (FIDO), which only a browser
    /// can do.
    #[error(
        "this Apple ID verifies with a security key, which is not supported here; \
         use an App Store Connect API key instead"
    )]
    SecurityKeyRequired,

    /// The Apple ID uses the legacy two-step verification (a 4-digit code sent
    /// to a chosen device), which this client does not implement.
    #[error(
        "this Apple ID uses legacy two-step verification, which is not supported here; \
         use an App Store Connect API key instead"
    )]
    TwoStepUnsupported,

    /// The sign-in worked but the Apple ID has no App Store Connect access
    /// (`itctx` refusal, or an olympus session without a team).
    #[error("this Apple ID is not enabled for App Store Connect")]
    NotEnabledForAppStoreConnect,

    /// Apple could not start or continue the two-factor step, e.g. it cannot
    /// send a code to the phone (-28248) or the account has no trusted device.
    #[error("Apple could not complete two-factor authentication: {message}")]
    TwoFactorUnavailable { message: String },

    /// Apple refused the request for a stated reason (a locked Apple ID, too
    /// many codes, a refused team switch).
    #[error("Apple refused the request (HTTP {status}): {message}")]
    Refused { status: u16, message: String },

    /// HTTP 429.
    #[error("Apple is rate limiting sign-ins; wait a few minutes and try again")]
    RateLimited,

    /// A 5xx from Apple.
    #[error("Apple's sign-in service is unavailable (HTTP {status}); try again later")]
    AppleUnavailable { status: u16 },

    /// The two-factor exchange timed out on Apple's side (HTTP 401 while a
    /// code was pending): the sign-in must start again.
    #[error("the sign-in expired before it was completed; start again")]
    SignInExpired,

    /// App Store Connect refused the cookies (HTTP 401/403 on the olympus
    /// session): signed out, expired, or a stale widget key.
    #[error("App Store Connect refused the session (HTTP {status}); sign in again")]
    SessionRejected { status: u16 },

    /// No widget key from any source (sign-out redirect, known key, olympus
    /// config).
    #[error("could not read the App Store Connect widget key from Apple")]
    WidgetKeyUnavailable,

    /// The SRP exchange could not be computed from what Apple sent (an
    /// unsupported protocol, a bad salt or `B`).
    #[error("the password exchange with Apple failed: {message}")]
    Srp { message: String },

    /// A call that does not fit the current step (a code before `start`, …).
    #[error("{message}")]
    WrongStep { message: String },

    /// A caller-supplied value was rejected before anything was sent.
    #[error("invalid input: {message}; nothing was sent to Apple")]
    InvalidInput { message: String },

    /// A session blob that is not one this core wrote, or a newer format.
    #[error("the saved Apple ID session cannot be read: {message}")]
    InvalidSessionBlob { message: String },

    /// A response this client does not understand.
    #[error("unexpected response from Apple (HTTP {status}): {message}")]
    Unexpected { status: u16, message: String },

    /// Transport failure (DNS, TLS, timeout, reading the body).
    #[error("could not reach Apple: {message}")]
    Network { message: String },
}

impl AppleSignInError {
    pub(crate) fn unexpected(status: u16, message: impl Into<String>) -> Self {
        Self::Unexpected {
            status,
            message: message.into(),
        }
    }

    pub(crate) fn wrong_step(message: impl Into<String>) -> Self {
        Self::WrongStep {
            message: message.into(),
        }
    }

    pub(crate) fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }

    pub(crate) fn srp(message: impl Into<String>) -> Self {
        Self::Srp {
            message: message.into(),
        }
    }

    pub(crate) fn invalid_blob(message: impl Into<String>) -> Self {
        Self::InvalidSessionBlob {
            message: message.into(),
        }
    }
}

/// Maps onto [`StackError`]: a wrong password or two-factor code →
/// `InvalidCredentials`; a
/// session App Store Connect refuses → `SessionExpired` (the host asks the
/// user to sign in again); an HTTP-status failure → `Http`; transport →
/// `Network`; an unreadable blob → `Decode`; everything else is an
/// authentication failure → `Auth`. The message is the [`Display`] text.
///
/// [`Display`]: std::fmt::Display
impl From<AppleSignInError> for StackError {
    fn from(err: AppleSignInError) -> Self {
        let message = err.to_string();
        match err {
            // A wrong password (from `start`) and a wrong two-factor code (from
            // `submit_code`, after which the sign-in still waits for a code)
            // are both the user's input being refused: the call tells them
            // apart.
            AppleSignInError::WrongCredentials | AppleSignInError::WrongCode => {
                StackError::invalid_credentials(message)
            }
            AppleSignInError::RateLimited => StackError::Http {
                status: 429,
                message,
            },
            AppleSignInError::AppleUnavailable { status }
            | AppleSignInError::Unexpected { status, .. } => StackError::Http { status, message },
            AppleSignInError::Network { .. } => StackError::network(message),
            AppleSignInError::InvalidSessionBlob { .. } => StackError::decode(message),
            AppleSignInError::SessionRejected { .. } => StackError::session_expired(message),
            AppleSignInError::AcknowledgementRequired
            | AppleSignInError::SecurityKeyRequired
            | AppleSignInError::TwoStepUnsupported
            | AppleSignInError::NotEnabledForAppStoreConnect
            | AppleSignInError::TwoFactorUnavailable { .. }
            | AppleSignInError::Refused { .. }
            | AppleSignInError::SignInExpired
            | AppleSignInError::WidgetKeyUnavailable
            | AppleSignInError::Srp { .. }
            | AppleSignInError::WrongStep { .. }
            | AppleSignInError::InvalidInput { .. } => StackError::auth(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_onto_existing_stack_error_variants() {
        assert!(matches!(
            StackError::from(AppleSignInError::WrongCredentials),
            StackError::InvalidCredentials { message } if message == "the Apple ID or password is incorrect"
        ));
        assert!(matches!(
            StackError::from(AppleSignInError::AppleUnavailable { status: 503 }),
            StackError::Http { status: 503, .. }
        ));
        assert!(matches!(
            StackError::from(AppleSignInError::RateLimited),
            StackError::Http { status: 429, .. }
        ));
        assert!(matches!(
            StackError::from(AppleSignInError::unexpected(418, "teapot")),
            StackError::Http { status: 418, message } if message.contains("teapot")
        ));
        assert!(matches!(
            StackError::from(AppleSignInError::Network {
                message: "dns".into()
            }),
            StackError::Network { .. }
        ));
        assert!(matches!(
            StackError::from(AppleSignInError::invalid_blob("bad")),
            StackError::Decode { .. }
        ));
        assert!(matches!(
            StackError::from(AppleSignInError::SessionRejected { status: 401 }),
            StackError::SessionExpired { message } if message.contains("HTTP 401")
        ));
        assert!(matches!(
            StackError::from(AppleSignInError::WrongCode),
            StackError::InvalidCredentials { message } if message == "the verification code is incorrect"
        ));
        for err in [
            AppleSignInError::AcknowledgementRequired,
            AppleSignInError::SecurityKeyRequired,
            AppleSignInError::TwoStepUnsupported,
            AppleSignInError::NotEnabledForAppStoreConnect,
            AppleSignInError::SignInExpired,
            AppleSignInError::WidgetKeyUnavailable,
            AppleSignInError::srp("x"),
            AppleSignInError::wrong_step("x"),
            AppleSignInError::invalid_input("x"),
            AppleSignInError::TwoFactorUnavailable {
                message: "x".into(),
            },
            AppleSignInError::Refused {
                status: 403,
                message: "locked".into(),
            },
        ] {
            let text = err.to_string();
            assert!(
                matches!(StackError::from(err), StackError::Auth { message } if message == text),
                "{text}"
            );
        }
    }

    #[test]
    fn messages_name_the_cause() {
        assert_eq!(
            AppleSignInError::WrongCode.to_string(),
            "the verification code is incorrect"
        );
        assert!(AppleSignInError::SecurityKeyRequired
            .to_string()
            .contains("API key"));
        assert_eq!(
            AppleSignInError::invalid_input("the code must be 6 digits").to_string(),
            "invalid input: the code must be 6 digits; nothing was sent to Apple"
        );
    }
}
