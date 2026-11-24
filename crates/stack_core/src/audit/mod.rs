//! Structured network audit: an optional, host-implemented
//! [`crate::ports::NetworkAuditSink`] receives one [`NetworkAuditEntry`] per
//! HTTP exchange the core performs (each retry attempt and each transport
//! failure included), with the raw request and response, timing, sizes, and
//! why the call happened ([`NetworkAuditEntry::operation`]).
//!
//! - [`scope`]: the task-local context (sink, account, operation, attempt)
//!   the exported entry points enter and a few call sites narrow;
//! - [`recorder`]: builds and emits the entry at the HTTP choke point
//!   ([`crate::http_debug`]), which every request goes through;
//! - [`credential`]: the credential hint and JWT expiry read from the
//!   request.
//!
//! Independent of the debug cURL trace ([`crate::ports::DebugLogger`]) and
//! the redacted host log ([`crate::ports::CoreLogger`]): it neither feeds nor
//! changes them. Without a sink nothing is captured or allocated.

pub(crate) mod credential;
pub(crate) mod recorder;
pub(crate) mod scope;
pub(crate) mod types;

pub(crate) use scope::{AuditBinding, AuditTarget};
pub use types::{
    NetworkAuditBodyState, NetworkAuditCredentialKind, NetworkAuditEntry, NetworkAuditHeader,
    NetworkAuditProvider,
};

/// Operation names of the internal steps that are not exported calls of
/// their own (the exported ones are `<handle>.<method>`; see the README's
/// *Network audit* section for the full list).
pub(crate) mod ops {
    /// A service account's OAuth 2.0 token exchange (JWT-bearer assertion
    /// for an access token).
    pub(crate) const GOOGLE_OAUTH_TOKEN: &str = "google.oauth.token";
    /// The Apple ID web session switching to the account's team.
    pub(crate) const APPLE_SESSION_SELECT_TEAM: &str = "apple_session.select_team";
    /// The Apple ID web session resolving the account's Developer Portal team.
    pub(crate) const APPLE_SESSION_PORTAL_TEAM: &str = "apple_session.portal_team";
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
