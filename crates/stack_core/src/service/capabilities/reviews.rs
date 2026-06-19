use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{CustomerReview, CustomerReviewsPage, ReviewResponse, ReviewSubmission};
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

    /// Fetches a single page of customer reviews for incremental (load-more)
    /// paging, returning the page's reviews plus an opaque `next_token`.
    async fn fetch_customer_reviews_page(
        &self,
        app_id: String,
        sort: String,
        filter_rating: Vec<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<CustomerReviewsPage, StackError>;

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

    /// Resubmits the draft review submission identified by `submission_id`.
    async fn submit_review_submission(&self, submission_id: String) -> Result<(), StackError>;

    /// Discards the review submission identified by `submission_id`, branching on
    /// its current state to clear a stale draft or cancel an in-flight
    /// submission.
    async fn discard_review_submission(&self, submission_id: String) -> Result<(), StackError>;
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

    /// Fetches a single page of customer reviews for incremental (load-more)
    /// paging, returning the page's reviews plus an opaque `next_token`.
    ///
    /// `sort` is the raw ASC sort value (`-createdDate` | `createdDate` |
    /// `-rating` | `rating`), passed through unchanged. `filter_rating` is empty
    /// for no filter, else the ratings to include. `page_token` is `None` for the
    /// first page; otherwise pass back a previous call's `next_token` verbatim.
    /// `next_token` is `None` once the last page has been reached.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_customer_reviews_page(
        &self,
        app_id: String,
        sort: String,
        filter_rating: Vec<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<CustomerReviewsPage, StackError> {
        self.inner
            .fetch_customer_reviews_page(app_id, sort, filter_rating, limit, page_token)
            .await
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

    /// Resubmits the draft review submission identified by `submission_id` by
    /// setting its `submitted` attribute to `true`. Use this to re-send a
    /// submission that was left in `READY_FOR_REVIEW`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn submit_review_submission(&self, submission_id: String) -> Result<(), StackError> {
        self.inner.submit_review_submission(submission_id).await
    }

    /// Discards the review submission identified by `submission_id`, branching on
    /// its current state: an in-flight submission (`WAITING_FOR_REVIEW` /
    /// `IN_REVIEW` / `UNRESOLVED_ISSUES`) is canceled, a not-yet-submitted draft
    /// (`READY_FOR_REVIEW`) is emptied of its items (returning its version to
    /// `PREPARE_FOR_SUBMISSION`), and any other/absent state is a no-op. A
    /// submission that no longer exists (`404`) is also a no-op. Use this to
    /// clear stale drafts that block creating a new submission.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response (other than
    /// the lookup's 404, which is treated as already-discarded),
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn discard_review_submission(&self, submission_id: String) -> Result<(), StackError> {
        self.inner.discard_review_submission(submission_id).await
    }
}
