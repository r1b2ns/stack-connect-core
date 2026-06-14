//! UniFFI-exported facade: the entire public surface Swift sees. Everything else
//! in the crate is internal. The facade owns binding concerns so `service`,
//! `providers`, and `auth` stay binding-agnostic.

use std::sync::Arc;

use crate::error::StackError;
use crate::ports::{BlobStore, CredentialStore};
use crate::service::kind::{CredentialField, ServiceKind};
use crate::service::provider::Provider;
use crate::service::registry;
use crate::service::sync::SyncService;

/// Every service the core can connect today. Drives the host's service picker.
#[uniffi::export]
pub fn available_services() -> Vec<ServiceKind> {
    registry::available_services()
}

/// The credential form the host should render to connect an account of `kind`.
#[uniffi::export]
pub fn credential_schema(kind: ServiceKind) -> Vec<CredentialField> {
    registry::credential_schema(kind)
}

/// Reads the secrets for `(kind, account_id)` from the host `store` and builds a
/// connected [`Provider`].
///
/// Synchronous on purpose: it only reads secrets through the (synchronous)
/// callback and parses the key material — no network. The returned provider does
/// the async work (`validate`, `fetch_apps`).
///
/// # Errors
/// [`StackError::InvalidCredentials`] if a required secret is missing.
#[uniffi::export]
pub fn connect(
    kind: ServiceKind,
    account_id: String,
    store: Arc<dyn CredentialStore>,
) -> Result<Arc<Provider>, StackError> {
    let inner = registry::build(kind, &account_id, &store)?;
    Ok(Provider::new(inner))
}

/// Builds a [`SyncService`] that syncs `provider` into the host `store` for
/// `account_id`. The account id is stamped into every persisted app blob
/// (`accountId`) so the host can derive its composite key and attribute apps.
///
/// Synchronous on purpose: it only wires the handles together. The returned
/// object does the async work (`sync_apps`).
#[uniffi::export]
pub fn make_sync_service(
    provider: Arc<Provider>,
    store: Arc<dyn BlobStore>,
    account_id: String,
) -> Arc<SyncService> {
    SyncService::new(provider, store, account_id)
}
