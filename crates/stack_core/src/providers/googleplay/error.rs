//! Maps Google API error responses (AIP-193 `{"error": {...}}` bodies) to
//! [`StackError`].
//!
//! Classification reads only the HTTP status and the machine-readable `reason`
//! fields — `google.rpc.ErrorInfo` entries in `error.details[]`, plus the legacy
//! `error.errors[].reason` — never the human `message`, whose wording Google may
//! change at any time.
//!
//! Both Google APIs the provider calls share this mapping; an [`ErrorHints`]
//! value names the API, the Play Console permission the call needs and, for
//! calls scoped to one app, the package a 404 refers to.

use serde::Deserialize;
use serde_json::Value;

use crate::error::StackError;

/// The API is not enabled in the service account's Google Cloud project.
const REASON_SERVICE_DISABLED: &str = "SERVICE_DISABLED";
/// The access token was minted without a scope this call needs.
const REASON_SCOPE_INSUFFICIENT: &str = "ACCESS_TOKEN_SCOPE_INSUFFICIENT";
/// Legacy (`errors[].reason`) spelling of [`REASON_SERVICE_DISABLED`].
const LEGACY_ACCESS_NOT_CONFIGURED: &str = "accessNotConfigured";
/// Legacy reason the Android Publisher API sends with a **401** ("The current
/// user has insufficient permissions to perform the requested operation.") when
/// the service account has no Play Console access to the app.
const LEGACY_PERMISSION_DENIED: &str = "permissionDenied";
/// `@type` suffix of the structured error detail that carries `reason`.
const ERROR_INFO_TYPE_SUFFIX: &str = "google.rpc.ErrorInfo";

/// The Play Console permission every read needs.
pub(super) const PERMISSION_VIEW_APP_INFO: &str = "View app information (read-only)";
/// The Play Console permission replying to reviews needs.
pub(super) const PERMISSION_REPLY_TO_REVIEWS: &str = "Reply to reviews";

/// The Google API a request went to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GoogleApi {
    /// Play Developer Reporting API (`apps:search`).
    Reporting,
    /// Google Play Android Developer API (`androidpublisher` v3).
    Publisher,
}

impl GoogleApi {
    /// The API's console name; used when a `SERVICE_DISABLED` detail names no
    /// service.
    fn title(self) -> &'static str {
        match self {
            Self::Reporting => "Google Play Developer Reporting API",
            Self::Publisher => "Google Play Android Developer API",
        }
    }
}

/// What a call addressed, which decides how a 404 and "no access" read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target<'a> {
    /// Nothing scoped to one app (`apps:search`).
    Account,
    /// The app itself (listing its reviews, opening an edit): a 404 means
    /// Google Play has no app with this package name.
    App(&'a str),
    /// A resource inside the app (a review, an open edit): a 404 may mean that
    /// resource is gone, so Google's own message is kept as is.
    AppResource(&'a str),
}

impl<'a> Target<'a> {
    /// The package the call is scoped to, if any.
    fn package(self) -> Option<&'a str> {
        match self {
            Self::Account => None,
            Self::App(package) | Self::AppResource(package) => Some(package),
        }
    }
}

/// What a failed call was about, so [`map_google_error`] can say what to
/// fix. It only changes the wording, never the [`StackError`] variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ErrorHints<'a> {
    /// The API the call went to.
    pub(super) api: GoogleApi,
    /// The Play Console permission the call needs, named in "no access" hints.
    pub(super) permission: &'a str,
    /// What the call addressed.
    pub(super) target: Target<'a>,
}

impl ErrorHints<'_> {
    /// `apps:search` on the Reporting API.
    pub(super) const REPORTING: ErrorHints<'static> = ErrorHints {
        api: GoogleApi::Reporting,
        permission: PERMISSION_VIEW_APP_INFO,
        target: Target::Account,
    };
}

#[derive(Deserialize)]
struct GoogleErrorEnvelope {
    error: GoogleErrorBody,
}

#[derive(Deserialize)]
struct GoogleErrorBody {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    status: Option<String>,
    /// Left untyped: `Help`, `LocalizedMessage` and other detail kinds have
    /// shapes of their own and must not break the parse.
    #[serde(default)]
    details: Vec<Value>,
    #[serde(default)]
    errors: Vec<LegacyErrorItem>,
}

#[derive(Deserialize)]
struct LegacyErrorItem {
    #[serde(default)]
    reason: Option<String>,
}

impl GoogleErrorBody {
    /// Whether a legacy `errors[]` item carries `reason`.
    fn has_legacy_reason(&self, reason: &str) -> bool {
        self.errors
            .iter()
            .any(|item| item.reason.as_deref() == Some(reason))
    }
}

/// A `google.rpc.ErrorInfo` detail, borrowed from the parsed body.
struct ErrorInfo<'a> {
    reason: &'a str,
    metadata: Option<&'a Value>,
}

impl ErrorInfo<'_> {
    /// A non-blank string `metadata` entry.
    fn metadata(&self, key: &str) -> Option<&str> {
        self.metadata?
            .get(key)?
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
    }
}

/// Parses an AIP-193 error body; `None` when the body has another shape.
fn parse_error_body(body: &str) -> Option<GoogleErrorBody> {
    serde_json::from_str::<GoogleErrorEnvelope>(body)
        .ok()
        .map(|envelope| envelope.error)
}

/// The `google.rpc.ErrorInfo` entries of `details`, in order.
fn error_infos(details: &[Value]) -> Vec<ErrorInfo<'_>> {
    details
        .iter()
        .filter(|detail| {
            detail
                .get("@type")
                .and_then(Value::as_str)
                .is_some_and(|t| t.ends_with(ERROR_INFO_TYPE_SUFFIX))
        })
        .filter_map(|detail| {
            Some(ErrorInfo {
                reason: detail.get("reason")?.as_str()?,
                metadata: detail.get("metadata"),
            })
        })
        .collect()
}

/// Google's own explanation: `error.message` (plus ` [status]`) when present,
/// otherwise the trimmed raw body.
fn google_message(parsed: Option<&GoogleErrorBody>, body: &str) -> String {
    fn non_blank(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }

    match parsed.and_then(|e| non_blank(e.message.as_deref())) {
        Some(message) => match parsed.and_then(|e| non_blank(e.status.as_deref())) {
            Some(status) => format!("{message} [{status}]"),
            None => message.to_string(),
        },
        None => body.trim().to_string(),
    }
}

/// Maps a non-2xx Google API response to a [`StackError`].
///
/// - **401** → [`StackError::Auth`]:
///   - legacy `permissionDenied` (how the Android Publisher API reports a
///     service account without access to the app): the "invite it in Play
///     Console" message below;
///   - anything else: worded neutrally, since Google answers 401 both for a bad
///     token and for missing Play Console access.
/// - **403** → [`StackError::Auth`]:
///   - `SERVICE_DISABLED` / legacy `accessNotConfigured`: the API must be
///     enabled in the service account's Cloud project (names the service and
///     its activation URL when Google sends them, else `hints.api`);
///   - `ACCESS_TOKEN_SCOPE_INSUFFICIENT`: the token lacks a scope;
///   - anything else: the service account (`client_email`) must be invited in
///     Play Console with `hints.permission`.
/// - **404** → [`StackError::Http`]; for a [`Target::App`] call the message
///   says Google Play has no app with that package name.
/// - **Anything else** (400, 409, 429, 5xx) → [`StackError::Http`] carrying
///   Google's message: not a credential problem.
pub(super) fn map_google_error(
    status: u16,
    body: &str,
    client_email: &str,
    hints: &ErrorHints<'_>,
) -> StackError {
    let parsed = parse_error_body(body);
    let google_msg = google_message(parsed.as_ref(), body);

    match status {
        401 if parsed
            .as_ref()
            .is_some_and(|e| e.has_legacy_reason(LEGACY_PERMISSION_DENIED)) =>
        {
            no_access(client_email, hints, &google_msg)
        }
        401 => StackError::auth(format!(
            "Google did not accept the service account's authorization (HTTP 401): \
             the access token may be invalid or revoked, or the service account may \
             lack access in Play Console. Google said: {google_msg}"
        )),
        403 => map_forbidden(parsed.as_ref(), &google_msg, client_email, hints),
        404 => StackError::Http {
            status,
            message: match hints.target {
                Target::App(package) => format!(
                    "Google Play found no app with package name {package} (HTTP 404): \
                     check the package name. Google said: {google_msg}"
                ),
                Target::Account | Target::AppResource(_) => google_msg,
            },
        },
        _ => StackError::Http {
            status,
            message: google_msg,
        },
    }
}

/// The 403 branch of [`map_google_error`].
fn map_forbidden(
    parsed: Option<&GoogleErrorBody>,
    google_msg: &str,
    client_email: &str,
    hints: &ErrorHints<'_>,
) -> StackError {
    let infos = parsed.map(|e| error_infos(&e.details)).unwrap_or_default();
    let has_legacy_reason = |reason: &str| parsed.is_some_and(|e| e.has_legacy_reason(reason));

    if let Some(info) = infos.iter().find(|i| i.reason == REASON_SERVICE_DISABLED) {
        return service_disabled(Some(info), hints.api);
    }
    if has_legacy_reason(LEGACY_ACCESS_NOT_CONFIGURED) {
        return service_disabled(None, hints.api);
    }
    if infos.iter().any(|i| i.reason == REASON_SCOPE_INSUFFICIENT) {
        return StackError::auth(format!(
            "The access token is missing a required OAuth scope: {google_msg}"
        ));
    }

    no_access(client_email, hints, google_msg)
}

/// The "invite the service account in Play Console" message, naming the app
/// when the call targets one and the permission the call needs.
fn no_access(client_email: &str, hints: &ErrorHints<'_>, google_msg: &str) -> StackError {
    let subject = match hints.target.package() {
        Some(package) => format!("the app {package}"),
        None => "this Google Play data".to_string(),
    };
    StackError::auth(format!(
        "The service account {client_email} cannot access {subject}. \
         Invite it in Play Console > Users and permissions with at least \"{}\"; \
         new access can take a while to propagate. Google said: {google_msg}",
        hints.permission
    ))
}

/// The "enable the API" message, naming the service and its activation URL
/// when the `ErrorInfo` metadata carries them, else `api`.
fn service_disabled(info: Option<&ErrorInfo<'_>>, api: GoogleApi) -> StackError {
    let title = info
        .and_then(|i| i.metadata("serviceTitle").or_else(|| i.metadata("service")))
        .unwrap_or(api.title());
    let action = match info.and_then(|i| i.metadata("activationUrl")) {
        Some(url) => format!("Enable it at {url}"),
        None => "Enable it in the Google Cloud console".to_string(),
    };
    StackError::auth(format!(
        "{title} is not enabled for this service account's Google Cloud project. \
         {action}, wait a few minutes, then retry."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMAIL: &str = "play-bot@my-project.iam.gserviceaccount.com";

    /// The real 403 Google returned for every Reporting API call of a project
    /// without the API enabled (captured 2026-09-11, see the design notes).
    const SERVICE_DISABLED_BODY: &str = r#"{"error":{"code":403,
 "message":"Google Play Developer Reporting API has not been used in project 764216752430 before or it is disabled. Enable it by visiting https://console.developers.google.com/apis/api/playdeveloperreporting.googleapis.com/overview?project=764216752430 then retry. If you enabled this API recently, wait a few minutes for the action to propagate to our systems and retry.",
 "status":"PERMISSION_DENIED",
 "details":[
  {"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"SERVICE_DISABLED","domain":"googleapis.com",
   "metadata":{"service":"playdeveloperreporting.googleapis.com","consumer":"projects/764216752430",
     "containerInfo":"764216752430","serviceTitle":"Google Play Developer Reporting API",
     "activationUrl":"https://console.developers.google.com/apis/api/playdeveloperreporting.googleapis.com/overview?project=764216752430"}},
  {"@type":"type.googleapis.com/google.rpc.LocalizedMessage","locale":"en-US","message":"Google Play Developer Reporting API has not been used in project 764216752430 before or it is disabled. Enable it by visiting https://console.developers.google.com/apis/api/playdeveloperreporting.googleapis.com/overview?project=764216752430 then retry. If you enabled this API recently, wait a few minutes for the action to propagate to our systems and retry."}]}}"#;

    const ACTIVATION_URL: &str = "https://console.developers.google.com/apis/api/playdeveloperreporting.googleapis.com/overview?project=764216752430";

    /// The mapping for an `apps:search` (Reporting API) failure.
    fn map_reporting_error(status: u16, body: &str, client_email: &str) -> StackError {
        map_google_error(status, body, client_email, &ErrorHints::REPORTING)
    }

    fn auth_message(err: StackError) -> String {
        match err {
            StackError::Auth { message } => message,
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    fn forbidden(details: Value) -> String {
        let body = serde_json::json!({
            "error": {
                "code": 403,
                "message": "The caller does not have permission",
                "status": "PERMISSION_DENIED",
                "details": details
            }
        })
        .to_string();
        auth_message(map_reporting_error(403, &body, EMAIL))
    }

    #[test]
    fn service_disabled_uses_title_and_activation_url() {
        let message = auth_message(map_reporting_error(403, SERVICE_DISABLED_BODY, EMAIL));
        assert_eq!(
            message,
            format!(
                "Google Play Developer Reporting API is not enabled for this service \
                 account's Google Cloud project. Enable it at {ACTIVATION_URL}, wait a \
                 few minutes, then retry."
            )
        );
    }

    #[test]
    fn service_disabled_without_metadata_falls_back() {
        let message = forbidden(serde_json::json!([{
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            "reason": "SERVICE_DISABLED"
        }]));
        assert!(message.starts_with("Google Play Developer Reporting API is not enabled"));
        assert!(message.contains("Enable it in the Google Cloud console"));
    }

    #[test]
    fn service_disabled_falls_back_to_service_name() {
        let message = forbidden(serde_json::json!([{
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            "reason": "SERVICE_DISABLED",
            "metadata": { "service": "androidpublisher.googleapis.com", "serviceTitle": " " }
        }]));
        assert!(message.starts_with("androidpublisher.googleapis.com is not enabled"));
    }

    #[test]
    fn legacy_access_not_configured_is_service_disabled() {
        let body = r#"{"error":{"code":403,"message":"Access Not Configured.",
            "errors":[{"message":"Access Not Configured.","domain":"usageLimits",
            "reason":"accessNotConfigured"}]}}"#;
        let message = auth_message(map_reporting_error(403, body, EMAIL));
        assert!(message.contains("is not enabled for this service account's Google Cloud project"));
    }

    #[test]
    fn scope_insufficient_maps_to_auth() {
        let message = forbidden(serde_json::json!([{
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            "reason": "ACCESS_TOKEN_SCOPE_INSUFFICIENT",
            "domain": "googleapis.com"
        }]));
        assert!(message.starts_with("The access token is missing a required OAuth scope"));
        assert!(message.contains("The caller does not have permission [PERMISSION_DENIED]"));
    }

    #[test]
    fn other_403_mentions_client_email_and_users_and_permissions() {
        let message = forbidden(serde_json::json!([]));
        assert!(message.contains(EMAIL));
        assert!(message.contains("Play Console > Users and permissions"));
        assert!(message.contains("\"View app information (read-only)\""));
        assert!(message.contains("propagate"));
        assert!(message
            .ends_with("Google said: The caller does not have permission [PERMISSION_DENIED]"));
    }

    #[test]
    fn legacy_insufficient_permissions_gets_invite_wording() {
        // `insufficientPermissions` also shows up for plain permission denials,
        // so it must not be read as a scope problem.
        let body = r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.",
            "status":"PERMISSION_DENIED","errors":[{"reason":"insufficientPermissions"}]}}"#;
        let message = auth_message(map_reporting_error(403, body, EMAIL));
        assert!(message.contains("Users and permissions"));
        assert!(!message.contains("missing a required OAuth scope"));
    }

    #[test]
    fn unauthenticated_401_maps_to_auth() {
        let body = r#"{"error":{"code":401,"message":"Request had invalid authentication credentials.","status":"UNAUTHENTICATED"}}"#;
        let message = auth_message(map_reporting_error(401, body, EMAIL));
        assert!(message.contains("HTTP 401"));
        assert!(
            message.contains("Request had invalid authentication credentials. [UNAUTHENTICATED]")
        );
    }

    #[test]
    fn status_429_and_500_map_to_http_with_google_message() {
        for (status, google_status) in [
            (400, "INVALID_ARGUMENT"),
            (404, "NOT_FOUND"),
            (429, "RESOURCE_EXHAUSTED"),
            (500, "INTERNAL"),
        ] {
            let body = format!(
                r#"{{"error":{{"code":{status},"message":"boom","status":"{google_status}"}}}}"#
            );
            match map_reporting_error(status, &body, EMAIL) {
                StackError::Http {
                    status: mapped,
                    message,
                } => {
                    assert_eq!(mapped, status);
                    assert_eq!(message, format!("boom [{google_status}]"));
                }
                other => panic!("expected Http for {status}, got {other:?}"),
            }
        }
    }

    #[test]
    fn non_json_body_uses_raw_text() {
        match map_reporting_error(502, "  <html>Bad Gateway</html>\n", EMAIL) {
            StackError::Http { status, message } => {
                assert_eq!(status, 502);
                assert_eq!(message, "<html>Bad Gateway</html>");
            }
            other => panic!("expected Http, got {other:?}"),
        }
        let message = auth_message(map_reporting_error(403, "Forbidden", EMAIL));
        assert!(message.ends_with("Google said: Forbidden"));
    }

    #[test]
    fn details_with_help_and_localized_message_still_parse() {
        let message = forbidden(serde_json::json!([
            {
                "@type": "type.googleapis.com/google.rpc.Help",
                "links": [{ "description": "Google developers console", "url": "https://console.developers.google.com" }]
            },
            {
                "@type": "type.googleapis.com/google.rpc.LocalizedMessage",
                "locale": "en-US",
                "message": "localized"
            },
            {
                "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                "reason": "SERVICE_DISABLED",
                "metadata": { "serviceTitle": "Some API", "activationUrl": "https://example.test/enable" }
            }
        ]));
        assert!(message.starts_with("Some API is not enabled"));
        assert!(message.contains("Enable it at https://example.test/enable,"));
    }

    #[test]
    fn reason_only_counts_on_an_error_info_detail() {
        // A `reason` on another detail type is not an ErrorInfo reason.
        let message = forbidden(serde_json::json!([{
            "@type": "type.googleapis.com/google.rpc.Help",
            "reason": "SERVICE_DISABLED"
        }]));
        assert!(message.contains("Users and permissions"));
    }

    #[test]
    fn reason_never_inferred_from_message_text() {
        let body = r#"{"error":{"code":403,
            "message":"Google Play Developer Reporting API has not been used in project 1 before or it is disabled.",
            "status":"PERMISSION_DENIED"}}"#;
        let message = auth_message(map_reporting_error(403, body, EMAIL));
        assert!(message.starts_with(&format!("The service account {EMAIL} cannot access")));
        assert!(!message.contains("is not enabled for this service account's"));
    }

    // -----------------------------------------------------------------------
    // Android Publisher hints
    // -----------------------------------------------------------------------

    const PACKAGE: &str = "com.example.app";

    fn publisher(permission: &'static str, target: Target<'static>) -> ErrorHints<'static> {
        ErrorHints {
            api: GoogleApi::Publisher,
            permission,
            target,
        }
    }

    fn http(err: StackError) -> (u16, String) {
        match err {
            StackError::Http { status, message } => (status, message),
            other => panic!("expected Http, got {other:?}"),
        }
    }

    /// The Reporting capture's shape with the Android Publisher service in
    /// its `ErrorInfo` metadata.
    const PUBLISHER_SERVICE_DISABLED_BODY: &str = r#"{"error":{"code":403,
 "message":"Google Play Android Developer API has not been used in project 764216752430 before or it is disabled.",
 "status":"PERMISSION_DENIED",
 "details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"SERVICE_DISABLED","domain":"googleapis.com",
   "metadata":{"service":"androidpublisher.googleapis.com","consumer":"projects/764216752430",
     "serviceTitle":"Google Play Android Developer API",
     "activationUrl":"https://console.developers.google.com/apis/api/androidpublisher.googleapis.com/overview?project=764216752430"}}]}}"#;

    /// How the Android Publisher API answers a service account without access
    /// to the app: 401 with the legacy `permissionDenied` reason.
    const PERMISSION_DENIED_401_BODY: &str = r#"{"error":{"code":401,
 "message":"The current user has insufficient permissions to perform the requested operation.",
 "errors":[{"message":"The current user has insufficient permissions to perform the requested operation.",
   "domain":"androidpublisher","reason":"permissionDenied"}]}}"#;

    #[test]
    fn publisher_service_disabled_names_the_android_developer_api() {
        let hints = publisher(PERMISSION_VIEW_APP_INFO, Target::App(PACKAGE));
        let message = auth_message(map_google_error(
            403,
            PUBLISHER_SERVICE_DISABLED_BODY,
            EMAIL,
            &hints,
        ));
        assert_eq!(
            message,
            "Google Play Android Developer API is not enabled for this service account's \
             Google Cloud project. Enable it at https://console.developers.google.com/apis/\
             api/androidpublisher.googleapis.com/overview?project=764216752430, wait a few \
             minutes, then retry."
        );
    }

    #[test]
    fn legacy_access_not_configured_falls_back_to_the_hinted_api() {
        let body = r#"{"error":{"code":403,"message":"Access Not Configured.",
            "errors":[{"reason":"accessNotConfigured"}]}}"#;

        let publisher_message = auth_message(map_google_error(
            403,
            body,
            EMAIL,
            &publisher(PERMISSION_VIEW_APP_INFO, Target::App(PACKAGE)),
        ));
        assert!(publisher_message.starts_with("Google Play Android Developer API is not enabled"));

        let reporting_message = auth_message(map_reporting_error(403, body, EMAIL));
        assert!(reporting_message.starts_with("Google Play Developer Reporting API is not enabled"));
    }

    #[test]
    fn publisher_403_names_the_app_and_the_needed_permission() {
        let body = r#"{"error":{"code":403,"message":"The caller does not have permission",
            "status":"PERMISSION_DENIED","errors":[{"reason":"forbidden"}]}}"#;
        let message = auth_message(map_google_error(
            403,
            body,
            EMAIL,
            &publisher(PERMISSION_REPLY_TO_REVIEWS, Target::AppResource(PACKAGE)),
        ));
        assert_eq!(
            message,
            format!(
                "The service account {EMAIL} cannot access the app com.example.app. Invite \
                 it in Play Console > Users and permissions with at least \"Reply to \
                 reviews\"; new access can take a while to propagate. Google said: The \
                 caller does not have permission [PERMISSION_DENIED]"
            )
        );
    }

    #[test]
    fn permission_denied_401_reads_as_no_access() {
        let message = auth_message(map_google_error(
            401,
            PERMISSION_DENIED_401_BODY,
            EMAIL,
            &publisher(PERMISSION_VIEW_APP_INFO, Target::App(PACKAGE)),
        ));
        assert!(message.starts_with(&format!(
            "The service account {EMAIL} cannot access the app com.example.app."
        )));
        assert!(message.contains("\"View app information (read-only)\""));
        assert!(message.ends_with(
            "Google said: The current user has insufficient permissions to perform the \
             requested operation."
        ));
        assert!(!message.contains("HTTP 401"));
    }

    #[test]
    fn plain_401_keeps_the_neutral_wording_for_the_publisher_api() {
        let body = r#"{"error":{"code":401,"message":"Request had invalid authentication credentials.","status":"UNAUTHENTICATED"}}"#;
        let message = auth_message(map_google_error(
            401,
            body,
            EMAIL,
            &publisher(PERMISSION_VIEW_APP_INFO, Target::App(PACKAGE)),
        ));
        assert!(message
            .starts_with("Google did not accept the service account's authorization (HTTP 401)"));
    }

    #[test]
    fn app_404_says_the_package_was_not_found() {
        let body = r#"{"error":{"code":404,"message":"Package not found: com.example.app.","status":"NOT_FOUND"}}"#;
        let (status, message) = http(map_google_error(
            404,
            body,
            EMAIL,
            &publisher(PERMISSION_VIEW_APP_INFO, Target::App(PACKAGE)),
        ));
        assert_eq!(status, 404);
        assert_eq!(
            message,
            "Google Play found no app with package name com.example.app (HTTP 404): check \
             the package name. Google said: Package not found: com.example.app. [NOT_FOUND]"
        );
    }

    #[test]
    fn app_resource_404_keeps_googles_message() {
        let body = r#"{"error":{"code":404,"message":"Review not found.","status":"NOT_FOUND"}}"#;
        let (status, message) = http(map_google_error(
            404,
            body,
            EMAIL,
            &publisher(PERMISSION_REPLY_TO_REVIEWS, Target::AppResource(PACKAGE)),
        ));
        assert_eq!(status, 404);
        assert_eq!(message, "Review not found. [NOT_FOUND]");
    }

    #[test]
    fn publisher_other_statuses_stay_http() {
        for status in [400, 409, 429, 500] {
            let body = format!(r#"{{"error":{{"code":{status},"message":"boom"}}}}"#);
            let (mapped, message) = http(map_google_error(
                status,
                &body,
                EMAIL,
                &publisher(PERMISSION_VIEW_APP_INFO, Target::App(PACKAGE)),
            ));
            assert_eq!(mapped, status);
            assert_eq!(message, "boom");
        }
    }
}
