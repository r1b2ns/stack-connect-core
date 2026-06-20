//! flutter_rust_bridge (FRB) facade: the public surface the Dart binding sees.
//!
//! This is a *second* binding alongside the UniFFI/Swift facade in
//! [`crate::facade`]. It is compiled only under the `frb` cargo feature
//! (default OFF) so the iOS staticlib and the default `cargo test` are
//! unaffected.
//!
//! Like the UniFFI facade, it owns its binding concerns: the core (`service`,
//! `providers`, `auth`) stays binding-agnostic. FRB mirrors the returned
//! [`crate::service::kind::ServiceKind`] into a Dart enum automatically; the
//! `uniffi::Enum` derive it carries does not interfere with FRB codegen.

use crate::service::kind::ServiceKind;
use crate::service::registry;

/// Every service the core can connect today, for the Dart host's service picker.
///
/// Calls the same real core logic as the UniFFI facade
/// ([`crate::facade::available_services`]) — `registry::available_services()` —
/// and hands the result to the FRB binding. FRB mirrors [`ServiceKind`] into a
/// matching Dart enum, so the Dart side receives the real value from real core
/// code.
///
/// # Examples
///
/// ```
/// # use stack_core::frb_api::available_services;
/// # use stack_core::ServiceKind;
/// assert_eq!(available_services(), vec![ServiceKind::AppStoreConnect]);
/// ```
#[flutter_rust_bridge::frb(sync)]
pub fn available_services() -> Vec<ServiceKind> {
    registry::available_services()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_services_returns_app_store_connect() {
        assert_eq!(available_services(), vec![ServiceKind::AppStoreConnect]);
    }
}
