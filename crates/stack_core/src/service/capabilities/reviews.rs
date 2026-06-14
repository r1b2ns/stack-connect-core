use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{CustomerReview, ReviewResponse, ReviewSubmission};
use crate::error::StackError;

/// Internal, non-exported contract for the Reviews capability. Kept off the FFI
/// for the same reason as [`crate::service::provider::ProviderImpl`]: UniFFI cannot
/// export an async *trait* cleanly, so the public surface is the concrete
/// [`Reviews`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn ReviewsImpl>` can live inside an `Arc<Reviews>`
/// shared across the tokio runtime.
///
/// Covers both reads (list reviews/submissions) and writes (reply to a review,
/// delete a response) — see RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait ReviewsImpl: Send + Sync {
    /// Lists the end-user reviews for `app_id`, newest first, including any
    /// developer responses.
    async fn fetch_customer_reviews(
        &self,
        app_id: String,
    ) -> Result<Vec<CustomerReview>, StackError>;

    /// Lists the review submissions for `app_id`, with resolved version and
    /// submitter where available.
    async fn fetch_review_submissions(
        &self,
        app_id: String,
    ) -> Result<Vec<ReviewSubmission>, StackError>;

    /// Creates or replaces the developer response for `review_id` with `body`,
    /// returning the resulting response.
    async fn reply_to_review(
        &self,
        review_id: String,
        body: String,
    ) -> Result<ReviewResponse, StackError>;

    /// Deletes the developer response identified by `response_id`.
    async fn delete_review_response(&self, response_id: String) -> Result<(), StackError>;
}

/// UniFFI-exported Reviews capability handle. A thin, binding-friendly wrapper
/// around a boxed [`ReviewsImpl`]; async work runs on the tokio runtime. Reached
/// via [`crate::service::provider::Provider::reviews`].
#[derive(uniffi::Object)]
pub struct Reviews {
    inner: Box<dyn ReviewsImpl>,
}

impl Reviews {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn ReviewsImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Reviews {
    /// Lists the end-user reviews for `app_id`, newest first, including any
    /// developer responses.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_customer_reviews(
        &self,
        app_id: String,
    ) -> Result<Vec<CustomerReview>, StackError> {
        self.inner.fetch_customer_reviews(app_id).await
    }

    /// Lists the review submissions for `app_id`, with resolved version and
    /// submitter where available.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx page, [`StackError::Decode`] on malformed
    /// JSON, or [`StackError::Network`] on transport failure.
    pub async fn fetch_review_submissions(
        &self,
        app_id: String,
    ) -> Result<Vec<ReviewSubmission>, StackError> {
        self.inner.fetch_review_submissions(app_id).await
    }

    /// Creates or replaces the developer response for `review_id` with `body`,
    /// returning the resulting response. Posting again for the same review
    /// replaces the existing response (upsert).
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response, [`StackError::Decode`] on
    /// malformed JSON, or [`StackError::Network`] on transport failure.
    pub async fn reply_to_review(
        &self,
        review_id: String,
        body: String,
    ) -> Result<ReviewResponse, StackError> {
        self.inner.reply_to_review(review_id, body).await
    }

    /// Deletes the developer response identified by `response_id`.
    ///
    /// # Errors
    /// [`StackError::Http`] on a non-2xx response or [`StackError::Network`] on
    /// transport failure.
    pub async fn delete_review_response(&self, response_id: String) -> Result<(), StackError> {
        self.inner.delete_review_response(response_id).await
    }
}
