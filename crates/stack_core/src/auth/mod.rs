mod jwt;
mod oauth;
mod service_account;

pub(crate) use oauth::GoogleAuthenticator;
pub(crate) use service_account::ServiceAccount;

/// Default OAuth scopes for the Google Play provider (androidpublisher + reporting).
pub(crate) const PLAY_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/androidpublisher",
    "https://www.googleapis.com/auth/playdeveloperreporting",
];
