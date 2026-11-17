//! Track writes: one [`ReleaseChange`] applied and published in one edit.
//!
//! Flow (decision R2): insert an edit → `edits.tracks.get` the target track
//! (and, for a promotion, the source track) fresh → apply the typed change
//! (`release_change`) → `edits.tracks.update` → `edits.validate` →
//! `edits.commit`. Any failure after the insert deletes the edit (best effort)
//! and returns the original error, so nothing is published half-way. Reading
//! and writing inside the same edit closes the window in which a cached track
//! could overwrite a release someone else just published.
//!
//! Like every edit, opening one invalidates any edit the same service account
//! has open for the app elsewhere (e.g. a CI upload). Writes hold the client's
//! edit lock, so they never overlap each other or an edit-based read.

use serde::Deserialize;
use serde_json::{Map, Value};

use super::client::GooglePlayClient;
use super::edits::{PlayTrack, EDITS};
use super::error::{
    ErrorHints, GoogleApi, Target, PERMISSION_RELEASE_PRODUCTION, PERMISSION_RELEASE_TESTING,
};
use super::release_change::{self, Release};
use crate::domain::{
    ChangesInReviewBehavior, CommitOptions, NewReleaseStatus, ReleaseChange, TrackInfo,
    TrackReleaseInfo,
};
use crate::error::StackError;
use crate::logging::{self, target};

/// The `tracks` collection inside an edit.
const TRACKS: &str = "tracks";
const KEY_TRACK: &str = "track";
const KEY_RELEASES: &str = "releases";

/// `edits.commit` answers the committed `AppEdit`; only its shape is checked.
#[derive(Deserialize)]
struct CommittedEdit {}

impl ChangesInReviewBehavior {
    /// The `changesInReviewBehavior` query value.
    fn as_query(self) -> &'static str {
        match self {
            Self::CancelInReviewAndSubmit => "CANCEL_IN_REVIEW_AND_SUBMIT",
            Self::ErrorIfInReview => "ERROR_IF_IN_REVIEW",
        }
    }
}

/// The permission a change to `track` needs: production (including form-factor
/// production tracks such as `wear:production`) or a testing track.
fn release_permission(track: &str) -> &'static str {
    if track == "production" || track.ends_with(":production") {
        PERMISSION_RELEASE_PRODUCTION
    } else {
        PERMISSION_RELEASE_TESTING
    }
}

/// Parses a `Track` body as a raw JSON object.
fn parse_track(body: &str, what: &str) -> Result<Map<String, Value>, StackError> {
    serde_json::from_str::<Map<String, Value>>(body)
        .map_err(|e| StackError::decode(format!("{what} response: {e}")))
}

/// The `releases[]` of a raw track; absent (proto3 JSON omits an empty list)
/// means none.
fn releases_of(track: &Map<String, Value>) -> Result<Vec<Release>, StackError> {
    match track.get(KEY_RELEASES) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Object(release) => Ok(release.clone()),
                _ => Err(StackError::decode(
                    "edits.tracks.get response: a release is not an object",
                )),
            })
            .collect(),
        Some(_) => Err(StackError::decode(
            "edits.tracks.get response: releases is not a list",
        )),
    }
}

/// `[1001, 1002]`.
fn codes(version_codes: &[String]) -> String {
    format!("[{}]", version_codes.join(", "))
}

fn describe_status(status: &NewReleaseStatus) -> String {
    match status {
        NewReleaseStatus::Draft => "draft".to_string(),
        NewReleaseStatus::InProgress { user_fraction } => {
            format!("inProgress at {user_fraction}")
        }
        NewReleaseStatus::Completed => "completed".to_string(),
    }
}

/// A one-line summary of a release change for the log: its kind, the version
/// codes it targets and the values it sets. Release notes are counted, never
/// quoted (they are user content).
fn describe_change(change: &ReleaseChange) -> String {
    match change {
        ReleaseChange::SetRollout {
            version_codes,
            user_fraction,
        } => format!("SetRollout {} to {user_fraction}", codes(version_codes)),
        ReleaseChange::Halt { version_codes } => format!("Halt {}", codes(version_codes)),
        ReleaseChange::Resume { version_codes } => format!("Resume {}", codes(version_codes)),
        ReleaseChange::Complete { version_codes } => {
            format!("Complete {}", codes(version_codes))
        }
        ReleaseChange::UpdateMetadata {
            version_codes,
            update,
        } => {
            let mut fields = Vec::new();
            if update.name.is_some() {
                fields.push("name".to_string());
            }
            if let Some(notes) = &update.release_notes {
                fields.push(format!("release notes ({} languages)", notes.len()));
            }
            if let Some(priority) = update.in_app_update_priority {
                fields.push(format!("priority {priority}"));
            }
            if update.country_targeting.is_some() {
                fields.push("country targeting".to_string());
            }
            let fields = if fields.is_empty() {
                "nothing".to_string()
            } else {
                fields.join(", ")
            };
            format!("UpdateMetadata {} ({fields})", codes(version_codes))
        }
        ReleaseChange::CreateRelease { release } => format!(
            "CreateRelease {} as {} ({} release-note languages)",
            codes(&release.version_codes),
            describe_status(&release.status),
            release.release_notes.len()
        ),
        ReleaseChange::Promote {
            from_track,
            version_codes,
            status,
            ..
        } => format!(
            "Promote {} from {from_track} as {}",
            codes(version_codes),
            describe_status(status)
        ),
    }
}

/// `completed [1001]; inProgress [1002] at 0.1`, or `no releases`.
fn describe_releases(releases: &[TrackReleaseInfo]) -> String {
    if releases.is_empty() {
        return "no releases".to_string();
    }
    releases
        .iter()
        .map(|release| {
            let status = release.status.as_deref().unwrap_or("?");
            let mut line = format!("{status} {}", codes(&release.version_codes));
            if let Some(fraction) = release.user_fraction {
                line.push_str(&format!(" at {fraction}"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("; ")
}

impl GooglePlayClient {
    /// Applies `change` to `track` of `package_name` and publishes it, in one
    /// edit (see the module docs), returning the track as Google stored it.
    ///
    /// Invalid values are rejected before anything is sent (see
    /// `release_change::validate`). `options` maps onto `edits.commit`:
    /// `send_for_review == false` sends `changesNotSentForReview=true`, and
    /// `changes_in_review_behavior` is sent when set.
    ///
    /// # Errors
    /// - [`StackError::Http`] `400` for a locally invalid change, or carrying
    ///   Google's message when `edits.validate` / `edits.commit` refuse it.
    /// - [`StackError::ReleaseNotFound`] when the targeted release is no longer
    ///   on its track (nothing is written; the edit is deleted).
    /// - [`StackError::Auth`] when Google denies access (the message names the
    ///   release permission the track needs) or the API is disabled.
    /// - [`StackError::Decode`] on a malformed body or an unusable track name,
    ///   [`StackError::Network`] on transport failure.
    pub(crate) async fn apply_release_change(
        &self,
        package_name: &str,
        track: &str,
        change: &ReleaseChange,
        options: CommitOptions,
    ) -> Result<TrackInfo, StackError> {
        release_change::validate(change, track)?;
        logging::info(target::PLAY_TRACKS, || {
            format!(
                "release change on {package_name} track {track}: {}",
                describe_change(change)
            )
        });
        let permission = release_permission(track);
        let hints = ErrorHints {
            api: GoogleApi::Publisher,
            permission,
            target: Target::AppResource(package_name),
        };
        // Build every URL that does not depend on the edit id up front, so a
        // bad track name fails before an edit is opened.
        self.publisher_url(package_name, &[EDITS, "-", TRACKS, track])?;
        if let ReleaseChange::Promote { from_track, .. } = change {
            self.publisher_url(package_name, &[EDITS, "-", TRACKS, from_track])?;
        }

        self.with_committed_edit(package_name, permission, |edit_id| async move {
            let mut target = self
                .get_track(package_name, &edit_id, track, &hints)
                .await?;
            let source = match change {
                ReleaseChange::Promote { from_track, .. } => {
                    let source = self
                        .get_track(package_name, &edit_id, from_track, &hints)
                        .await?;
                    Some((from_track.as_str(), releases_of(&source)?))
                }
                _ => None,
            };

            let releases = release_change::apply(
                change,
                track,
                releases_of(&target)?,
                source
                    .as_ref()
                    .map(|(name, releases)| (*name, releases.as_slice())),
            )?;
            target.insert(KEY_TRACK.to_string(), Value::String(track.to_string()));
            target.insert(
                KEY_RELEASES.to_string(),
                Value::Array(releases.into_iter().map(Value::Object).collect()),
            );

            let stored = self
                .update_track(package_name, &edit_id, track, &target, &hints)
                .await?;
            logging::debug(target::PLAY_TRACKS, || {
                format!("edit {edit_id}: track {track} updated; validating")
            });
            self.post_edit_action(package_name, &edit_id, "validate", &[], &hints)
                .await?;
            self.commit_edit(package_name, &edit_id, options, &hints)
                .await?;
            logging::info(target::PLAY_TRACKS, || {
                format!(
                    "release change on {package_name} track {track} committed in edit \
                     {edit_id}; track now: {}",
                    describe_releases(&stored.releases)
                )
            });
            Ok(stored)
        })
        .await
    }

    /// `GET …/edits/{edit_id}/tracks/{track}` as a raw JSON object.
    async fn get_track(
        &self,
        package_name: &str,
        edit_id: &str,
        track: &str,
        hints: &ErrorHints<'_>,
    ) -> Result<Map<String, Value>, StackError> {
        let url = self.publisher_url(package_name, &[EDITS, edit_id, TRACKS, track])?;
        let body = self.execute(|| self.http().get(url.clone()), hints).await?;
        parse_track(&body, "edits.tracks.get")
    }

    /// `PUT …/edits/{edit_id}/tracks/{track}` with the whole `track` object,
    /// returning the track Google stored (falling back to the requested name).
    async fn update_track(
        &self,
        package_name: &str,
        edit_id: &str,
        track: &str,
        body: &Map<String, Value>,
        hints: &ErrorHints<'_>,
    ) -> Result<TrackInfo, StackError> {
        let url = self.publisher_url(package_name, &[EDITS, edit_id, TRACKS, track])?;
        let response = self
            .execute(|| self.http().put(url.clone()).json(body), hints)
            .await?;
        let stored: PlayTrack = serde_json::from_str(&response)
            .map_err(|e| StackError::decode(format!("edits.tracks.update response: {e}")))?;
        Ok(stored.into_info().unwrap_or_else(|| TrackInfo {
            track: track.to_string(),
            releases: Vec::new(),
        }))
    }

    /// `POST …/edits/{edit_id}:{action}` with `query` and an empty body,
    /// returning the 2xx body.
    async fn post_edit_action(
        &self,
        package_name: &str,
        edit_id: &str,
        action: &str,
        query: &[(&str, &str)],
        hints: &ErrorHints<'_>,
    ) -> Result<String, StackError> {
        let segment = format!("{edit_id}:{action}");
        let url = self.publisher_url(package_name, &[EDITS, &segment])?;
        self.execute(
            || {
                self.http()
                    .post(url.clone())
                    .query(query)
                    // Google's front end answers 411 to a POST without a length.
                    .header(reqwest::header::CONTENT_LENGTH, "0")
            },
            hints,
        )
        .await
    }

    /// `POST …/edits/{edit_id}:commit` with the query `options` map onto.
    async fn commit_edit(
        &self,
        package_name: &str,
        edit_id: &str,
        options: CommitOptions,
        hints: &ErrorHints<'_>,
    ) -> Result<(), StackError> {
        let mut query = Vec::new();
        if !options.send_for_review {
            query.push(("changesNotSentForReview", "true"));
        }
        if let Some(behavior) = options.changes_in_review_behavior {
            query.push(("changesInReviewBehavior", behavior.as_query()));
        }
        logging::info(target::PLAY_EDITS, || {
            let options = if query.is_empty() {
                "default options".to_string()
            } else {
                query
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!("committing edit {edit_id} of {package_name} ({options})")
        });
        let body = self
            .post_edit_action(package_name, edit_id, "commit", &query, hints)
            .await?;
        serde_json::from_str::<CommittedEdit>(&body)
            .map(|_| ())
            .map_err(|e| StackError::decode(format!("edits.commit response: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        LocalizedTextInfo, NewRelease, NewReleaseStatus, ReleaseMetadataUpdate, TrackReleaseInfo,
    };
    use crate::logging::testing::capture_logs;
    use crate::providers::googleplay::test_support::{client, mount_token, CapturingLogger};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use wiremock::matchers::{
        body_json, header, method, path, query_param, query_param_is_missing,
    };
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    const PACKAGE: &str = "com.example.app";
    const EDITS_PATH: &str = "/androidpublisher/v3/applications/com.example.app/edits";

    const SEND: CommitOptions = CommitOptions {
        send_for_review: true,
        changes_in_review_behavior: None,
    };

    fn codes(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn insert() -> wiremock::MockBuilder {
        Mock::given(method("POST")).and(path(EDITS_PATH))
    }

    fn track_get(edit: &str, track: &str) -> wiremock::MockBuilder {
        Mock::given(method("GET")).and(path(format!("{EDITS_PATH}/{edit}/tracks/{track}")))
    }

    fn track_put(edit: &str, track: &str) -> wiremock::MockBuilder {
        Mock::given(method("PUT")).and(path(format!("{EDITS_PATH}/{edit}/tracks/{track}")))
    }

    fn action(edit: &str, action: &str) -> wiremock::MockBuilder {
        Mock::given(method("POST")).and(path(format!("{EDITS_PATH}/{edit}:{action}")))
    }

    fn delete(edit: &str) -> wiremock::MockBuilder {
        Mock::given(method("DELETE")).and(path(format!("{EDITS_PATH}/{edit}")))
    }

    fn ok_json(value: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(value)
    }

    fn google_error(status: u16, message: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(serde_json::json!({
            "error": { "code": status, "message": message }
        }))
    }

    /// Echoes the PUT body back, as Google does with the stored track.
    struct Echo;

    impl Respond for Echo {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            ResponseTemplate::new(200).set_body_bytes(request.body.clone())
        }
    }

    /// `(method, path?query)` of every request after the token exchange.
    async fn api_calls(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() != "/token")
            .map(|r| {
                let suffix = r
                    .url
                    .path()
                    .strip_prefix(EDITS_PATH)
                    .unwrap_or(r.url.path());
                match r.url.query() {
                    Some(q) => format!("{} {suffix}?{q}", r.method),
                    None => format!("{} {suffix}", r.method),
                }
            })
            .collect()
    }

    /// The production track: 42 rolling out at 10 %, 40 completed.
    fn production() -> Value {
        serde_json::json!({
            "track": "production",
            "releases": [
                { "versionCodes": ["42"], "status": "inProgress", "userFraction": 0.1,
                  "name": "4.2", "unmodeled": "kept" },
                { "versionCodes": ["40"], "status": "completed", "name": "4.0" }
            ]
        })
    }

    /// Mounts insert `E1` + GET production; every later call is the caller's.
    async fn mount_read(server: &MockServer, track: Value) {
        insert()
            .respond_with(ok_json(serde_json::json!({ "id": "E1" })))
            .expect(1)
            .mount(server)
            .await;
        track_get("E1", "production")
            .respond_with(ok_json(track))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_validate_and_commit(server: &MockServer) {
        action("E1", "validate")
            .respond_with(ok_json(serde_json::json!({ "id": "E1" })))
            .expect(1)
            .mount(server)
            .await;
        action("E1", "commit")
            .respond_with(ok_json(serde_json::json!({ "id": "E1" })))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn set_rollout_reads_updates_validates_and_commits_in_one_edit() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_read(&server, production()).await;
        track_put("E1", "production")
            .and(body_json(serde_json::json!({
                "track": "production",
                "releases": [
                    // Only the fraction moved; the unmodeled field survived.
                    { "versionCodes": ["42"], "status": "inProgress", "userFraction": 0.5,
                      "name": "4.2", "unmodeled": "kept" },
                    { "versionCodes": ["40"], "status": "completed", "name": "4.0" }
                ]
            })))
            .respond_with(Echo)
            .expect(1)
            .mount(&server)
            .await;
        mount_validate_and_commit(&server).await;
        // A committed edit is gone: no delete.
        delete("E1")
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let track = client(&server)
            .apply_release_change(
                PACKAGE,
                "production",
                &ReleaseChange::SetRollout {
                    version_codes: codes(&["42"]),
                    user_fraction: 0.5,
                },
                SEND,
            )
            .await
            .unwrap();

        assert_eq!(track.track, "production");
        assert_eq!(
            track.releases[0],
            TrackReleaseInfo {
                name: Some("4.2".into()),
                status: Some("inProgress".into()),
                version_codes: codes(&["42"]),
                user_fraction: Some(0.5),
                release_notes: vec![],
                in_app_update_priority: None,
                country_targeting: None,
            }
        );
        assert_eq!(
            api_calls(&server).await,
            vec![
                "POST ",
                "GET /E1/tracks/production",
                "PUT /E1/tracks/production",
                "POST /E1:validate",
                "POST /E1:commit",
            ]
        );
    }

    #[tokio::test]
    async fn validate_and_commit_send_an_empty_body_with_a_length() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_read(&server, production()).await;
        track_put("E1", "production")
            .respond_with(Echo)
            .mount(&server)
            .await;
        for name in ["validate", "commit"] {
            action("E1", name)
                .and(header("content-length", "0"))
                .respond_with(ok_json(serde_json::json!({ "id": "E1" })))
                .expect(1)
                .mount(&server)
                .await;
        }

        client(&server)
            .apply_release_change(
                PACKAGE,
                "production",
                &ReleaseChange::Halt {
                    version_codes: codes(&["42"]),
                },
                SEND,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn commit_query_follows_the_options() {
        for (options, not_sent, behavior) in [
            (SEND, None, None),
            (
                CommitOptions {
                    send_for_review: false,
                    changes_in_review_behavior: Some(ChangesInReviewBehavior::ErrorIfInReview),
                },
                Some("true"),
                Some("ERROR_IF_IN_REVIEW"),
            ),
            (
                CommitOptions {
                    send_for_review: true,
                    changes_in_review_behavior: Some(
                        ChangesInReviewBehavior::CancelInReviewAndSubmit,
                    ),
                },
                None,
                Some("CANCEL_IN_REVIEW_AND_SUBMIT"),
            ),
        ] {
            let server = MockServer::start().await;
            mount_token(&server, "tok-1", 1).await;
            mount_read(&server, production()).await;
            track_put("E1", "production")
                .respond_with(Echo)
                .mount(&server)
                .await;
            action("E1", "validate")
                .respond_with(ok_json(serde_json::json!({})))
                .mount(&server)
                .await;
            let commit = action("E1", "commit");
            let commit = match not_sent {
                Some(value) => commit.and(query_param("changesNotSentForReview", value)),
                None => commit.and(query_param_is_missing("changesNotSentForReview")),
            };
            let commit = match behavior {
                Some(value) => commit.and(query_param("changesInReviewBehavior", value)),
                None => commit.and(query_param_is_missing("changesInReviewBehavior")),
            };
            commit
                .respond_with(ok_json(serde_json::json!({ "id": "E1" })))
                .expect(1)
                .mount(&server)
                .await;

            client(&server)
                .apply_release_change(
                    PACKAGE,
                    "production",
                    &ReleaseChange::Complete {
                        version_codes: codes(&["42"]),
                    },
                    options,
                )
                .await
                .unwrap();
        }
    }

    /// `Level [target] message` of every Google Play line (no HTTP/auth).
    fn play_lines(logs: &crate::logging::testing::Capture) -> Vec<String> {
        logs.records()
            .into_iter()
            .filter(|r| r.target.starts_with("googleplay."))
            .map(|r| format!("{:?} [{}] {}", r.level, r.target, r.message))
            .collect()
    }

    #[tokio::test]
    async fn host_log_follows_a_committed_release_change() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_read(&server, production()).await;
        track_put("E1", "production")
            .respond_with(Echo)
            .mount(&server)
            .await;
        mount_validate_and_commit(&server).await;

        let logs = capture_logs();
        client(&server)
            .apply_release_change(
                PACKAGE,
                "production",
                &ReleaseChange::SetRollout {
                    version_codes: codes(&["42"]),
                    user_fraction: 0.5,
                },
                CommitOptions {
                    send_for_review: false,
                    changes_in_review_behavior: Some(ChangesInReviewBehavior::ErrorIfInReview),
                },
            )
            .await
            .unwrap();

        assert_eq!(
            play_lines(&logs),
            [
                "Info [googleplay.tracks] release change on com.example.app track production: \
                 SetRollout [42] to 0.5",
                "Info [googleplay.edits] edit E1 opened on com.example.app (write)",
                "Debug [googleplay.tracks] edit E1: track production updated; validating",
                "Info [googleplay.edits] committing edit E1 of com.example.app \
                 (changesNotSentForReview=true, changesInReviewBehavior=ERROR_IF_IN_REVIEW)",
                "Info [googleplay.tracks] release change on com.example.app track production \
                 committed in edit E1; track now: inProgress [42] at 0.5; completed [40]",
            ]
        );
    }

    #[tokio::test]
    async fn host_log_shows_a_refused_release_change_and_the_deleted_edit() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_read(&server, production()).await;
        track_put("E1", "production")
            .respond_with(Echo)
            .mount(&server)
            .await;
        action("E1", "validate")
            .respond_with(google_error(400, "Release notes are too long."))
            .mount(&server)
            .await;
        delete("E1")
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let logs = capture_logs();
        client(&server)
            .apply_release_change(
                PACKAGE,
                "production",
                &ReleaseChange::Halt {
                    version_codes: codes(&["42"]),
                },
                SEND,
            )
            .await
            .unwrap_err();

        assert_eq!(
            play_lines(&logs),
            [
                "Info [googleplay.tracks] release change on com.example.app track production: \
                 Halt [42]",
                "Info [googleplay.edits] edit E1 opened on com.example.app (write)",
                "Debug [googleplay.tracks] edit E1: track production updated; validating",
                "Warn [googleplay.edits] edit E1 of com.example.app failed (write): HTTP 400: \
                 Release notes are too long. (HTTP 400 on POST \
                 applications/com.example.app/edits/E1:validate); deleting it, nothing is \
                 committed",
                "Info [googleplay.edits] edit E1 of com.example.app deleted",
            ]
        );
    }

    #[test]
    fn describe_change_names_the_kind_codes_and_values_but_never_note_text() {
        let note = LocalizedTextInfo {
            language: "en-US".into(),
            text: "secret launch notes".into(),
        };
        let cases = [
            (
                ReleaseChange::SetRollout {
                    version_codes: codes(&["1", "2"]),
                    user_fraction: 0.25,
                },
                "SetRollout [1, 2] to 0.25",
            ),
            (
                ReleaseChange::Halt {
                    version_codes: codes(&["3"]),
                },
                "Halt [3]",
            ),
            (
                ReleaseChange::Resume {
                    version_codes: codes(&["3"]),
                },
                "Resume [3]",
            ),
            (
                ReleaseChange::Complete {
                    version_codes: codes(&["3"]),
                },
                "Complete [3]",
            ),
            (
                ReleaseChange::UpdateMetadata {
                    version_codes: codes(&["3"]),
                    update: ReleaseMetadataUpdate {
                        name: Some("n".into()),
                        release_notes: Some(vec![note.clone()]),
                        in_app_update_priority: Some(4),
                        country_targeting: None,
                    },
                },
                "UpdateMetadata [3] (name, release notes (1 languages), priority 4)",
            ),
            (
                ReleaseChange::UpdateMetadata {
                    version_codes: codes(&["3"]),
                    update: ReleaseMetadataUpdate::default(),
                },
                "UpdateMetadata [3] (nothing)",
            ),
            (
                ReleaseChange::CreateRelease {
                    release: NewRelease {
                        version_codes: codes(&["5"]),
                        name: None,
                        release_notes: vec![note.clone()],
                        status: NewReleaseStatus::InProgress { user_fraction: 0.1 },
                        in_app_update_priority: None,
                        country_targeting: None,
                    },
                },
                "CreateRelease [5] as inProgress at 0.1 (1 release-note languages)",
            ),
            (
                ReleaseChange::Promote {
                    from_track: "beta".into(),
                    version_codes: codes(&["5"]),
                    status: NewReleaseStatus::Completed,
                    name: None,
                    release_notes: Some(vec![note]),
                    in_app_update_priority: None,
                    country_targeting: None,
                },
                "Promote [5] from beta as completed",
            ),
        ];
        for (change, expected) in cases {
            let described = describe_change(&change);
            assert_eq!(described, expected);
            assert!(!described.contains("secret"));
        }
        assert_eq!(
            describe_status(&NewReleaseStatus::Draft),
            "draft".to_string()
        );
        assert_eq!(describe_releases(&[]), "no releases");
    }

    #[tokio::test]
    async fn failed_validate_never_commits_and_deletes_the_edit() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_read(&server, production()).await;
        track_put("E1", "production")
            .respond_with(Echo)
            .expect(1)
            .mount(&server)
            .await;
        action("E1", "validate")
            .respond_with(google_error(
                400,
                "Release notes for language en-US are too long.",
            ))
            .expect(1)
            .mount(&server)
            .await;
        action("E1", "commit")
            .respond_with(ok_json(serde_json::json!({})))
            .expect(0)
            .mount(&server)
            .await;
        delete("E1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server)
            .apply_release_change(
                PACKAGE,
                "production",
                &ReleaseChange::Halt {
                    version_codes: codes(&["42"]),
                },
                SEND,
            )
            .await
            .unwrap_err()
        {
            StackError::Http { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(
                    message,
                    "Release notes for language en-US are too long. \
                     (HTTP 400 on POST applications/com.example.app/edits/E1:validate)"
                );
            }
            other => panic!("expected Google's Http 400, got {other:?}"),
        }
        assert_eq!(
            api_calls(&server).await,
            vec![
                "POST ",
                "GET /E1/tracks/production",
                "PUT /E1/tracks/production",
                "POST /E1:validate",
                "DELETE /E1",
            ]
        );
    }

    #[tokio::test]
    async fn failed_commit_deletes_the_edit_and_its_error_wins_over_the_delete() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_read(&server, production()).await;
        track_put("E1", "production")
            .respond_with(Echo)
            .mount(&server)
            .await;
        action("E1", "validate")
            .respond_with(ok_json(serde_json::json!({})))
            .mount(&server)
            .await;
        action("E1", "commit")
            .respond_with(google_error(
                400,
                "Changes cannot be sent for review automatically. Please set the query \
                 parameter changesNotSentForReview to true.",
            ))
            .expect(1)
            .mount(&server)
            .await;
        delete("E1")
            .respond_with(google_error(503, "delete exploded"))
            .expect(1)
            .mount(&server)
            .await;

        let logger = Arc::new(CapturingLogger::default());
        let err = client(&server)
            .with_debug_logger(Some(logger.clone()))
            .apply_release_change(
                PACKAGE,
                "production",
                &ReleaseChange::Halt {
                    version_codes: codes(&["42"]),
                },
                SEND,
            )
            .await
            .unwrap_err();
        match err {
            StackError::Http { status, message } => {
                assert_eq!(status, 400);
                assert!(message.contains("changesNotSentForReview to true"));
            }
            other => panic!("expected the commit's Http 400, got {other:?}"),
        }
        assert!(logger
            .messages()
            .last()
            .unwrap()
            .contains("could not delete edit E1"));
    }

    #[tokio::test]
    async fn failed_update_skips_validate_and_commit() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_read(&server, production()).await;
        track_put("E1", "production")
            .respond_with(google_error(403, "The caller does not have permission"))
            .expect(1)
            .mount(&server)
            .await;
        delete("E1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server)
            .apply_release_change(
                PACKAGE,
                "production",
                &ReleaseChange::Halt {
                    version_codes: codes(&["42"]),
                },
                SEND,
            )
            .await
            .unwrap_err()
        {
            StackError::Auth { message } => assert!(message.contains(
                "\"Release to production, exclude devices, and use app signing by Google Play\""
            )),
            other => panic!("expected Auth, got {other:?}"),
        }
        assert_eq!(
            api_calls(&server).await,
            vec![
                "POST ",
                "GET /E1/tracks/production",
                "PUT /E1/tracks/production",
                "DELETE /E1",
            ]
        );
    }

    #[tokio::test]
    async fn missing_release_is_typed_writes_nothing_and_deletes_the_edit() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_read(&server, production()).await;
        delete("E1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let err = client(&server)
            .apply_release_change(
                PACKAGE,
                "production",
                &ReleaseChange::Resume {
                    version_codes: codes(&["41"]),
                },
                SEND,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::ReleaseNotFound { .. }), "{err:?}");
        assert_eq!(
            api_calls(&server).await,
            vec!["POST ", "GET /E1/tracks/production", "DELETE /E1"]
        );
    }

    #[tokio::test]
    async fn invalid_changes_and_names_open_no_edit() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 0).await;
        let client = client(&server);
        let cases: Vec<(&str, ReleaseChange)> = vec![
            (
                "production",
                ReleaseChange::SetRollout {
                    version_codes: codes(&["42"]),
                    user_fraction: 1.0,
                },
            ),
            (
                "production",
                ReleaseChange::SetRollout {
                    version_codes: codes(&["42"]),
                    user_fraction: 0.0,
                },
            ),
            (
                "beta",
                ReleaseChange::CreateRelease {
                    release: NewRelease {
                        version_codes: vec![],
                        name: None,
                        release_notes: vec![],
                        status: NewReleaseStatus::Draft,
                        in_app_update_priority: None,
                        country_targeting: None,
                    },
                },
            ),
            (
                "beta",
                ReleaseChange::UpdateMetadata {
                    version_codes: codes(&["1"]),
                    update: ReleaseMetadataUpdate {
                        in_app_update_priority: Some(9),
                        ..ReleaseMetadataUpdate::default()
                    },
                },
            ),
        ];
        for (track, change) in cases {
            let err = client
                .apply_release_change(PACKAGE, track, &change, SEND)
                .await
                .unwrap_err();
            assert!(
                matches!(err, StackError::Http { status: 400, .. }),
                "{change:?}: {err:?}"
            );
        }

        let halt = ReleaseChange::Halt {
            version_codes: codes(&["1"]),
        };
        let err = client
            .apply_release_change("not-a-package", "beta", &halt, SEND)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
        let err = client
            .apply_release_change(PACKAGE, "..", &halt, SEND)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Decode { .. }));
        let promote = ReleaseChange::Promote {
            from_track: ".".into(),
            version_codes: codes(&["1"]),
            status: NewReleaseStatus::Draft,
            name: None,
            release_notes: None,
            in_app_update_priority: None,
            country_targeting: None,
        };
        let err = client
            .apply_release_change(PACKAGE, "beta", &promote, SEND)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Decode { .. }));

        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn promote_reads_both_tracks_in_the_same_edit() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        insert()
            .respond_with(ok_json(serde_json::json!({ "id": "E1" })))
            .mount(&server)
            .await;
        // The target track has no release yet: Google omits `releases`.
        track_get("E1", "beta")
            .respond_with(ok_json(serde_json::json!({ "track": "beta" })))
            .expect(1)
            .mount(&server)
            .await;
        track_get("E1", "internal")
            .respond_with(ok_json(serde_json::json!({
                "track": "internal",
                "releases": [{
                    "versionCodes": ["50"], "status": "completed", "name": "5.0",
                    "releaseNotes": [{ "language": "en-US", "text": "Hi" }]
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        track_put("E1", "beta")
            .and(body_json(serde_json::json!({
                "track": "beta",
                "releases": [{
                    "versionCodes": ["50"], "name": "5.0",
                    "releaseNotes": [{ "language": "en-US", "text": "Hi" }],
                    "status": "completed"
                }]
            })))
            .respond_with(Echo)
            .expect(1)
            .mount(&server)
            .await;
        mount_validate_and_commit(&server).await;

        let track = client(&server)
            .apply_release_change(
                PACKAGE,
                "beta",
                &ReleaseChange::Promote {
                    from_track: "internal".into(),
                    version_codes: codes(&["50"]),
                    status: NewReleaseStatus::Completed,
                    name: None,
                    release_notes: None,
                    in_app_update_priority: None,
                    country_targeting: None,
                },
                SEND,
            )
            .await
            .unwrap();
        assert_eq!(track.releases.len(), 1);
        assert_eq!(
            track.releases[0].release_notes,
            vec![LocalizedTextInfo {
                language: "en-US".into(),
                text: "Hi".into()
            }]
        );
        assert_eq!(
            api_calls(&server).await,
            vec![
                "POST ",
                "GET /E1/tracks/beta",
                "GET /E1/tracks/internal",
                "PUT /E1/tracks/beta",
                "POST /E1:validate",
                "POST /E1:commit",
            ]
        );
    }

    #[tokio::test]
    async fn create_release_on_a_testing_track_names_the_testing_permission() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        insert()
            .respond_with(google_error(403, "The caller does not have permission"))
            .expect(1)
            .mount(&server)
            .await;

        let change = ReleaseChange::CreateRelease {
            release: NewRelease {
                version_codes: codes(&["60"]),
                name: Some("6.0".into()),
                release_notes: vec![],
                status: NewReleaseStatus::Draft,
                in_app_update_priority: None,
                country_targeting: None,
            },
        };
        match client(&server)
            .apply_release_change(PACKAGE, "alpha", &change, SEND)
            .await
            .unwrap_err()
        {
            StackError::Auth { message } => {
                assert!(message.contains("\"Release to testing tracks\""))
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[test]
    fn form_factor_production_tracks_need_the_production_permission() {
        assert_eq!(
            release_permission("production"),
            PERMISSION_RELEASE_PRODUCTION
        );
        assert_eq!(
            release_permission("wear:production"),
            PERMISSION_RELEASE_PRODUCTION
        );
        for track in ["beta", "alpha", "internal", "qa-team", "wear:beta"] {
            assert_eq!(release_permission(track), PERMISSION_RELEASE_TESTING);
        }
    }

    #[tokio::test]
    async fn malformed_track_read_is_decode_and_deletes_the_edit() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_read(
            &server,
            serde_json::json!({ "track": "production", "releases": "nope" }),
        )
        .await;
        delete("E1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let err = client(&server)
            .apply_release_change(
                PACKAGE,
                "production",
                &ReleaseChange::Halt {
                    version_codes: codes(&["42"]),
                },
                SEND,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Decode { .. }), "{err:?}");
    }

    /// Hands out `E1`, `E2`, … so overlapping sessions would be visible.
    struct NumberedEdits {
        next: AtomicUsize,
    }

    impl Respond for NumberedEdits {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
            ok_json(serde_json::json!({ "id": format!("E{n}") }))
        }
    }

    #[tokio::test]
    async fn concurrent_changes_on_one_app_never_overlap() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        insert()
            .respond_with(NumberedEdits {
                next: AtomicUsize::new(0),
            })
            .expect(2)
            .mount(&server)
            .await;
        for edit in ["E1", "E2"] {
            // A slow read gives the second write every chance to interleave.
            track_get(edit, "production")
                .respond_with(ok_json(production()).set_delay(Duration::from_millis(150)))
                .expect(1)
                .mount(&server)
                .await;
            track_put(edit, "production")
                .respond_with(Echo)
                .expect(1)
                .mount(&server)
                .await;
            for name in ["validate", "commit"] {
                action(edit, name)
                    .respond_with(ok_json(serde_json::json!({})))
                    .expect(1)
                    .mount(&server)
                    .await;
            }
        }

        let client = client(&server);
        let halt = ReleaseChange::Halt {
            version_codes: codes(&["42"]),
        };
        let rollout = ReleaseChange::SetRollout {
            version_codes: codes(&["42"]),
            user_fraction: 0.3,
        };
        let (first, second) = tokio::join!(
            client.apply_release_change(PACKAGE, "production", &halt, SEND),
            client.apply_release_change(PACKAGE, "production", &rollout, SEND)
        );
        first.unwrap();
        second.unwrap();

        let calls = api_calls(&server).await;
        let session = |edit: &str| {
            vec![
                "POST ".to_string(),
                format!("GET /{edit}/tracks/production"),
                format!("PUT /{edit}/tracks/production"),
                format!("POST /{edit}:validate"),
                format!("POST /{edit}:commit"),
            ]
        };
        assert_eq!(calls, [session("E1"), session("E2")].concat());
    }
}
