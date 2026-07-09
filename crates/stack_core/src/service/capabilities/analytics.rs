use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{
    AnalyticsReportData, AnalyticsReportInstancesPage, AnalyticsReportRequest,
    AnalyticsReportRequestsPage, AnalyticsReportSegmentsPage, AnalyticsReportsPage,
};
use crate::error::StackError;

/// Internal, non-exported contract for the Analytics (App Store Connect Analytics
/// Reports) capability. Kept off the FFI for the same reason as
/// [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an async
/// *trait* cleanly, so the public surface is the concrete [`Analytics`] object
/// below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn AnalyticsImpl>` can live inside an `Arc<Analytics>`
/// shared across the tokio runtime.
///
/// Covers the M0 reads (navigate report requests → reports → instances →
/// segments, each paginated), the M1 write (create a report request), and the L2
/// download (fetch a segment, gunzip it, parse the tab-delimited file into
/// generic headers + rows).
#[async_trait]
pub(crate) trait AnalyticsImpl: Send + Sync {
    /// Creates an analytics report request for `app_id` with `access_type` (the
    /// raw ASC value, e.g. `ONGOING`).
    async fn create_analytics_report_request(
        &self,
        app_id: String,
        access_type: String,
    ) -> Result<AnalyticsReportRequest, StackError>;

    /// Fetches a single page of the analytics report requests for `app_id`,
    /// optionally filtered by `filter_access_type`, returning the page's requests
    /// plus an opaque `next_token`.
    async fn fetch_analytics_report_requests_page(
        &self,
        app_id: String,
        filter_access_type: Option<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<AnalyticsReportRequestsPage, StackError>;

    /// Fetches a single page of the reports under the report request `request_id`,
    /// optionally filtered by `filter_category`, returning the page's reports plus
    /// an opaque `next_token`.
    async fn fetch_analytics_reports_page(
        &self,
        request_id: String,
        filter_category: Option<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<AnalyticsReportsPage, StackError>;

    /// Fetches a single page of the instances of the report `report_id`,
    /// optionally filtered by `filter_granularity`, returning the page's instances
    /// plus an opaque `next_token`.
    async fn fetch_analytics_report_instances_page(
        &self,
        report_id: String,
        filter_granularity: Option<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<AnalyticsReportInstancesPage, StackError>;

    /// Fetches a single page of the segments of the report instance `instance_id`,
    /// returning the page's segments plus an opaque `next_token`.
    async fn fetch_analytics_report_segments_page(
        &self,
        instance_id: String,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<AnalyticsReportSegmentsPage, StackError>;

    /// Downloads the segment at the pre-signed `url` (no bearer auth), gunzips it,
    /// and parses the tab-delimited payload into generic headers + rows,
    /// refusing transfers larger than `max_bytes`.
    async fn download_analytics_segment(
        &self,
        url: String,
        max_bytes: u64,
    ) -> Result<AnalyticsReportData, StackError>;
}

/// UniFFI-exported Analytics capability handle. A thin, binding-friendly wrapper
/// around a boxed [`AnalyticsImpl`]; async work runs on the tokio runtime.
/// Reached via [`crate::service::provider::Provider::analytics`].
#[derive(uniffi::Object)]
pub struct Analytics {
    inner: Box<dyn AnalyticsImpl>,
}

impl Analytics {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn AnalyticsImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Analytics {
    /// Creates an analytics report request for `app_id` with `access_type` (the
    /// raw ASC value — `ONGOING` / `ONE_TIME_SNAPSHOT` — passed through verbatim),
    /// returning the created request.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_analytics_report_request(
        &self,
        app_id: String,
        access_type: String,
    ) -> Result<AnalyticsReportRequest, StackError> {
        self.inner
            .create_analytics_report_request(app_id, access_type)
            .await
    }

    /// Fetches a single page of the analytics report requests for `app_id`.
    ///
    /// `filter_access_type` is `None` for no filter, else the raw ASC
    /// `accessType` to include. `page_token` is `None` for the first page;
    /// otherwise pass back a previous call's `next_token` verbatim. `next_token`
    /// is `None` once the last page has been reached.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_analytics_report_requests_page(
        &self,
        app_id: String,
        filter_access_type: Option<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<AnalyticsReportRequestsPage, StackError> {
        self.inner
            .fetch_analytics_report_requests_page(app_id, filter_access_type, limit, page_token)
            .await
    }

    /// Fetches a single page of the reports under the report request `request_id`.
    ///
    /// `filter_category` is `None` for no filter, else the raw ASC `category` to
    /// include. Paging semantics match
    /// [`Self::fetch_analytics_report_requests_page`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_analytics_reports_page(
        &self,
        request_id: String,
        filter_category: Option<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<AnalyticsReportsPage, StackError> {
        self.inner
            .fetch_analytics_reports_page(request_id, filter_category, limit, page_token)
            .await
    }

    /// Fetches a single page of the instances of the report `report_id`.
    ///
    /// `filter_granularity` is `None` for no filter, else the raw ASC
    /// `granularity` to include (`DAILY` / `WEEKLY` / `MONTHLY`). Paging semantics
    /// match [`Self::fetch_analytics_report_requests_page`].
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_analytics_report_instances_page(
        &self,
        report_id: String,
        filter_granularity: Option<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<AnalyticsReportInstancesPage, StackError> {
        self.inner
            .fetch_analytics_report_instances_page(report_id, filter_granularity, limit, page_token)
            .await
    }

    /// Fetches a single page of the segments of the report instance `instance_id`.
    ///
    /// Paging semantics match [`Self::fetch_analytics_report_requests_page`]
    /// (there is no filter here).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_analytics_report_segments_page(
        &self,
        instance_id: String,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<AnalyticsReportSegmentsPage, StackError> {
        self.inner
            .fetch_analytics_report_segments_page(instance_id, limit, page_token)
            .await
    }

    /// Downloads the analytics segment at the pre-signed `url`, gunzips it, and
    /// parses the tab-delimited payload into generic headers + rows. No bearer
    /// token is sent (the URL is pre-signed). Transfers advertising, or growing
    /// past, `max_bytes` are refused so the host can download such large segments
    /// via the URL directly.
    ///
    /// # Errors
    /// [`StackError::Unsupported`] when the segment exceeds `max_bytes`,
    /// [`StackError::PendingAgreements`] on a pending-agreements 403,
    /// [`StackError::Http`] on any other non-2xx response, [`StackError::Decode`]
    /// when the gzip payload cannot be inflated or is not valid UTF-8, or
    /// [`StackError::Network`] on transport failure.
    pub async fn download_analytics_segment(
        &self,
        url: String,
        max_bytes: u64,
    ) -> Result<AnalyticsReportData, StackError> {
        self.inner.download_analytics_segment(url, max_bytes).await
    }
}
