use async_trait::async_trait;

use super::client::AppStoreClient;
use crate::auth::es256::AppStoreAuthenticator;
use crate::domain::AppInfo;
use crate::error::StackError;
use crate::service::kind::ServiceKind;
use crate::service::provider::{Capability, ProviderImpl};

/// App Store Connect implementation of the internal [`ProviderImpl`] contract.
pub(crate) struct AppStoreProvider {
    client: AppStoreClient,
}

impl AppStoreProvider {
    /// Builds the provider from the three required credentials.
    pub(crate) fn new(issuer_id: String, key_id: String, private_key_p8: Vec<u8>) -> Self {
        let auth = AppStoreAuthenticator::new(issuer_id, key_id, private_key_p8);
        Self {
            client: AppStoreClient::new(auth),
        }
    }
}

#[async_trait]
impl ProviderImpl for AppStoreProvider {
    fn kind(&self) -> ServiceKind {
        ServiceKind::AppStoreConnect
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Apps]
    }

    async fn validate(&self) -> Result<(), StackError> {
        self.client.validate().await
    }

    async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        self.client.fetch_apps().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> AppStoreProvider {
        AppStoreProvider::new(
            "issuer".into(),
            "kid".into(),
            include_bytes!("../../../tests/fixtures/test_ec_private.p8").to_vec(),
        )
    }

    #[test]
    fn reports_kind_and_capabilities() {
        let p = provider();
        assert_eq!(p.kind(), ServiceKind::AppStoreConnect);
        assert_eq!(p.capabilities(), vec![Capability::Apps]);
    }
}
