//! Google Play plugin (the second concrete provider).
//!
//! Auth is a Google service-account OAuth2 JWT-bearer exchange
//! (`auth::oauth_jwt`). Apps are listed through the Play Developer Reporting API
//! (`apps:search`, `nextPageToken` pagination): the Android Publisher API has no
//! list-apps endpoint. Everything else goes to the Android Publisher API
//! (`androidpublisher` v3): reviews (`reviews`) and the edit-scoped reads —
//! app details, store listings, tracks (`edits`).
//!
//! One connected account is one service account, which may see apps from
//! several Play developer accounts; `apps:search` returns them mixed together.
//!
//! Modules: `client` (transport, URL building, Reporting calls), `error`
//! (Google error mapping), `reviews`, `edits`, `timestamp` (Google's number
//! and date encodings) and `provider` (the capability wiring).

mod client;
mod edits;
mod error;
mod provider;
mod reviews;
#[cfg(test)]
mod test_support;
mod timestamp;

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
/// the iOS implementation): the Android Publisher API for reviews, details,
/// listings and tracks, and the Developer Reporting API for listing apps.
pub(crate) const PLAY_SCOPES: &str = "https://www.googleapis.com/auth/androidpublisher \
     https://www.googleapis.com/auth/playdeveloperreporting";

/// `value` unless it is absent or blank. Google's proto3 JSON omits empty
/// strings, so "blank" and "absent" mean the same thing on the wire.
fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

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
    fn non_blank_drops_absent_empty_and_whitespace() {
        assert_eq!(non_blank(None), None);
        assert_eq!(non_blank(Some(String::new())), None);
        assert_eq!(non_blank(Some(" \t\n".into())), None);
        // Kept verbatim, surrounding whitespace included.
        assert_eq!(non_blank(Some(" a ".into())), Some(" a ".into()));
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
