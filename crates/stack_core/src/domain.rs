/// Cross-provider app metadata. Mirrors the Swift `AppInfo` from `StackProtocols`,
/// which is dissolved into the core (see RUST_CORE_PLAN.md §4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
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
