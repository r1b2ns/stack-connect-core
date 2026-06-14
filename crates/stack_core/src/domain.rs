/// Cross-provider app metadata. Mirrors the Swift `AppInfo` from `StackProtocols`,
/// which is dissolved into the core (see RUST_CORE_PLAN.md §4).
///
/// Serializes camelCase (`bundleId`, not `bundle_id`) so persisted blobs match the
/// iOS-facing contract — see [`crate::service::sync::SyncService`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub bundle_id: String,
    pub platform: Option<String>,
}

/// The developer's response attached to a [`CustomerReview`]. Dates are raw
/// ISO8601 strings; the core does no date parsing (the host owns that).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct ReviewResponse {
    pub id: String,
    pub body: Option<String>,
    pub state: Option<String>,
    pub last_modified_date: Option<String>,
}

/// A single end-user App Store review, optionally with the developer's response.
/// Dates are raw ISO8601 strings; the core does no date parsing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct CustomerReview {
    pub id: String,
    pub rating: i32,
    pub title: Option<String>,
    pub body: Option<String>,
    pub reviewer_nickname: Option<String>,
    pub created_date: Option<String>,
    pub territory: Option<String>,
    pub response: Option<ReviewResponse>,
}

/// One page of customer reviews plus an opaque token to fetch the next page.
/// `next_token` is `None` on the last page; otherwise pass it back verbatim as
/// the next call's `page_token` (it is the JSON:API `links.next` URL).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CustomerReviewsPage {
    pub reviews: Vec<CustomerReview>,
    pub next_token: Option<String>,
}

/// A review submission to App Store review (the act of submitting an app version
/// for review), with the resolved version and submitter where available. Dates
/// are raw ISO8601 strings; the core does no date parsing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct ReviewSubmission {
    pub id: String,
    pub app_id: String,
    pub platform: Option<String>,
    pub submitted_date: Option<String>,
    pub state: Option<String>,
    pub version_string: Option<String>,
    pub version_id: Option<String>,
    pub submitted_by_name: Option<String>,
    pub submitted_by_email: Option<String>,
}

/// An App Store version of an app. Dates are raw ISO8601 strings; the core does
/// no date parsing (the host owns that).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct AppStoreVersionInfo {
    pub id: String,
    pub app_id: String,
    pub platform: Option<String>,
    pub app_store_state: Option<String>,
    pub app_version_state: Option<String>,
    pub version_string: Option<String>,
    pub copyright: Option<String>,
    pub release_type: Option<String>,
    pub created_date: Option<String>,
}

/// A build (TestFlight / App Store Connect) of an app. `version` is the build
/// number (the ASC `version` attribute, distinct from a version string). Dates
/// are raw ISO8601 strings; the core does no date parsing (the host owns that).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub id: String,
    pub app_id: String,
    pub version: Option<String>,
    pub uploaded_date: Option<String>,
    pub expired: Option<bool>,
    pub processing_state: Option<String>,
    pub min_os_version: Option<String>,
    pub expiration_date: Option<String>,
}
