//! `stack_core` — shared Rust core for [stack-connect](../stack-connect).
//!
//! A multi-service hub: each external service is a plugin implementing a uniform
//! contract. App Store Connect and Google Play (apps, reviews, app details, store
//! listings, tracks, app icons) are in the core today; Firebase, AWS and GitHub
//! may follow. The UniFFI facade exposes a stable surface — `available_services`, `credential_schema`, `connect` →
//! `Provider` — that does not change as plugins are added. Consumed natively by
//! iOS via UniFFI and by the Flutter apps via flutter_rust_bridge (`frb`
//! feature).

uniffi::setup_scaffolding!();

mod apple_id;
mod auth;
mod domain;
mod error;
mod facade;
mod http_debug;
mod logging;
mod ports;
mod providers;
mod service;

// `.scexport` account-export crypto (cross-compatible with iOS). Consumed by the
// FRB facade; also compiled under `cargo test` so its unit tests run by default.
// Kept out of the default iOS staticlib build (no consumer there) to avoid
// pulling the crypto stack in unused.
#[cfg(any(feature = "frb", test))]
mod crypto;

// Second binding: flutter_rust_bridge (Dart). Entirely behind the `frb` feature
// (default OFF) so the iOS staticlib and the default test build are unaffected.
// `frb_api` is the hand-written FRB facade; `frb_generated` is produced by
// `flutter_rust_bridge_codegen generate` and must not be edited by hand.
#[cfg(feature = "frb")]
pub mod frb_api;
#[cfg(feature = "frb")]
mod frb_generated;

/// Apple ID web sign-in internals, exposed only for the dev-only
/// `apple_signin_probe` binary (`--features apple-signin-probe`). The hosts
/// use the exported [`AppleIdSignIn`] and [`connect_with_apple_session`]
/// instead.
#[cfg(feature = "apple-signin-probe")]
pub mod apple_signin {
    pub use crate::auth::apple_session::error::AppleSignInError;
    pub use crate::auth::apple_session::flow::{
        AppleCodeDelivery, AppleCodeMethod, AppleSignIn, AppleSignInStep, AppleTrustedPhone,
    };
    pub use crate::auth::apple_session::portal::PORTAL_PROTOCOL_VERSION;
    pub use crate::auth::apple_session::probe::{
        failure_reason, portal_teams, summarize_errors, ProbeHost, ProbeReply, ProbeRequest,
        SessionProbe,
    };
    pub use crate::auth::apple_session::session::{
        AppleSession, AppleTeam, OlympusSession, SessionBlob, SessionResponse,
    };
    pub use crate::auth::apple_session::srp::Password;
    pub use crate::auth::apple_session::AppleEndpoints;
}

pub use apple_id::AppleIdSignIn;
pub use auth::apple_session::flow::{
    AppleCodeDelivery, AppleCodeMethod, AppleSignInStep, AppleTrustedPhone,
};
pub use auth::apple_session::session::AppleTeam;
pub use domain::{
    AccountExport, AnalyticsReport, AnalyticsReportData, AnalyticsReportInstance,
    AnalyticsReportInstancesPage, AnalyticsReportRequest, AnalyticsReportRequestsPage,
    AnalyticsReportSegment, AnalyticsReportSegmentsPage, AnalyticsReportsPage, AppDetailsInfo,
    AppInfo, AppReviewDetailInfo, AppStoreLocalizationInfo, AppStoreVersionInfo, BetaGroupInfo,
    BetaTesterInfo, BuildDetailInfo, BuildInfo, BuildsPage, BundleInfo, ChangesInReviewBehavior,
    CommitOptions, CountryTargetingInfo, CustomerReview, CustomerReviewsPage, LocalizedTextInfo,
    NewRelease, NewReleaseStatus, PhasedReleaseInfo, PlayGrantInfo, PlayUserInfo, ReleaseChange,
    ReleaseMetadataUpdate, ReleaseSummaryInfo, ReviewResponse, ReviewSubmission, ScreenshotInfo,
    ScreenshotSetInfo, StoreListingInfo, TrackInfo, TrackReleaseInfo,
};
pub use error::StackError;
pub use facade::{
    available_services, connect, connect_with_access_token, connect_with_apple_session,
    credential_schema, make_sync_service, set_core_logger,
};
pub use logging::LogLevel;
pub use ports::{AccessTokenProvider, BlobStore, CoreLogger, CredentialStore, SessionStore};
pub use service::capabilities::analytics::Analytics;
pub use service::capabilities::app_details::AppDetails;
pub use service::capabilities::app_icons::AppIcons;
pub use service::capabilities::app_store_versions::AppStoreVersions;
pub use service::capabilities::beta_groups::BetaGroups;
pub use service::capabilities::builds::Builds;
pub use service::capabilities::play_users::PlayUsers;
pub use service::capabilities::reviews::Reviews;
pub use service::capabilities::store_listings::StoreListings;
pub use service::capabilities::tracks::Tracks;
pub use service::kind::{CredentialField, ServiceKind};
pub use service::provider::{Capability, Provider};
pub use service::sync::SyncService;
