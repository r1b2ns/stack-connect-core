//! Google Play plugin (the second concrete provider).
//!
//! Auth is a Google service-account OAuth2 JWT-bearer exchange
//! (`auth::oauth_jwt`). Apps are listed through the Play Developer Reporting API
//! (`apps:search`, `nextPageToken` pagination): the Android Publisher API has no
//! list-apps endpoint.
//!
//! One connected account is one service account, which may see apps from
//! several Play developer accounts; `apps:search` returns them mixed together.

mod client;
mod error;
mod provider;

use crate::service::kind::CredentialField;

pub(crate) use provider::GooglePlayProvider;

/// Credential keys this provider reads from the host `CredentialStore`, in the
/// exact order `registry::build` consults them. The host extracts them from the
/// service account's JSON key file (`client_email`, `private_key_id`,
/// `private_key`).
pub(crate) const KEY_CLIENT_EMAIL: &str = "clientEmail";
pub(crate) const KEY_PRIVATE_KEY_ID: &str = "privateKeyId";
pub(crate) const KEY_PRIVATE_KEY: &str = "privateKey";

/// OAuth scopes requested for every Google Play token (space-separated, as in
/// the iOS implementation): the Android Publisher API for future capabilities
/// and the Developer Reporting API for listing apps.
pub(crate) const PLAY_SCOPES: &str = "https://www.googleapis.com/auth/androidpublisher \
     https://www.googleapis.com/auth/playdeveloperreporting";

/// The credential form the host renders to connect a Google Play account. The
/// service account email is not secret; the key ID and the PEM key are.
pub(crate) fn credential_schema() -> Vec<CredentialField> {
    vec![
        CredentialField::new(KEY_CLIENT_EMAIL, "Service Account Email", false, false),
        CredentialField::new(KEY_PRIVATE_KEY_ID, "Private Key ID", true, false),
        CredentialField::new(KEY_PRIVATE_KEY, "Private Key (PEM)", true, true),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_keys_flags_and_order() {
        assert_eq!(
            credential_schema(),
            vec![
                CredentialField::new("clientEmail", "Service Account Email", false, false),
                CredentialField::new("privateKeyId", "Private Key ID", true, false),
                CredentialField::new("privateKey", "Private Key (PEM)", true, true),
            ]
        );
    }

    #[test]
    fn play_scopes_cover_publisher_and_reporting() {
        assert_eq!(
            PLAY_SCOPES,
            "https://www.googleapis.com/auth/androidpublisher \
             https://www.googleapis.com/auth/playdeveloperreporting"
        );
        assert_eq!(PLAY_SCOPES.split(' ').count(), 2);
    }
}
