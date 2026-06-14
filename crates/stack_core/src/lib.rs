//! `stack_core` — shared Rust core for [stack-connect](../stack-connect).
//!
//! A multi-service hub: each external service (App Store Connect today; Firebase,
//! Google Play, AWS, GitHub later) is a plugin implementing a uniform contract.
//! The UniFFI facade exposes a stable surface — `available_services`,
//! `credential_schema`, `connect` → `Provider` — that does not change as plugins
//! are added. Consumed natively by iOS via UniFFI.

uniffi::setup_scaffolding!();

mod auth;
mod domain;
mod error;
mod facade;
mod ports;
mod providers;
mod service;

pub use domain::{AppInfo, CustomerReview, ReviewResponse, ReviewSubmission};
pub use error::StackError;
pub use facade::{available_services, connect, credential_schema, make_sync_service};
pub use ports::{BlobStore, CredentialStore};
pub use service::capabilities::reviews::Reviews;
pub use service::kind::{CredentialField, ServiceKind};
pub use service::provider::{Capability, Provider};
pub use service::sync::SyncService;
