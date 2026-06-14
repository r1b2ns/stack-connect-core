//! App Store Connect plugin (the first concrete provider).
//!
//! Auth is an ES256 team JWT (`auth::es256`); the API is JSON:API with
//! `links.next` pagination.

mod client;
mod provider;

use crate::service::kind::CredentialField;

pub(crate) use provider::AppStoreProvider;

/// Credential keys this provider reads from the host `CredentialStore`, in the
/// exact order `registry::build` consults them.
pub(crate) const KEY_ISSUER_ID: &str = "issuerId";
pub(crate) const KEY_KEY_ID: &str = "keyId";
pub(crate) const KEY_PRIVATE_KEY_P8: &str = "privateKeyP8";

/// The credential form the host renders to connect an App Store Connect account.
pub(crate) fn credential_schema() -> Vec<CredentialField> {
    vec![
        CredentialField::new(KEY_ISSUER_ID, "Issuer ID", true, false),
        CredentialField::new(KEY_KEY_ID, "Key ID", true, false),
        CredentialField::new(KEY_PRIVATE_KEY_P8, "Private Key (.p8)", true, true),
    ]
}
