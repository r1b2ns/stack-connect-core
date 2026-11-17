//! Users of a Google Play developer account and their per-app grants
//! (`developers.users`, `developers.users.grants`).
//!
//! Every call is scoped to one developer account, addressed by its numeric ID
//! (the API cannot discover it). None of them opens an edit, so none can
//! cancel a CI upload. The calling service account needs "Admin (all
//! permissions)". Permissions cross as raw strings: a permission Google adds
//! later never breaks decoding, and the core never drops one on a round trip.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{json, Map};

use super::client::{is_valid_package_name, GooglePlayClient};
use super::error::{local_rejection, ErrorHints, GoogleApi, Target, PERMISSION_ADMIN};
use super::non_blank;
use crate::domain::{PlayGrantInfo, PlayUserInfo};
use crate::error::StackError;
use crate::logging::redact::{mask_email, quoted, shape};
use crate::logging::target;

/// The `users` collection under a developer account.
const USERS: &str = "users";
/// The `grants` collection under a user.
const GRANTS: &str = "grants";
/// Users per page. Google does not document a maximum; larger values are
/// expected to be coerced.
const PAGE_SIZE: u32 = 100;
/// Hard cap on `users.list` pages, so a server that never stops paginating
/// cannot loop forever.
const MAX_PAGES: usize = 100;
/// `users.patch` replaces exactly these fields.
const USER_UPDATE_MASK: &str = "developerAccountPermissions,expirationTime";
/// `grants.patch` replaces exactly this field.
const GRANT_UPDATE_MASK: &str = "appLevelPermissions";

/// `ListUsersResponse`. Proto3 JSON omits empty fields.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListUsersResponse {
    #[serde(default)]
    users: Vec<PlayUser>,
    #[serde(default)]
    next_page_token: Option<String>,
}

/// A `User`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayUser {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    access_state: Option<String>,
    #[serde(default)]
    expiration_time: Option<String>,
    #[serde(default)]
    developer_account_permissions: Vec<String>,
    #[serde(default)]
    grants: Vec<PlayGrant>,
    #[serde(default)]
    partial: bool,
}

/// A `Grant`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayGrant {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    package_name: Option<String>,
    #[serde(default)]
    app_level_permissions: Vec<String>,
}

/// The last `/` segment of a resource name (`…/users/{email}`,
/// `…/grants/{package}`), when non-blank.
fn last_segment(name: Option<&str>) -> Option<String> {
    non_blank(name?.rsplit('/').next().map(str::to_string))
}

impl PlayUser {
    /// `None` without an e-mail (also looked for at the end of `name`): it is
    /// what identifies a user in every other call.
    fn into_info(self) -> Option<PlayUserInfo> {
        let email = non_blank(self.email).or_else(|| last_segment(self.name.as_deref()))?;
        Some(PlayUserInfo {
            email,
            name: non_blank(self.name),
            access_state: non_blank(self.access_state),
            expiration_time: non_blank(self.expiration_time),
            developer_account_permissions: self.developer_account_permissions,
            grants: self
                .grants
                .into_iter()
                .filter_map(PlayGrant::into_info)
                .collect(),
            partial: self.partial,
        })
    }
}

impl PlayGrant {
    /// `None` without a package name or, for a draft app (which has none), an
    /// app ID at the end of `name`.
    fn into_info(self) -> Option<PlayGrantInfo> {
        Some(PlayGrantInfo {
            package_name: non_blank(self.package_name)
                .or_else(|| last_segment(self.name.as_deref()))?,
            app_level_permissions: self.app_level_permissions,
        })
    }
}

/// A locally rejected argument, as [`StackError::Http`] `400` (see
/// [`local_rejection`]).
fn invalid(detail: impl std::fmt::Display) -> StackError {
    local_rejection(400, target::PLAY_USERS, detail)
}

/// An e-mail that can address a user: something on both sides of an `@`, no
/// whitespace and no `/` (it becomes a path segment). The rejection names the
/// address masked.
fn ensure_email(email: &str) -> Result<(), StackError> {
    let valid = email
        .split_once('@')
        .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
        && !email.chars().any(|c| c.is_whitespace() || c == '/');
    if valid {
        Ok(())
    } else {
        Err(invalid(format!(
            "{} is not a valid e-mail address ({})",
            quoted(&mask_email(email)),
            shape(email)
        )))
    }
}

/// A package name, or the numeric app ID Google uses for a draft app's grant.
fn ensure_grant_app(package_name: &str) -> Result<(), StackError> {
    let draft_app_id = !package_name.is_empty() && package_name.bytes().all(|b| b.is_ascii_digit());
    if is_valid_package_name(package_name) || draft_app_id {
        Ok(())
    } else {
        Err(invalid(format!(
            "grant package name {} is not a valid Android package name ({})",
            quoted(package_name),
            shape(package_name)
        )))
    }
}

fn hints(target: Target<'_>) -> ErrorHints<'_> {
    ErrorHints {
        api: GoogleApi::Publisher,
        permission: PERMISSION_ADMIN,
        target,
    }
}

fn user_name(developer_id: &str, email: &str) -> String {
    format!("developers/{developer_id}/users/{email}")
}

fn decode_user(body: &str, what: &str) -> Result<PlayUserInfo, StackError> {
    let user: PlayUser = serde_json::from_str(body)
        .map_err(|e| StackError::decode(format!("{what} response: {e}")))?;
    user.into_info()
        .ok_or_else(|| StackError::decode(format!("{what} response: user without an e-mail")))
}

fn decode_grant(body: &str, what: &str) -> Result<PlayGrantInfo, StackError> {
    let grant: PlayGrant = serde_json::from_str(body)
        .map_err(|e| StackError::decode(format!("{what} response: {e}")))?;
    grant
        .into_info()
        .ok_or_else(|| StackError::decode(format!("{what} response: grant without an app")))
}

impl GooglePlayClient {
    /// Every user of `developer_id`, following `nextPageToken` until it is
    /// absent or blank. Users without an e-mail are skipped.
    ///
    /// # Errors
    /// [`StackError::Http`] `404` naming the developer account when Google
    /// does not know it (or locally, for an ID that cannot be one);
    /// [`StackError::Auth`] when access is denied; [`StackError::Decode`] on a
    /// malformed page, a repeated page token or more than [`MAX_PAGES`] pages;
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_users(
        &self,
        developer_id: &str,
    ) -> Result<Vec<PlayUserInfo>, StackError> {
        let url = self.developer_url(developer_id, &[USERS])?;
        let hints = hints(Target::Developer(developer_id));
        let mut users = Vec::new();
        let mut seen_tokens = HashSet::new();
        let mut page_token: Option<String> = None;

        for _ in 0..MAX_PAGES {
            let body = self
                .execute(
                    || {
                        let request = self
                            .http()
                            .get(url.clone())
                            .query(&[("pageSize", PAGE_SIZE)]);
                        match &page_token {
                            Some(token) => request.query(&[("pageToken", token)]),
                            None => request,
                        }
                    },
                    &hints,
                )
                .await?;
            let page: ListUsersResponse = serde_json::from_str(&body)
                .map_err(|e| StackError::decode(format!("users.list response: {e}")))?;
            users.extend(page.users.into_iter().filter_map(PlayUser::into_info));

            match page.next_page_token.filter(|t| !t.trim().is_empty()) {
                None => return Ok(users),
                Some(next) if !seen_tokens.insert(next.clone()) => {
                    return Err(StackError::decode("users.list repeated nextPageToken"));
                }
                Some(next) => page_token = Some(next),
            }
        }
        Err(StackError::decode(format!(
            "users.list exceeded {MAX_PAGES} pages"
        )))
    }

    /// Invites `email` (`users.create`), then creates each grant
    /// (`grants.create`; `User.grants` is output-only). A failed grant removes
    /// the new user again (best effort, traced on failure) and its error is
    /// returned.
    ///
    /// # Errors
    /// Local `Http 400` for an invalid e-mail or grant package (nothing is
    /// sent); otherwise as [`Self::fetch_users`].
    pub(crate) async fn invite_user(
        &self,
        developer_id: &str,
        email: &str,
        account_permissions: &[String],
        grants: &[PlayGrantInfo],
        expiration_time: Option<&str>,
    ) -> Result<PlayUserInfo, StackError> {
        ensure_email(email)?;
        for grant in grants {
            ensure_grant_app(&grant.package_name)?;
        }
        let url = self.developer_url(developer_id, &[USERS])?;

        let mut body = Map::new();
        body.insert("name".into(), json!(user_name(developer_id, email)));
        body.insert("email".into(), json!(email));
        body.insert(
            "developerAccountPermissions".into(),
            json!(account_permissions),
        );
        if let Some(expiration) = expiration_time {
            body.insert("expirationTime".into(), json!(expiration));
        }
        let response = self
            .execute(
                || self.http().post(url.clone()).json(&body),
                &hints(Target::Developer(developer_id)),
            )
            .await?;
        let mut user = decode_user(&response, "users.create")?;

        let mut created = Vec::with_capacity(grants.len());
        for grant in grants {
            match self
                .create_grant(
                    developer_id,
                    email,
                    &grant.package_name,
                    &grant.app_level_permissions,
                )
                .await
            {
                Ok(stored) => created.push(stored),
                Err(err) => {
                    if let Err(cleanup) = self.delete_user(developer_id, email).await {
                        self.trace(|| {
                            format!(
                                "[RustCore] Google Play: could not remove the invitation of \
                                 {email} after a failed grant (ignored): {cleanup}"
                            )
                        });
                    }
                    return Err(err);
                }
            }
        }
        if !grants.is_empty() {
            user.grants = created;
        }
        Ok(user)
    }

    /// `users.patch` with [`USER_UPDATE_MASK`]: both fields are replaced, so
    /// `expiration_time: None` removes the expiry.
    ///
    /// # Errors
    /// As [`Self::invite_user`].
    pub(crate) async fn update_user(
        &self,
        developer_id: &str,
        email: &str,
        account_permissions: &[String],
        expiration_time: Option<&str>,
    ) -> Result<PlayUserInfo, StackError> {
        ensure_email(email)?;
        let url = self.developer_url(developer_id, &[USERS, email])?;
        let mut body = Map::new();
        body.insert("name".into(), json!(user_name(developer_id, email)));
        body.insert(
            "developerAccountPermissions".into(),
            json!(account_permissions),
        );
        if let Some(expiration) = expiration_time {
            body.insert("expirationTime".into(), json!(expiration));
        }
        let response = self
            .execute(
                || {
                    self.http()
                        .patch(url.clone())
                        .query(&[("updateMask", USER_UPDATE_MASK)])
                        .json(&body)
                },
                &hints(Target::DeveloperResource(developer_id)),
            )
            .await?;
        decode_user(&response, "users.patch")
    }

    /// `users.delete`.
    ///
    /// # Errors
    /// As [`Self::invite_user`].
    pub(crate) async fn delete_user(
        &self,
        developer_id: &str,
        email: &str,
    ) -> Result<(), StackError> {
        ensure_email(email)?;
        let url = self.developer_url(developer_id, &[USERS, email])?;
        self.execute(
            || self.http().delete(url.clone()),
            &hints(Target::DeveloperResource(developer_id)),
        )
        .await
        .map(|_| ())
    }

    /// Updates the grant (`grants.patch` with [`GRANT_UPDATE_MASK`]); when
    /// Google answers `404` (no grant for this app yet), creates it instead.
    ///
    /// # Errors
    /// As [`Self::invite_user`]; the create's error when the fallback fails.
    pub(crate) async fn set_grant(
        &self,
        developer_id: &str,
        email: &str,
        package_name: &str,
        app_level_permissions: &[String],
    ) -> Result<PlayGrantInfo, StackError> {
        ensure_email(email)?;
        ensure_grant_app(package_name)?;
        let url = self.developer_url(developer_id, &[USERS, email, GRANTS, package_name])?;
        let body = json!({
            "name": format!("{}/grants/{package_name}", user_name(developer_id, email)),
            "appLevelPermissions": app_level_permissions,
        });
        let patched = self
            .execute(
                || {
                    self.http()
                        .patch(url.clone())
                        .query(&[("updateMask", GRANT_UPDATE_MASK)])
                        .json(&body)
                },
                &hints(Target::DeveloperResource(developer_id)),
            )
            .await;
        match patched {
            Ok(response) => decode_grant(&response, "grants.patch"),
            Err(StackError::Http { status: 404, .. }) => {
                self.create_grant(developer_id, email, package_name, app_level_permissions)
                    .await
            }
            Err(err) => Err(err),
        }
    }

    /// `grants.delete`.
    ///
    /// # Errors
    /// As [`Self::invite_user`].
    pub(crate) async fn delete_grant(
        &self,
        developer_id: &str,
        email: &str,
        package_name: &str,
    ) -> Result<(), StackError> {
        ensure_email(email)?;
        ensure_grant_app(package_name)?;
        let url = self.developer_url(developer_id, &[USERS, email, GRANTS, package_name])?;
        self.execute(
            || self.http().delete(url.clone()),
            &hints(Target::DeveloperResource(developer_id)),
        )
        .await
        .map(|_| ())
    }

    /// `grants.create` under the user.
    async fn create_grant(
        &self,
        developer_id: &str,
        email: &str,
        package_name: &str,
        app_level_permissions: &[String],
    ) -> Result<PlayGrantInfo, StackError> {
        let url = self.developer_url(developer_id, &[USERS, email, GRANTS])?;
        let body = json!({
            "name": format!("{}/grants/{package_name}", user_name(developer_id, email)),
            "packageName": package_name,
            "appLevelPermissions": app_level_permissions,
        });
        let response = self
            .execute(
                || self.http().post(url.clone()).json(&body),
                &hints(Target::DeveloperResource(developer_id)),
            )
            .await?;
        decode_grant(&response, "grants.create")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::testing::capture_logs;
    use crate::logging::LogLevel;
    use crate::providers::googleplay::test_support::{
        client, mount_token, CapturingLogger, EMAIL as SA_EMAIL,
    };
    use std::sync::Arc;
    use wiremock::matchers::{body_json, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const DEV: &str = "5374";
    const USERS_PATH: &str = "/androidpublisher/v3/developers/5374/users";
    const EMAIL: &str = "ana@example.com";

    fn user_path(email: &str) -> String {
        format!("{USERS_PATH}/{email}")
    }

    fn grant_path(email: &str, package: &str) -> String {
        format!("{USERS_PATH}/{email}/grants/{package}")
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn google_error(status: u16, message: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(serde_json::json!({
            "error": { "code": status, "message": message, "status": "X" }
        }))
    }

    async fn api_calls(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() != "/token")
            .map(|r| match r.url.query() {
                Some(q) => format!("{} {}?{q}", r.method, r.url.path()),
                None => format!("{} {}", r.method, r.url.path()),
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // fetch_users
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_users_follows_pages_and_maps_users() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .and(query_param("pageSize", "100"))
            .and(query_param_is_missing("pageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "users": [{
                    "name": "developers/5374/users/ana@example.com",
                    "email": "ana@example.com",
                    "accessState": "ACCESS_GRANTED",
                    "expirationTime": "2027-01-01T00:00:00Z",
                    "developerAccountPermissions": ["CAN_MANAGE_PERMISSIONS_GLOBAL", "CAN_FUTURE_THING"],
                    "grants": [
                        { "name": "developers/5374/users/ana@example.com/grants/com.example.app",
                          "packageName": "com.example.app",
                          "appLevelPermissions": ["CAN_REPLY_TO_REVIEWS"] },
                        { "name": "developers/5374/users/ana@example.com/grants/4975", "appLevelPermissions": [] }
                    ],
                    "partial": true
                }],
                "nextPageToken": "a+b/c="
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .and(query_param("pageToken", "a+b/c="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "users": [
                    { "name": "developers/5374/users/bot@p.iam.gserviceaccount.com",
                      "accessState": "INVITED" },
                    { "accessState": "INVITED" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let users = client(&server).fetch_users(DEV).await.unwrap();
        assert_eq!(
            users,
            vec![
                PlayUserInfo {
                    email: "ana@example.com".into(),
                    name: Some("developers/5374/users/ana@example.com".into()),
                    access_state: Some("ACCESS_GRANTED".into()),
                    expiration_time: Some("2027-01-01T00:00:00Z".into()),
                    developer_account_permissions: strings(&[
                        "CAN_MANAGE_PERMISSIONS_GLOBAL",
                        "CAN_FUTURE_THING"
                    ]),
                    grants: vec![
                        PlayGrantInfo {
                            package_name: "com.example.app".into(),
                            app_level_permissions: strings(&["CAN_REPLY_TO_REVIEWS"]),
                        },
                        // A draft app: Google's app ID from the resource name.
                        PlayGrantInfo {
                            package_name: "4975".into(),
                            app_level_permissions: vec![],
                        },
                    ],
                    partial: true,
                },
                // No `email`: taken from the resource name.
                PlayUserInfo {
                    email: "bot@p.iam.gserviceaccount.com".into(),
                    name: Some("developers/5374/users/bot@p.iam.gserviceaccount.com".into()),
                    access_state: Some("INVITED".into()),
                    expiration_time: None,
                    developer_account_permissions: vec![],
                    grants: vec![],
                    partial: false,
                },
            ]
        );
        // The page token is URL-encoded.
        let calls = api_calls(&server).await;
        assert!(calls[1].ends_with("pageToken=a%2Bb%2Fc%3D"), "{calls:?}");
    }

    #[tokio::test]
    async fn fetch_users_empty_object_is_empty() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client(&server).fetch_users(DEV).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn fetch_users_repeated_token_is_decode() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "users": [], "nextPageToken": "T1"
            })))
            .expect(2)
            .mount(&server)
            .await;

        match client(&server).fetch_users(DEV).await.unwrap_err() {
            StackError::Decode { message } => assert!(message.contains("repeated nextPageToken")),
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_developer_is_a_404_naming_it() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .respond_with(google_error(404, "Developer not found."))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server).fetch_users(DEV).await.unwrap_err() {
            StackError::Http { status, message } => {
                assert_eq!(status, 404);
                assert!(message
                    .starts_with("Google Play found no developer account with ID 5374 (HTTP 404)"));
                assert!(message.ends_with(
                    "Google said: Developer not found. [X] (HTTP 404 on GET developers/5374/users)"
                ));
            }
            other => panic!("expected Http 404, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn forbidden_keeps_googles_message_naming_the_permission() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .respond_with(google_error(
                403,
                "The caller does not have permission CAN_MANAGE_PERMISSIONS_GLOBAL.",
            ))
            .mount(&server)
            .await;

        match client(&server).fetch_users(DEV).await.unwrap_err() {
            StackError::Auth { message } => {
                assert!(message.contains("cannot access the developer account 5374"));
                assert!(message.contains("\"Admin (all permissions)\""));
                assert!(message.ends_with(
                    "Google said: The caller does not have permission \
                     CAN_MANAGE_PERMISSIONS_GLOBAL. [X] (HTTP 403 on GET developers/5374/users)"
                ));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    /// The `Http` status and message of `err`.
    fn http(err: StackError) -> (u16, String) {
        match err {
            StackError::Http { status, message } => (status, message),
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn local_rejections_say_so_log_a_warning_and_mask_the_email() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 0).await;
        let client = client(&server);
        let logs = capture_logs();

        let (status, message) = http(client.fetch_users("abc").await.unwrap_err());
        assert_eq!(status, 404);
        assert_eq!(
            message,
            "local validation failed: developer ID \"abc\" is not numeric (3 chars: \
             letters); it is the number after /developers/ in a Play Console URL; request \
             not sent to Google"
        );
        let (_, message) = http(client.fetch_users("").await.unwrap_err());
        assert!(
            message.starts_with("local validation failed: developer ID \"\" is empty (0 chars)"),
            "{message}"
        );

        let (status, message) = http(
            client
                .delete_user(DEV, "dev@agency example")
                .await
                .unwrap_err(),
        );
        assert_eq!(status, 400);
        assert_eq!(
            message,
            "local validation failed: \"d***@agency example\" is not a valid e-mail address \
             (18 chars: letters, whitespace, '@'); request not sent to Google"
        );

        let (_, message) = http(
            client
                .set_grant(DEV, EMAIL, "com..app", &[])
                .await
                .unwrap_err(),
        );
        assert_eq!(
            message,
            "local validation failed: grant package name \"com..app\" is not a valid Android \
             package name (8 chars: letters, '.'); request not sent to Google"
        );

        // Each rejection is one Warn line carrying the same text, and none
        // reached the network.
        let warns = logs.messages(LogLevel::Warn, "googleplay.users");
        assert_eq!(warns.len(), 4, "{warns:?}");
        assert!(warns
            .iter()
            .all(|w| w.starts_with("local validation failed:")));
        assert!(logs.messages(LogLevel::Debug, "http").is_empty());
        assert!(!logs.all_text().contains("dev@"));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn google_404_on_the_users_list_names_the_resource_and_keeps_googles_words() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .respond_with(
                ResponseTemplate::new(404)
                    .insert_header("x-goog-request-id", "goog-404")
                    .set_body_json(serde_json::json!({
                        "error": {
                            "code": 404,
                            "message": "Requested entity was not found.",
                            "status": "NOT_FOUND"
                        }
                    })),
            )
            .mount(&server)
            .await;

        let logs = capture_logs();
        let (status, message) = http(client(&server).fetch_users(DEV).await.unwrap_err());
        assert_eq!(status, 404);
        assert_eq!(
            message,
            "Google Play found no developer account with ID 5374 (HTTP 404): check the \
             developer ID. Google said: Requested entity was not found. [NOT_FOUND] \
             (HTTP 404 on GET developers/5374/users)"
        );

        let warns = logs.messages(LogLevel::Warn, "http");
        assert_eq!(warns.len(), 1, "{warns:?}");
        assert!(
            warns[0].starts_with("GET 127.0.0.1:")
                && warns[0]
                    .contains("/androidpublisher/v3/developers/5374/users → 404 Not Found (")
                && warns[0].ends_with(
                    " ms) [x-goog-request-id: goog-404]; google error: code=404 \
                     status=NOT_FOUND message=\"Requested entity was not found.\""
                ),
            "{}",
            warns[0]
        );
    }

    #[tokio::test]
    async fn google_403_without_admin_is_auth_naming_the_permission_and_logs_no_secret() {
        let server = MockServer::start().await;
        mount_token(&server, "ya29.secret-access-token", 1).await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": {
                    "code": 403,
                    "message": "The caller does not have permission",
                    "status": "PERMISSION_DENIED",
                    "details": [{
                        "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                        "reason": "CALLER_LACKS_PERMISSION",
                        "domain": "androidpublisher.googleapis.com"
                    }]
                }
            })))
            .mount(&server)
            .await;

        let logs = capture_logs();
        let message = match client(&server).fetch_users(DEV).await.unwrap_err() {
            StackError::Auth { message } => message,
            other => panic!("expected Auth, got {other:?}"),
        };
        assert_eq!(
            message,
            format!(
                "The service account {SA_EMAIL} cannot access the developer account 5374. Invite \
                 it in Play Console > Users and permissions with at least \"Admin (all \
                 permissions)\"; new access can take a while to propagate. Google said: The \
                 caller does not have permission [PERMISSION_DENIED] (HTTP 403 on GET \
                 developers/5374/users)"
            )
        );

        let warns = logs.messages(LogLevel::Warn, "http");
        assert_eq!(warns.len(), 1, "{warns:?}");
        assert!(
            warns[0].ends_with(
                "google error: code=403 status=PERMISSION_DENIED message=\"The caller does \
                 not have permission\" details=[ErrorInfo reason=CALLER_LACKS_PERMISSION \
                 domain=androidpublisher.googleapis.com]"
            ),
            "{}",
            warns[0]
        );
        let text = logs.all_text();
        for secret in [
            "ya29.secret-access-token",
            "eyJ",
            "PRIVATE KEY",
            "play-bot@",
        ] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
    }

    #[tokio::test]
    async fn invalid_arguments_send_nothing() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 0).await;
        let client = client(&server);

        for developer in ["", "abc", "12/3"] {
            let err = client.fetch_users(developer).await.unwrap_err();
            assert!(
                matches!(err, StackError::Http { status: 404, .. }),
                "{developer:?}"
            );
        }
        for email in ["", "ana", "@x", "ana@", "a na@x.com", "a/b@x.com", ".."] {
            let err = client.delete_user(DEV, email).await.unwrap_err();
            assert!(
                matches!(err, StackError::Http { status: 400, .. }),
                "{email:?}"
            );
        }
        let err = client
            .set_grant(DEV, EMAIL, "not-a-package", &[])
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 400, .. }));
        let err = client
            .invite_user(
                DEV,
                EMAIL,
                &[],
                &[PlayGrantInfo {
                    package_name: "..".into(),
                    app_level_permissions: vec![],
                }],
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Http { status: 400, .. }));

        assert!(server.received_requests().await.unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // invite / update / delete
    // -----------------------------------------------------------------------

    fn created_user() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "developers/5374/users/ana@example.com",
            "email": "ana@example.com",
            "accessState": "INVITED",
            "developerAccountPermissions": ["CAN_VIEW_NON_FINANCIAL_DATA_GLOBAL"]
        }))
    }

    #[tokio::test]
    async fn invite_creates_the_user_then_each_grant() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("POST"))
            .and(path(USERS_PATH))
            .and(body_json(serde_json::json!({
                "name": "developers/5374/users/ana@example.com",
                "email": "ana@example.com",
                "developerAccountPermissions": ["CAN_VIEW_NON_FINANCIAL_DATA_GLOBAL"],
                "expirationTime": "2027-01-01T00:00:00Z"
            })))
            .respond_with(created_user())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{USERS_PATH}/{EMAIL}/grants")))
            .and(body_json(serde_json::json!({
                "name": "developers/5374/users/ana@example.com/grants/com.example.app",
                "packageName": "com.example.app",
                "appLevelPermissions": ["CAN_REPLY_TO_REVIEWS"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "packageName": "com.example.app",
                "appLevelPermissions": ["CAN_REPLY_TO_REVIEWS"]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let user = client(&server)
            .invite_user(
                DEV,
                EMAIL,
                &strings(&["CAN_VIEW_NON_FINANCIAL_DATA_GLOBAL"]),
                &[PlayGrantInfo {
                    package_name: "com.example.app".into(),
                    app_level_permissions: strings(&["CAN_REPLY_TO_REVIEWS"]),
                }],
                Some("2027-01-01T00:00:00Z"),
            )
            .await
            .unwrap();
        assert_eq!(user.email, EMAIL);
        assert_eq!(user.access_state.as_deref(), Some("INVITED"));
        assert_eq!(
            user.grants,
            vec![PlayGrantInfo {
                package_name: "com.example.app".into(),
                app_level_permissions: strings(&["CAN_REPLY_TO_REVIEWS"]),
            }]
        );
    }

    #[tokio::test]
    async fn invite_without_expiry_or_grants_sends_neither() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("POST"))
            .and(path(USERS_PATH))
            .and(body_json(serde_json::json!({
                "name": "developers/5374/users/ana@example.com",
                "email": "ana@example.com",
                "developerAccountPermissions": []
            })))
            .respond_with(created_user())
            .expect(1)
            .mount(&server)
            .await;

        let user = client(&server)
            .invite_user(DEV, EMAIL, &[], &[], None)
            .await
            .unwrap();
        assert_eq!(
            user.developer_account_permissions,
            strings(&["CAN_VIEW_NON_FINANCIAL_DATA_GLOBAL"])
        );
        assert_eq!(api_calls(&server).await, vec![format!("POST {USERS_PATH}")]);
    }

    #[tokio::test]
    async fn failed_grant_removes_the_invitation_and_its_error_wins() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("POST"))
            .and(path(USERS_PATH))
            .respond_with(created_user())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{USERS_PATH}/{EMAIL}/grants")))
            .respond_with(google_error(400, "Unknown package com.example.gone."))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(user_path(EMAIL)))
            .respond_with(google_error(503, "later"))
            .expect(1)
            .mount(&server)
            .await;

        let logger = Arc::new(CapturingLogger::default());
        let err = client(&server)
            .with_debug_logger(Some(logger.clone()))
            .invite_user(
                DEV,
                EMAIL,
                &[],
                &[PlayGrantInfo {
                    package_name: "com.example.gone".into(),
                    app_level_permissions: vec![],
                }],
                None,
            )
            .await
            .unwrap_err();
        match err {
            StackError::Http { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(
                    message,
                    "Unknown package com.example.gone. [X] \
                     (HTTP 400 on POST developers/5374/users/a***@example.com/grants)"
                );
            }
            other => panic!("expected the grant's Http 400, got {other:?}"),
        }
        assert!(logger
            .messages()
            .last()
            .unwrap()
            .contains("could not remove the invitation of ana@example.com"));
    }

    #[tokio::test]
    async fn update_patches_both_fields_with_the_mask() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("PATCH"))
            .and(path(user_path(EMAIL)))
            .and(query_param(
                "updateMask",
                "developerAccountPermissions,expirationTime",
            ))
            // No expiry in the body, but in the mask: Google clears it.
            .and(body_json(serde_json::json!({
                "name": "developers/5374/users/ana@example.com",
                "developerAccountPermissions": ["CAN_REPLY_TO_REVIEWS_GLOBAL"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "email": "ana@example.com",
                "developerAccountPermissions": ["CAN_REPLY_TO_REVIEWS_GLOBAL"]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let user = client(&server)
            .update_user(DEV, EMAIL, &strings(&["CAN_REPLY_TO_REVIEWS_GLOBAL"]), None)
            .await
            .unwrap();
        assert_eq!(user.expiration_time, None);
        assert_eq!(
            user.developer_account_permissions,
            strings(&["CAN_REPLY_TO_REVIEWS_GLOBAL"])
        );
    }

    #[tokio::test]
    async fn update_sends_the_expiry_when_set() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("PATCH"))
            .and(path(user_path(EMAIL)))
            .and(body_json(serde_json::json!({
                "name": "developers/5374/users/ana@example.com",
                "developerAccountPermissions": [],
                "expirationTime": "2030-05-01T12:00:00Z"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "email": "ana@example.com", "expirationTime": "2030-05-01T12:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let user = client(&server)
            .update_user(DEV, EMAIL, &[], Some("2030-05-01T12:00:00Z"))
            .await
            .unwrap();
        assert_eq!(
            user.expiration_time.as_deref(),
            Some("2030-05-01T12:00:00Z")
        );
    }

    #[tokio::test]
    async fn update_of_an_unknown_user_keeps_googles_404() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("PATCH"))
            .and(path(user_path(EMAIL)))
            .respond_with(google_error(404, "User not found."))
            .mount(&server)
            .await;

        match client(&server)
            .update_user(DEV, EMAIL, &[], None)
            .await
            .unwrap_err()
        {
            StackError::Http { status, message } => {
                assert_eq!(status, 404);
                assert_eq!(
                    message,
                    "User not found. [X] (HTTP 404 on PATCH developers/5374/users/a***@example.com)"
                );
            }
            other => panic!("expected Http 404, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_user_and_grant_call_their_resources() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("DELETE"))
            .and(path(user_path("bot@p.iam.gserviceaccount.com")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(grant_path(EMAIL, "com.example.app")))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let client = client(&server);
        client
            .delete_user(DEV, "bot@p.iam.gserviceaccount.com")
            .await
            .unwrap();
        client
            .delete_grant(DEV, EMAIL, "com.example.app")
            .await
            .unwrap();
    }

    // -----------------------------------------------------------------------
    // set_grant
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn set_grant_patches_an_existing_grant() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("PATCH"))
            .and(path(grant_path(EMAIL, "com.example.app")))
            .and(query_param("updateMask", "appLevelPermissions"))
            .and(body_json(serde_json::json!({
                "name": "developers/5374/users/ana@example.com/grants/com.example.app",
                "appLevelPermissions": ["CAN_MANAGE_TRACK_APKS", "CAN_VIEW_NON_FINANCIAL_DATA"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "developers/5374/users/ana@example.com/grants/com.example.app",
                "packageName": "com.example.app",
                "appLevelPermissions": ["CAN_MANAGE_TRACK_APKS", "CAN_VIEW_NON_FINANCIAL_DATA"]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let grant = client(&server)
            .set_grant(
                DEV,
                EMAIL,
                "com.example.app",
                &strings(&["CAN_MANAGE_TRACK_APKS", "CAN_VIEW_NON_FINANCIAL_DATA"]),
            )
            .await
            .unwrap();
        assert_eq!(grant.package_name, "com.example.app");
        assert_eq!(grant.app_level_permissions.len(), 2);
        assert_eq!(api_calls(&server).await.len(), 1);
    }

    #[tokio::test]
    async fn set_grant_creates_when_the_patch_finds_none() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("PATCH"))
            .and(path(grant_path(EMAIL, "com.example.app")))
            .respond_with(google_error(404, "Grant not found."))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{USERS_PATH}/{EMAIL}/grants")))
            .and(body_json(serde_json::json!({
                "name": "developers/5374/users/ana@example.com/grants/com.example.app",
                "packageName": "com.example.app",
                "appLevelPermissions": ["CAN_REPLY_TO_REVIEWS"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "packageName": "com.example.app",
                "appLevelPermissions": ["CAN_REPLY_TO_REVIEWS"]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let grant = client(&server)
            .set_grant(
                DEV,
                EMAIL,
                "com.example.app",
                &strings(&["CAN_REPLY_TO_REVIEWS"]),
            )
            .await
            .unwrap();
        assert_eq!(
            grant.app_level_permissions,
            strings(&["CAN_REPLY_TO_REVIEWS"])
        );
    }

    #[tokio::test]
    async fn set_grant_other_errors_do_not_fall_back() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("PATCH"))
            .and(path(grant_path(EMAIL, "com.example.app")))
            .respond_with(google_error(403, "nope"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{USERS_PATH}/{EMAIL}/grants")))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let err = client(&server)
            .set_grant(DEV, EMAIL, "com.example.app", &[])
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Auth { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn malformed_responses_are_decode() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("PATCH"))
            .and(path(user_path(EMAIL)))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(USERS_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"users": 1}"#))
            .mount(&server)
            .await;

        let client = client(&server);
        match client.update_user(DEV, EMAIL, &[], None).await.unwrap_err() {
            StackError::Decode { message } => assert!(message.contains("user without an e-mail")),
            other => panic!("expected Decode, got {other:?}"),
        }
        match client.fetch_users(DEV).await.unwrap_err() {
            StackError::Decode { message } => assert!(message.contains("users.list response")),
            other => panic!("expected Decode, got {other:?}"),
        }
    }
}
