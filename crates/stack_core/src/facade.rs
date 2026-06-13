use std::sync::Arc;

use crate::api::play::PlayClient;
use crate::auth::ServiceAccount;
use crate::domain::AppInfo;
use crate::error::StackError;
use crate::ports::{CredentialStore, SERVICE_ACCOUNT_KEY};

/// UniFFI-exported entry point for the Google Play provider.
#[derive(uniffi::Object)]
pub struct PlayProvider {
    client: PlayClient,
}

#[uniffi::export]
impl PlayProvider {
    /// Builds a provider from a raw Google service-account JSON string.
    #[uniffi::constructor]
    pub fn new(service_account_json: String) -> Result<Arc<Self>, StackError> {
        let account = ServiceAccount::from_json(&service_account_json)?;
        Ok(Arc::new(Self {
            client: PlayClient::new(account),
        }))
    }

    /// Builds a provider by reading the service-account JSON from a native
    /// `CredentialStore` (e.g. the iOS Keychain) — exercises the callback boundary.
    #[uniffi::constructor]
    pub fn with_credentials(
        store: Arc<dyn CredentialStore>,
        account_id: String,
    ) -> Result<Arc<Self>, StackError> {
        let json = store
            .secret(account_id, SERVICE_ACCOUNT_KEY.to_string())
            .ok_or_else(|| StackError::invalid_credentials("missing service-account JSON"))?;
        Self::new(json)
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl PlayProvider {
    /// Lists the developer's apps from the Play Developer Reporting API.
    pub async fn search_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        self.client.search_apps().await
    }
}
