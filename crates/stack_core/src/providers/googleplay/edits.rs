//! Reads that the Android Publisher API only serves inside an **edit**: app
//! details (`edits.details`), store listings (`edits.listings`) and release
//! tracks (`edits.tracks`).
//!
//! Each read runs in a throwaway edit session (insert, read, delete) and never
//! commits, so nothing changes on the store. It does have one side effect:
//! Google keeps a single open edit per service account and app, and inserting a
//! new edit invalidates the previous one. A read therefore breaks an edit the
//! same service account has open elsewhere (e.g. a CI upload in progress), and
//! sessions through one client are serialized by its edit lock so concurrent
//! reads cannot break each other.

use std::future::Future;

use serde::Deserialize;
use serde_json::json;

use super::client::GooglePlayClient;
use super::error::{ErrorHints, GoogleApi, Target, PERMISSION_VIEW_APP_INFO};
use super::non_blank;
use super::timestamp::Int64;
use crate::domain::{
    AppDetailsInfo, LocalizedTextInfo, StoreListingInfo, TrackInfo, TrackReleaseInfo,
};
use crate::error::StackError;

/// The `edits` collection under an app.
const EDITS: &str = "edits";

/// An `AppEdit` as `edits.insert` returns it (`expiryTimeSeconds` unused).
#[derive(Deserialize)]
struct AppEdit {
    #[serde(default)]
    id: Option<String>,
}

/// `edits.details` (`AppDetails`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayAppDetails {
    #[serde(default)]
    default_language: Option<String>,
    #[serde(default)]
    contact_email: Option<String>,
    #[serde(default)]
    contact_phone: Option<String>,
    #[serde(default)]
    contact_website: Option<String>,
}

/// `edits.listings.list`. Proto3 JSON omits an empty `listings`.
#[derive(Deserialize)]
struct ListingsListResponse {
    #[serde(default)]
    listings: Vec<PlayListing>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayListing {
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    short_description: Option<String>,
    #[serde(default)]
    full_description: Option<String>,
    #[serde(default)]
    video: Option<String>,
}

/// `edits.tracks.list`. Proto3 JSON omits an empty `tracks`.
#[derive(Deserialize)]
struct TracksListResponse {
    #[serde(default)]
    tracks: Vec<PlayTrack>,
}

#[derive(Deserialize)]
struct PlayTrack {
    #[serde(default)]
    track: Option<String>,
    #[serde(default)]
    releases: Vec<PlayRelease>,
}

/// A `TrackRelease` (`countryTargeting` is not mapped yet).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayRelease {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    /// int64 values, sent as strings.
    #[serde(default)]
    version_codes: Vec<Int64>,
    #[serde(default)]
    user_fraction: Option<f64>,
    #[serde(default)]
    release_notes: Vec<PlayLocalizedText>,
    #[serde(default)]
    in_app_update_priority: Option<i32>,
}

#[derive(Deserialize)]
struct PlayLocalizedText {
    #[serde(default)]
    language: String,
    #[serde(default)]
    text: String,
}

impl PlayAppDetails {
    fn into_info(self, app_id: &str) -> AppDetailsInfo {
        AppDetailsInfo {
            app_id: app_id.to_string(),
            default_language: non_blank(self.default_language),
            contact_email: non_blank(self.contact_email),
            contact_phone: non_blank(self.contact_phone),
            contact_website: non_blank(self.contact_website),
        }
    }
}

impl PlayListing {
    /// `None` without a language: it is what identifies a listing.
    fn into_info(self) -> Option<StoreListingInfo> {
        Some(StoreListingInfo {
            language: non_blank(self.language)?,
            title: non_blank(self.title),
            short_description: non_blank(self.short_description),
            full_description: non_blank(self.full_description),
            video: non_blank(self.video),
        })
    }
}

impl PlayTrack {
    /// `None` without a track name: it is what identifies a track.
    fn into_info(self) -> Option<TrackInfo> {
        Some(TrackInfo {
            track: non_blank(self.track)?,
            releases: self
                .releases
                .into_iter()
                .map(PlayRelease::into_info)
                .collect(),
        })
    }
}

impl PlayRelease {
    fn into_info(self) -> TrackReleaseInfo {
        TrackReleaseInfo {
            name: non_blank(self.name),
            status: non_blank(self.status),
            version_codes: self
                .version_codes
                .into_iter()
                .map(Int64::into_text)
                .collect(),
            user_fraction: self.user_fraction,
            release_notes: self
                .release_notes
                .into_iter()
                .map(|note| LocalizedTextInfo {
                    language: note.language,
                    text: note.text,
                })
                .collect(),
            in_app_update_priority: self.in_app_update_priority,
        }
    }
}

/// Hints for edit calls: reads need "View app information"; a 404 on the
/// insert means the app is unknown, on anything inside the edit it is kept as
/// Google's message.
fn edit_hints(target: Target<'_>) -> ErrorHints<'_> {
    ErrorHints {
        api: GoogleApi::Publisher,
        permission: PERMISSION_VIEW_APP_INFO,
        target,
    }
}

impl GooglePlayClient {
    /// Runs `read` inside a throwaway edit of `package_name`: inserts an edit,
    /// hands its id to `read`, then **always** deletes the edit, whether `read`
    /// succeeded or not. Nothing is ever committed.
    ///
    /// The delete is best effort: its failure is traced through the debug
    /// logger and otherwise ignored (an abandoned edit expires on its own and
    /// is invalidated by the next insert), so it never replaces `read`'s result
    /// or error. The session holds the client's edit lock from insert to
    /// delete. If the caller drops the future mid-session, the delete does not
    /// run; the edit then expires on its own.
    ///
    /// # Errors
    /// The insert's error (then `read` and the delete never run), else `read`'s.
    pub(super) async fn with_edit<T, F, Fut>(
        &self,
        package_name: &str,
        read: F,
    ) -> Result<T, StackError>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = Result<T, StackError>>,
    {
        let _session = self.edit_lock().lock().await;
        let edit_id = self.insert_edit(package_name).await?;
        let result = read(edit_id.clone()).await;

        if let Err(err) = self.delete_edit(package_name, &edit_id).await {
            self.trace(|| {
                format!(
                    "[RustCore] Google Play: could not delete edit {edit_id} of \
                     {package_name} (ignored; it expires on its own): {err}"
                )
            });
        }
        result
    }

    /// `POST …/applications/{package_name}/edits` with an empty `AppEdit`,
    /// returning the new edit's id.
    async fn insert_edit(&self, package_name: &str) -> Result<String, StackError> {
        let url = self.publisher_url(package_name, &[EDITS])?;
        let body = self
            .execute(
                || self.http().post(url.clone()).json(&json!({})),
                &edit_hints(Target::App(package_name)),
            )
            .await?;
        let edit: AppEdit = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("edits.insert response: {e}")))?;
        non_blank(edit.id).ok_or_else(|| StackError::decode("edits.insert response: missing id"))
    }

    /// `DELETE …/applications/{package_name}/edits/{edit_id}`.
    async fn delete_edit(&self, package_name: &str, edit_id: &str) -> Result<(), StackError> {
        let url = self.publisher_url(package_name, &[EDITS, edit_id])?;
        self.execute(
            || self.http().delete(url.clone()),
            &edit_hints(Target::AppResource(package_name)),
        )
        .await
        .map(|_| ())
    }

    /// `GET …/applications/{package_name}/edits/{edit_id}/{resource}`, returning
    /// the 2xx body.
    async fn get_edit_resource(
        &self,
        package_name: &str,
        edit_id: &str,
        resource: &str,
    ) -> Result<String, StackError> {
        let url = self.publisher_url(package_name, &[EDITS, edit_id, resource])?;
        self.execute(
            || self.http().get(url.clone()),
            &edit_hints(Target::AppResource(package_name)),
        )
        .await
    }

    /// One edit session that reads `resource`, returning its body.
    async fn read_in_edit(&self, package_name: &str, resource: &str) -> Result<String, StackError> {
        self.with_edit(package_name, |edit_id| async move {
            self.get_edit_resource(package_name, &edit_id, resource)
                .await
        })
        .await
    }

    /// The app's default language and store contact details (`edits.details`).
    ///
    /// Also the "can this service account reach the app?" check: `Ok` means
    /// yes, `Http 404` means Google Play has no such package (an invalid name
    /// is rejected locally the same way), `Auth` means no access or a disabled
    /// API.
    ///
    /// # Errors
    /// See [`Self::with_edit`] and [`Self::execute`]; [`StackError::Decode`] on a
    /// malformed body.
    pub(crate) async fn fetch_app_details(
        &self,
        package_name: &str,
    ) -> Result<AppDetailsInfo, StackError> {
        let body = self.read_in_edit(package_name, "details").await?;
        let details: PlayAppDetails = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("edits.details response: {e}")))?;
        Ok(details.into_info(package_name))
    }

    /// The app's localized store listings (`edits.listings.list`), in Google's
    /// order. Listings without a language are skipped.
    ///
    /// # Errors
    /// As [`Self::fetch_app_details`].
    pub(crate) async fn fetch_store_listings(
        &self,
        package_name: &str,
    ) -> Result<Vec<StoreListingInfo>, StackError> {
        let body = self.read_in_edit(package_name, "listings").await?;
        let response: ListingsListResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("edits.listings response: {e}")))?;
        Ok(response
            .listings
            .into_iter()
            .filter_map(PlayListing::into_info)
            .collect())
    }

    /// The app's release tracks with their releases (`edits.tracks.list`), in
    /// Google's order; tracks without releases included. Tracks without a name
    /// are skipped.
    ///
    /// # Errors
    /// As [`Self::fetch_app_details`].
    pub(crate) async fn fetch_tracks(
        &self,
        package_name: &str,
    ) -> Result<Vec<TrackInfo>, StackError> {
        let body = self.read_in_edit(package_name, "tracks").await?;
        let response: TracksListResponse = serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("edits.tracks response: {e}")))?;
        Ok(response
            .tracks
            .into_iter()
            .filter_map(PlayTrack::into_info)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::googleplay::test_support::{client, mount_token, CapturingLogger};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    const PACKAGE: &str = "com.example.app";
    const EDITS_PATH: &str = "/androidpublisher/v3/applications/com.example.app/edits";

    fn insert() -> wiremock::MockBuilder {
        Mock::given(method("POST")).and(path(EDITS_PATH))
    }

    fn get(edit: &str, resource: &str) -> wiremock::MockBuilder {
        Mock::given(method("GET")).and(path(format!("{EDITS_PATH}/{edit}/{resource}")))
    }

    fn delete(edit: &str) -> wiremock::MockBuilder {
        Mock::given(method("DELETE")).and(path(format!("{EDITS_PATH}/{edit}")))
    }

    fn edit_created(id: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": id, "expiryTimeSeconds": "1700003600"
        }))
    }

    fn google_error(status: u16, message: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(serde_json::json!({
            "error": { "code": status, "message": message }
        }))
    }

    /// Mounts an edit `E1` whose `resource` answers `read` and whose delete
    /// answers `deleted`; each is expected exactly once.
    async fn mount_session(
        server: &MockServer,
        resource: &str,
        read: ResponseTemplate,
        deleted: ResponseTemplate,
    ) {
        insert()
            .and(body_json(serde_json::json!({})))
            .respond_with(edit_created("E1"))
            .expect(1)
            .mount(server)
            .await;
        get("E1", resource)
            .respond_with(read)
            .expect(1)
            .mount(server)
            .await;
        delete("E1")
            .respond_with(deleted)
            .expect(1)
            .mount(server)
            .await;
    }

    /// `(method, path)` of every request after the token exchange.
    async fn api_calls(server: &MockServer) -> Vec<(String, String)> {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() != "/token")
            .map(|r| (r.method.to_string(), r.url.path().to_string()))
            .collect()
    }

    fn call(method: &str, suffix: &str) -> (String, String) {
        (method.to_string(), format!("{EDITS_PATH}{suffix}"))
    }

    // -----------------------------------------------------------------------
    // Edit sessions
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn session_inserts_reads_then_deletes() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_session(
            &server,
            "details",
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "defaultLanguage": "en-US",
                "contactEmail": "support@example.com",
                "contactPhone": "+1 555 0100",
                "contactWebsite": "https://example.com"
            })),
            // Google answers an empty JSON object.
            ResponseTemplate::new(200).set_body_string("{}"),
        )
        .await;

        let details = client(&server).fetch_app_details(PACKAGE).await.unwrap();
        assert_eq!(
            details,
            AppDetailsInfo {
                app_id: PACKAGE.into(),
                default_language: Some("en-US".into()),
                contact_email: Some("support@example.com".into()),
                contact_phone: Some("+1 555 0100".into()),
                contact_website: Some("https://example.com".into()),
            }
        );
        assert_eq!(
            api_calls(&server).await,
            vec![
                call("POST", ""),
                call("GET", "/E1/details"),
                call("DELETE", "/E1")
            ]
        );
    }

    #[tokio::test]
    async fn failed_read_still_deletes_and_its_error_wins() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_session(
            &server,
            "details",
            google_error(500, "read exploded"),
            google_error(503, "delete exploded"),
        )
        .await;

        match client(&server)
            .fetch_app_details(PACKAGE)
            .await
            .unwrap_err()
        {
            StackError::Http { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(message, "read exploded");
            }
            other => panic!("expected the read's Http 500, got {other:?}"),
        }
        assert_eq!(
            api_calls(&server).await,
            vec![
                call("POST", ""),
                call("GET", "/E1/details"),
                call("DELETE", "/E1")
            ]
        );
    }

    #[tokio::test]
    async fn malformed_read_is_decode_and_the_edit_is_still_deleted() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_session(
            &server,
            "tracks",
            ResponseTemplate::new(200).set_body_string(r#"{"tracks": "nope"}"#),
            ResponseTemplate::new(204),
        )
        .await;

        match client(&server).fetch_tracks(PACKAGE).await.unwrap_err() {
            StackError::Decode { message } => assert!(message.contains("edits.tracks response")),
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn failed_delete_keeps_the_read_and_is_traced() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_session(
            &server,
            "listings",
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "kind": "androidpublisher#listingsListResponse",
                "listings": [{ "language": "en-US", "title": "Example" }]
            })),
            google_error(503, "try later"),
        )
        .await;

        let logger = Arc::new(CapturingLogger::default());
        let client = client(&server).with_debug_logger(Some(logger.clone()));
        let listings = client.fetch_store_listings(PACKAGE).await.unwrap();
        assert_eq!(listings.len(), 1);

        let messages = logger.messages();
        let last = messages.last().expect("a trace line");
        assert_eq!(
            last,
            "[RustCore] Google Play: could not delete edit E1 of com.example.app \
             (ignored; it expires on its own): HTTP 503: try later"
        );
    }

    #[tokio::test]
    async fn failed_insert_skips_read_and_delete() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        insert()
            .respond_with(google_error(404, "Package not found: com.example.app."))
            .expect(1)
            .mount(&server)
            .await;
        get("E1", "details")
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        delete("E1")
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        match client(&server)
            .fetch_app_details(PACKAGE)
            .await
            .unwrap_err()
        {
            StackError::Http { status, message } => {
                assert_eq!(status, 404);
                assert_eq!(
                    message,
                    "Google Play found no app with package name com.example.app (HTTP 404): \
                     check the package name. Google said: Package not found: com.example.app."
                );
            }
            other => panic!("expected Http 404, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn insert_without_id_is_decode() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        insert()
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"id":" "}"#))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server)
            .fetch_app_details(PACKAGE)
            .await
            .unwrap_err()
        {
            StackError::Decode { message } => assert!(message.contains("missing id")),
            other => panic!("expected Decode, got {other:?}"),
        }
        assert_eq!(api_calls(&server).await, vec![call("POST", "")]);
    }

    #[tokio::test]
    async fn insert_forbidden_is_auth_naming_the_app() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        insert()
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": { "code": 403, "message": "The caller does not have permission",
                           "status": "PERMISSION_DENIED" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server)
            .fetch_app_details(PACKAGE)
            .await
            .unwrap_err()
        {
            StackError::Auth { message } => {
                assert!(message.contains("cannot access the app com.example.app"));
                assert!(message.contains("\"View app information (read-only)\""));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn insert_with_api_disabled_names_the_android_developer_api() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        insert()
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": { "code": 403, "status": "PERMISSION_DENIED", "message": "disabled",
                    "errors": [{ "reason": "accessNotConfigured" }] }
            })))
            .mount(&server)
            .await;

        match client(&server).fetch_tracks(PACKAGE).await.unwrap_err() {
            StackError::Auth { message } => assert!(message.starts_with(
                "Google Play Android Developer API is not enabled for this service \
                 account's Google Cloud project. Enable it in the Google Cloud console"
            )),
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_package_sends_nothing() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 0).await;

        let err = client(&server)
            .fetch_store_listings("com")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 404, .. }));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    /// Hands out `E1`, `E2`, … so overlapping sessions would be visible.
    struct NumberedEdits {
        next: AtomicUsize,
    }

    impl Respond for NumberedEdits {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
            edit_created(&format!("E{n}"))
        }
    }

    #[tokio::test]
    async fn concurrent_reads_never_overlap_their_edits() {
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
            // Slow reads give a second session every chance to interleave.
            get(edit, "details")
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string("{}")
                        .set_delay(Duration::from_millis(150)),
                )
                .expect(1)
                .mount(&server)
                .await;
            delete(edit)
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
        }

        let client = client(&server);
        let (first, second) = tokio::join!(
            client.fetch_app_details(PACKAGE),
            client.fetch_app_details(PACKAGE)
        );
        first.unwrap();
        second.unwrap();

        assert_eq!(
            api_calls(&server).await,
            vec![
                call("POST", ""),
                call("GET", "/E1/details"),
                call("DELETE", "/E1"),
                call("POST", ""),
                call("GET", "/E2/details"),
                call("DELETE", "/E2"),
            ]
        );
    }

    #[tokio::test]
    async fn debug_logger_traces_every_session_call() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_session(
            &server,
            "details",
            ResponseTemplate::new(200).set_body_string("{}"),
            ResponseTemplate::new(204),
        )
        .await;

        let logger = Arc::new(CapturingLogger::default());
        let client = client(&server).with_debug_logger(Some(logger.clone()));
        client.fetch_app_details(PACKAGE).await.unwrap();

        // Token (2 lines), then a request + response line per edit call.
        let messages = logger.messages();
        assert_eq!(messages.len(), 8);
        let uri = server.uri();
        assert!(messages[2].contains(&format!("curl -X POST '{uri}{EDITS_PATH}'")));
        assert!(messages[4].contains(&format!("curl -X GET '{uri}{EDITS_PATH}/E1/details'")));
        assert!(messages[6].contains(&format!("curl -X DELETE '{uri}{EDITS_PATH}/E1'")));
        assert!(messages[7].starts_with("[RustCore] ← 204"));
    }

    // -----------------------------------------------------------------------
    // Mapping
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn details_blank_or_missing_fields_are_none() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_session(
            &server,
            "details",
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "defaultLanguage": "pt-BR", "contactEmail": "  "
            })),
            ResponseTemplate::new(204),
        )
        .await;

        let details = client(&server).fetch_app_details(PACKAGE).await.unwrap();
        assert_eq!(
            details,
            AppDetailsInfo {
                app_id: PACKAGE.into(),
                default_language: Some("pt-BR".into()),
                contact_email: None,
                contact_phone: None,
                contact_website: None,
            }
        );
    }

    #[tokio::test]
    async fn listings_map_in_order_and_skip_unkeyed_ones() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_session(
            &server,
            "listings",
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "kind": "androidpublisher#listingsListResponse",
                "listings": [
                    { "language": "en-US", "title": "Example", "shortDescription": "Short",
                      "fullDescription": "Full\nText", "video": "https://youtu.be/x" },
                    { "title": "No language" },
                    { "language": "de-AT", "title": "Beispiel" }
                ]
            })),
            ResponseTemplate::new(204),
        )
        .await;

        let listings = client(&server).fetch_store_listings(PACKAGE).await.unwrap();
        assert_eq!(
            listings,
            vec![
                StoreListingInfo {
                    language: "en-US".into(),
                    title: Some("Example".into()),
                    short_description: Some("Short".into()),
                    full_description: Some("Full\nText".into()),
                    video: Some("https://youtu.be/x".into()),
                },
                StoreListingInfo {
                    language: "de-AT".into(),
                    title: Some("Beispiel".into()),
                    short_description: None,
                    full_description: None,
                    video: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn tracks_map_releases() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        mount_session(
            &server,
            "tracks",
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "kind": "androidpublisher#tracksListResponse",
                "tracks": [
                    { "track": "production", "releases": [{
                        "name": "1.4.2 (42)",
                        "versionCodes": ["42", 41],
                        "status": "inProgress",
                        "userFraction": 0.25,
                        "releaseNotes": [
                            { "language": "en-US", "text": "Bug fixes" },
                            { "language": "pt-BR" }
                        ],
                        "inAppUpdatePriority": 3,
                        "countryTargeting": { "countries": ["BR"], "includeRestOfWorld": false }
                    }, {
                        "versionCodes": ["40"],
                        "status": "completed"
                    }] },
                    { "track": "internal" },
                    { "releases": [] }
                ]
            })),
            ResponseTemplate::new(204),
        )
        .await;

        let tracks = client(&server).fetch_tracks(PACKAGE).await.unwrap();
        assert_eq!(
            tracks,
            vec![
                TrackInfo {
                    track: "production".into(),
                    releases: vec![
                        TrackReleaseInfo {
                            name: Some("1.4.2 (42)".into()),
                            status: Some("inProgress".into()),
                            version_codes: vec!["42".into(), "41".into()],
                            user_fraction: Some(0.25),
                            release_notes: vec![
                                LocalizedTextInfo {
                                    language: "en-US".into(),
                                    text: "Bug fixes".into(),
                                },
                                LocalizedTextInfo {
                                    language: "pt-BR".into(),
                                    text: String::new(),
                                },
                            ],
                            in_app_update_priority: Some(3),
                        },
                        TrackReleaseInfo {
                            name: None,
                            status: Some("completed".into()),
                            version_codes: vec!["40".into()],
                            user_fraction: None,
                            release_notes: vec![],
                            in_app_update_priority: None,
                        },
                    ],
                },
                TrackInfo {
                    track: "internal".into(),
                    releases: vec![],
                },
            ]
        );
    }

    #[tokio::test]
    async fn empty_lists_are_empty() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        insert()
            .respond_with(NumberedEdits {
                next: AtomicUsize::new(0),
            })
            .expect(2)
            .mount(&server)
            .await;
        get("E1", "listings")
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "kind": "androidpublisher#listingsListResponse"
            })))
            .mount(&server)
            .await;
        get("E2", "tracks")
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        for edit in ["E1", "E2"] {
            delete(edit)
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
        }

        let client = client(&server);
        assert!(client
            .fetch_store_listings(PACKAGE)
            .await
            .unwrap()
            .is_empty());
        assert!(client.fetch_tracks(PACKAGE).await.unwrap().is_empty());
    }
}
