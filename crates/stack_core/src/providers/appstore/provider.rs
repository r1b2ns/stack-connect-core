use std::sync::Arc;

use async_trait::async_trait;

use super::client::AppStoreClient;
use crate::auth::es256::AppStoreAuthenticator;
use crate::domain::{AppInfo, CustomerReview, ReviewResponse, ReviewSubmission};
use crate::error::StackError;
use crate::service::capabilities::reviews::{Reviews, ReviewsImpl};
use crate::service::kind::ServiceKind;
use crate::service::provider::{Capability, ProviderImpl};

/// App Store Connect implementation of the internal [`ProviderImpl`] contract.
///
/// The [`AppStoreClient`] is held behind an `Arc` so capability sub-objects (the
/// [`Reviews`] handle) share the same client — and therefore the same
/// authenticator and token cache — as `validate`/`fetch_apps`.
pub(crate) struct AppStoreProvider {
    client: Arc<AppStoreClient>,
}

impl AppStoreProvider {
    /// Builds the provider from the three required credentials.
    pub(crate) fn new(issuer_id: String, key_id: String, private_key_p8: Vec<u8>) -> Self {
        let auth = AppStoreAuthenticator::new(issuer_id, key_id, private_key_p8);
        Self {
            client: Arc::new(AppStoreClient::new(auth)),
        }
    }
}

#[async_trait]
impl ProviderImpl for AppStoreProvider {
    fn kind(&self) -> ServiceKind {
        ServiceKind::AppStoreConnect
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Apps, Capability::Reviews]
    }

    async fn validate(&self) -> Result<(), StackError> {
        self.client.validate().await
    }

    async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        self.client.fetch_apps().await
    }

    fn reviews(&self) -> Option<Arc<Reviews>> {
        Some(Reviews::new(Box::new(AppStoreReviews {
            client: Arc::clone(&self.client),
        })))
    }
}

/// App Store Connect implementation of the [`ReviewsImpl`] capability contract.
/// Holds a shared [`AppStoreClient`] so it reuses the provider's token cache.
struct AppStoreReviews {
    client: Arc<AppStoreClient>,
}

#[async_trait]
impl ReviewsImpl for AppStoreReviews {
    async fn fetch_customer_reviews(
        &self,
        app_id: String,
    ) -> Result<Vec<CustomerReview>, StackError> {
        self.client.fetch_customer_reviews(&app_id).await
    }

    async fn fetch_review_submissions(
        &self,
        app_id: String,
    ) -> Result<Vec<ReviewSubmission>, StackError> {
        self.client.fetch_review_submissions(&app_id).await
    }

    async fn reply_to_review(
        &self,
        review_id: String,
        body: String,
    ) -> Result<ReviewResponse, StackError> {
        self.client.reply_to_review(&review_id, &body).await
    }

    async fn delete_review_response(&self, response_id: String) -> Result<(), StackError> {
        self.client.delete_review_response(&response_id).await
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
        assert_eq!(
            p.capabilities(),
            vec![Capability::Apps, Capability::Reviews]
        );
    }

    #[test]
    fn exposes_reviews_capability_handle() {
        // App Store Connect supports Reviews, so the accessor must return `Some`.
        // (Appstore is the only provider today; a `None` provider cannot yet be
        // constructed to assert the unsupported branch.)
        assert!(provider().reviews().is_some());
        assert!(provider().capabilities().contains(&Capability::Reviews));
    }
}
