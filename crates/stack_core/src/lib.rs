//! `stack_core` — shared Rust core for [stack-connect](../stack-connect).
//!
//! Phase 0 scope: the Google Play provider (`apps:search`) end-to-end, including
//! service-account RS256 JWT auth, OAuth2 token caching, and the UniFFI facade.

uniffi::setup_scaffolding!();

mod api;
mod auth;
mod domain;
mod error;
mod facade;
mod ports;

pub use domain::AppInfo;
pub use error::StackError;
pub use facade::PlayProvider;
pub use ports::CredentialStore;
