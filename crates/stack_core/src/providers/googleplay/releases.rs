//! Release summaries of one track (`applications.tracks.releases.list`).
//!
//! The only release read the Android Publisher API serves **outside** an edit:
//! it never opens one, so it cannot invalidate an edit the same service account
//! has open elsewhere (e.g. a CI upload), and it is safe on automatic paths
//! (list load, pull to refresh, background sync). It is also the only read that
//! carries the review / lifecycle state of a release, which `edits.tracks` does
//! not.

use serde::Deserialize;

use super::client::GooglePlayClient;
use super::error::{ErrorHints, GoogleApi, Target, PERMISSION_VIEW_APP_INFO};
use super::non_blank;
use super::timestamp::Int64;
use crate::domain::ReleaseSummaryInfo;
use crate::error::StackError;
use crate::logging::{self, target};

/// The `tracks` collection under an app (outside an edit).
const TRACKS: &str = "tracks";
/// The `releases` collection under a track.
const RELEASES: &str = "releases";

/// `ListReleaseSummariesResponse`. Proto3 JSON omits an empty `releases`.
#[derive(Deserialize)]
struct ListReleaseSummariesResponse {
    #[serde(default)]
    releases: Vec<PlayReleaseSummary>,
}

/// A `ReleaseSummary`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayReleaseSummary {
    #[serde(default)]
    track: Option<String>,
    #[serde(default)]
    release_name: Option<String>,
    #[serde(default)]
    release_lifecycle_state: Option<String>,
    #[serde(default)]
    active_artifacts: Vec<PlayArtifactSummary>,
}

/// An `ArtifactSummary` (`versionCode` is int32 in the discovery document; a
/// string is accepted as well).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayArtifactSummary {
    #[serde(default)]
    version_code: Option<Int64>,
}

impl PlayReleaseSummary {
    /// Maps to a [`ReleaseSummaryInfo`]; `requested_track` stands in for an
    /// absent `track`. The lifecycle state is passed through verbatim, so a
    /// state Google adds later reaches the host unchanged.
    fn into_info(self, requested_track: &str) -> ReleaseSummaryInfo {
        ReleaseSummaryInfo {
            track: non_blank(self.track).unwrap_or_else(|| requested_track.to_string()),
            name: non_blank(self.release_name),
            lifecycle_state: non_blank(self.release_lifecycle_state),
            version_codes: self
                .active_artifacts
                .into_iter()
                .filter_map(|artifact| artifact.version_code.map(Int64::into_text))
                .collect(),
        }
    }
}

/// `2 releases: "1.2 (1002)" IN_REVIEW [1002]; "1.1" RELEASED [1001]`.
fn describe_summaries(summaries: &[ReleaseSummaryInfo]) -> String {
    if summaries.is_empty() {
        return "no releases".to_string();
    }
    let entries: Vec<String> = summaries
        .iter()
        .map(|summary| {
            let name = summary
                .name
                .as_deref()
                .map(|n| format!("{n:?} "))
                .unwrap_or_default();
            format!(
                "{name}{} [{}]",
                summary.lifecycle_state.as_deref().unwrap_or("?"),
                summary.version_codes.join(", ")
            )
        })
        .collect();
    format!("{} release(s): {}", summaries.len(), entries.join("; "))
}

impl GooglePlayClient {
    /// The summaries of the releases on `track` of `package_name`
    /// (`GET …/applications/{package}/tracks/{track}/releases`), in Google's
    /// order. Google returns at most 20 and leaves out obsolete releases.
    ///
    /// **No edit is opened**, so this never invalidates an edit open elsewhere.
    ///
    /// A `404` from Google is read as "this track does not exist" and answered
    /// with an empty list (Google cannot tell an unknown track from one without
    /// releases here in any way the client may rely on: the human message is
    /// the only difference). An invalid package name is still rejected locally
    /// with `Http 404` before any request, as everywhere else.
    ///
    /// # Errors
    /// [`StackError::Auth`] when Google denies access or the API is disabled,
    /// [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on a malformed body or an unusable track name
    /// (blank, `.` or `..`), or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_release_summaries(
        &self,
        package_name: &str,
        track: &str,
    ) -> Result<Vec<ReleaseSummaryInfo>, StackError> {
        let url = self.publisher_url(package_name, &[TRACKS, track, RELEASES])?;
        let hints = ErrorHints {
            api: GoogleApi::Publisher,
            permission: PERMISSION_VIEW_APP_INFO,
            target: Target::AppResource(package_name),
        };
        let body = match self.execute(|| self.http().get(url.clone()), &hints).await {
            Ok(body) => body,
            Err(StackError::Http { status: 404, .. }) => {
                logging::info(target::PLAY_TRACKS, || {
                    format!(
                        "release summaries of {package_name} track {track}: Google answered \
                         404, read as an unknown track; returning no releases"
                    )
                });
                return Ok(Vec::new());
            }
            Err(err) => return Err(err),
        };
        let response: ListReleaseSummariesResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("tracks.releases.list response: {e}")))?;
        let summaries: Vec<ReleaseSummaryInfo> = response
            .releases
            .into_iter()
            .map(|summary| summary.into_info(track))
            .collect();
        logging::debug(target::PLAY_TRACKS, || {
            format!(
                "release summaries of {package_name} track {track}: {}",
                describe_summaries(&summaries)
            )
        });
        Ok(summaries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::googleplay::test_support::{client, mount_token};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PACKAGE: &str = "com.example.app";
    const RELEASES_PATH: &str =
        "/androidpublisher/v3/applications/com.example.app/tracks/production/releases";

    fn list() -> wiremock::MockBuilder {
        Mock::given(method("GET")).and(path(RELEASES_PATH))
    }

    /// Paths of every request after the token exchange.
    async fn api_paths(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() != "/token")
            .map(|r| format!("{} {}", r.method, r.url.path()))
            .collect()
    }

    #[tokio::test]
    async fn maps_summaries_in_order() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "releases": [
                    {
                        "track": "production",
                        "releaseName": "4.2.0 (420)",
                        "releaseLifecycleState": "RELEASE_LIFECYCLE_STATE_IN_REVIEW",
                        "activeArtifacts": [{ "versionCode": 420 }, { "versionCode": "419" }]
                    },
                    {
                        "track": "production",
                        "releaseName": "4.1.0",
                        "releaseLifecycleState": "RELEASE_LIFECYCLE_STATE_PUBLISHED",
                        "activeArtifacts": [{ "versionCode": 410 }]
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let summaries = client(&server)
            .fetch_release_summaries(PACKAGE, "production")
            .await
            .unwrap();
        assert_eq!(
            summaries,
            vec![
                ReleaseSummaryInfo {
                    track: "production".into(),
                    name: Some("4.2.0 (420)".into()),
                    lifecycle_state: Some("RELEASE_LIFECYCLE_STATE_IN_REVIEW".into()),
                    version_codes: vec!["420".into(), "419".into()],
                },
                ReleaseSummaryInfo {
                    track: "production".into(),
                    name: Some("4.1.0".into()),
                    lifecycle_state: Some("RELEASE_LIFECYCLE_STATE_PUBLISHED".into()),
                    version_codes: vec!["410".into()],
                },
            ]
        );
    }

    #[tokio::test]
    async fn never_opens_an_edit() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "releases": [{ "releaseLifecycleState": "RELEASE_LIFECYCLE_STATE_DRAFT" }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        // Any `edits` call would be unmatched (404) and visible below.
        Mock::given(method("POST"))
            .and(path(
                "/androidpublisher/v3/applications/com.example.app/edits",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        client(&server)
            .fetch_release_summaries(PACKAGE, "production")
            .await
            .unwrap();
        assert_eq!(
            api_paths(&server).await,
            vec![format!("GET {RELEASES_PATH}")]
        );
    }

    #[tokio::test]
    async fn unknown_lifecycle_state_passes_through_and_blank_is_none() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "releases": [
                    { "releaseLifecycleState": "RELEASE_LIFECYCLE_STATE_FROM_THE_FUTURE" },
                    { "releaseLifecycleState": " ", "releaseName": "" },
                    { "activeArtifacts": [{}, { "versionCode": 7 }] }
                ]
            })))
            .mount(&server)
            .await;

        let summaries = client(&server)
            .fetch_release_summaries(PACKAGE, "production")
            .await
            .unwrap();
        assert_eq!(
            summaries[0].lifecycle_state.as_deref(),
            Some("RELEASE_LIFECYCLE_STATE_FROM_THE_FUTURE")
        );
        // An absent `track` falls back to the requested one.
        assert_eq!(summaries[0].track, "production");
        assert_eq!(summaries[1].lifecycle_state, None);
        assert_eq!(summaries[1].name, None);
        // An artifact without a version code is skipped.
        assert_eq!(summaries[2].version_codes, vec!["7".to_string()]);
    }

    #[tokio::test]
    async fn empty_object_is_empty() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        assert!(client(&server)
            .fetch_release_summaries(PACKAGE, "production")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn unknown_track_404_is_an_empty_list() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("GET"))
            .and(path(
                "/androidpublisher/v3/applications/com.example.app/tracks/my%20track/releases",
            ))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": { "code": 404, "message": "Track not found.", "status": "NOT_FOUND" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let logs = crate::logging::testing::capture_logs();
        let summaries = client(&server)
            .fetch_release_summaries(PACKAGE, "my track")
            .await
            .unwrap();
        assert!(summaries.is_empty());
        assert_eq!(
            logs.messages(crate::logging::LogLevel::Info, "googleplay.tracks"),
            [
                "release summaries of com.example.app track my track: Google answered 404, read \
              as an unknown track; returning no releases"
            ]
        );
    }

    #[test]
    fn describe_summaries_lists_name_state_and_codes() {
        assert_eq!(describe_summaries(&[]), "no releases");
        assert_eq!(
            describe_summaries(&[
                ReleaseSummaryInfo {
                    track: "production".into(),
                    name: Some("1.2".into()),
                    lifecycle_state: Some("IN_REVIEW".into()),
                    version_codes: vec!["1002".into()],
                },
                ReleaseSummaryInfo {
                    track: "production".into(),
                    name: None,
                    lifecycle_state: None,
                    version_codes: vec!["1000".into(), "1001".into()],
                },
            ]),
            "2 release(s): \"1.2\" IN_REVIEW [1002]; ? [1000, 1001]"
        );
    }

    #[tokio::test]
    async fn form_factor_track_keeps_its_colon() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("GET"))
            .and(path(
                "/androidpublisher/v3/applications/com.example.app/tracks/wear:production/releases",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        client(&server)
            .fetch_release_summaries(PACKAGE, "wear:production")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn other_errors_are_kept() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": { "code": 500, "message": "boom" }
            })))
            .mount(&server)
            .await;

        match client(&server)
            .fetch_release_summaries(PACKAGE, "production")
            .await
            .unwrap_err()
        {
            StackError::Http { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(
                    message,
                    "boom (HTTP 500 on GET applications/com.example.app/tracks/production/releases)"
                );
            }
            other => panic!("expected Http 500, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn forbidden_is_auth_naming_the_app() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": { "code": 403, "message": "The caller does not have permission" }
            })))
            .mount(&server)
            .await;

        match client(&server)
            .fetch_release_summaries(PACKAGE, "production")
            .await
            .unwrap_err()
        {
            StackError::Auth { message } => {
                assert!(message.contains("cannot access the app com.example.app"));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn quota_403_is_http_429_not_auth() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        // What a real account answered once the listing quota ran out.
        list()
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("Retry-After", "60")
                    .set_body_json(serde_json::json!({
                        "error": {
                            "code": 403,
                            "message": "Listing releases quota exceeded.",
                            "status": "PERMISSION_DENIED"
                        }
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let logs = crate::logging::testing::capture_logs();
        match client(&server)
            .fetch_release_summaries(PACKAGE, "production")
            .await
            .unwrap_err()
        {
            StackError::Http { status, message } => {
                assert_eq!(status, 429);
                assert_eq!(
                    message,
                    "Listing releases quota exceeded. [PERMISSION_DENIED]; this is a Google \
                     Play API quota/rate limit, not a permission problem — wait and retry \
                     later (Retry-After: 60 s) (HTTP 403 on GET \
                     applications/com.example.app/tracks/production/releases)"
                );
            }
            other => panic!("expected Http 429, got {other:?}"),
        }
        let warnings = logs.messages(crate::logging::LogLevel::Warn, "googleplay");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("quota: Google answered HTTP 403"));
    }

    #[tokio::test]
    async fn malformed_body_is_decode() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"releases": 3}"#))
            .mount(&server)
            .await;

        match client(&server)
            .fetch_release_summaries(PACKAGE, "production")
            .await
            .unwrap_err()
        {
            StackError::Decode { message } => {
                assert!(message.contains("tracks.releases.list response"))
            }
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_package_or_track_sends_nothing() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 0).await;
        let client = client(&server);

        // An invalid package stays a local 404, not an empty list.
        let err = client
            .fetch_release_summaries("com", "production")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
        for track in ["", ".", ".."] {
            let err = client
                .fetch_release_summaries(PACKAGE, track)
                .await
                .unwrap_err();
            assert!(matches!(err, StackError::Decode { .. }), "{track:?}");
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
