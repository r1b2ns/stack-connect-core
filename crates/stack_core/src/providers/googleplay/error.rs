//! Maps Google API error responses (AIP-193 `{"error": {...}}` bodies) to
//! [`StackError`].
//!
//! Classification reads the HTTP status, `error.status` and the
//! machine-readable `reason` fields — `google.rpc.ErrorInfo` entries in
//! `error.details[]`, plus the legacy `error.errors[].reason` — and not the
//! human `message`, whose wording Google may change at any time, with one
//! exception: a 403 whose message mentions a quota (see [`is_quota_error`]).
//! The Android Publisher API reports some quotas only that way (`403
//! PERMISSION_DENIED` "Listing releases quota exceeded.", no `details[]`), and
//! reading it as missing access would send the user to fix permissions that
//! are fine.
//!
//! Both Google APIs the provider calls share this mapping; an [`ErrorHints`]
//! value names the API, the Play Console permission the call needs and, for
//! calls scoped to one app, the package a 404 refers to.

use std::fmt::Display;

use serde::Deserialize;
use serde_json::Value;

use crate::error::StackError;
use crate::logging::{self, http::RequestInfo, target};

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
/// `ErrorInfo.reason` of a per-minute or per-user rate limit.
const REASON_RATE_LIMIT_EXCEEDED: &str = "RATE_LIMIT_EXCEEDED";
/// `error.status` (`google.rpc.Code`) of an exhausted quota or rate limit.
const STATUS_RESOURCE_EXHAUSTED: &str = "RESOURCE_EXHAUSTED";
/// Legacy (`errors[].reason`, domain `usageLimits`) quota and rate-limit
/// reasons, which come with a 403.
const LEGACY_QUOTA_REASONS: [&str; 3] = [
    "userRateLimitExceeded",
    "rateLimitExceeded",
    "quotaExceeded",
];
/// What a quota message contains, matched ASCII case-insensitively.
const QUOTA_WORD: &str = "quota";
/// The status a quota or rate limit is reported with, whatever Google
/// answered (403 or 429): "too many requests", not "no access".
const QUOTA_STATUS: u16 = 429;

/// The Play Console permission every read needs.
pub(super) const PERMISSION_VIEW_APP_INFO: &str = "View app information (read-only)";
/// The Play Console permission replying to reviews needs.
pub(super) const PERMISSION_REPLY_TO_REVIEWS: &str = "Reply to reviews";
/// The Play Console permission changing the production track needs.
pub(super) const PERMISSION_RELEASE_PRODUCTION: &str =
    "Release to production, exclude devices, and use app signing by Google Play";
/// The Play Console permission changing a testing track needs.
pub(super) const PERMISSION_RELEASE_TESTING: &str = "Release to testing tracks";
/// The Play Console permission managing users and their access needs.
pub(super) const PERMISSION_ADMIN: &str = "Admin (all permissions)";

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
    /// A Play developer account itself (listing or inviting its users): a 404
    /// means Google Play has no developer account with this ID.
    Developer(&'a str),
    /// A resource inside a developer account (a user, a grant): a 404 may mean
    /// that resource is gone, so Google's own message is kept as is.
    DeveloperResource(&'a str),
}

impl Target<'_> {
    /// What the call is scoped to, as "no access" messages name it.
    fn subject(self) -> Option<String> {
        match self {
            Self::Account => None,
            Self::App(package) | Self::AppResource(package) => Some(format!("the app {package}")),
            Self::Developer(id) | Self::DeveloperResource(id) => {
                Some(format!("the developer account {id}"))
            }
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

/// Who authorized a failed call, so [`map_google_error`] can say whose access
/// is missing. It only changes the wording, never the [`StackError`] variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Principal<'a> {
    /// A service account, by its `client_email` (not secret: it is what a Play
    /// Console admin invites).
    ServiceAccount(&'a str),
    /// The user's own Google login ("Sign in with Google"), whose access token
    /// the host supplies. Not named: the host's account id may be a personal
    /// e-mail address, and an error message is shown and logged by the host.
    SignedInUser,
}

impl Principal<'_> {
    /// How log lines and messages name the principal.
    pub(super) fn describe(self) -> String {
        match self {
            Self::ServiceAccount(email) => email.to_string(),
            Self::SignedInUser => "the signed-in Google account".to_string(),
        }
    }
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
/// - **A quota or rate limit** ([`is_quota_error`]: any 429, or a 403 that
///   says so) → [`StackError::Http`] `429`, whatever status Google sent:
///   Google's message first, then that it is a quota and not a permission
///   problem, and `retry_after_secs` (the `Retry-After` header) when known.
///   Logged as `Warn` under `googleplay`, starting with `quota:`.
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
///   says Google Play has no app with that package name, for a
///   [`Target::Developer`] call no developer account with that ID.
/// - **Anything else** (400, 409, 5xx) → [`StackError::Http`] carrying
///   Google's message: not a credential problem.
pub(super) fn map_google_error(
    status: u16,
    body: &str,
    retry_after_secs: Option<u64>,
    principal: Principal<'_>,
    hints: &ErrorHints<'_>,
) -> StackError {
    let parsed = parse_error_body(body);
    let google_msg = google_message(parsed.as_ref(), body);

    if is_quota_error(status, parsed.as_ref()) {
        return quota_exceeded(status, &google_msg, retry_after_secs);
    }

    match status {
        401 if parsed
            .as_ref()
            .is_some_and(|e| e.has_legacy_reason(LEGACY_PERMISSION_DENIED)) =>
        {
            no_access(principal, hints, &google_msg)
        }
        401 => unauthorized(principal, &google_msg),
        403 => map_forbidden(parsed.as_ref(), &google_msg, principal, hints),
        404 => StackError::Http {
            status,
            message: match hints.target {
                Target::App(package) => format!(
                    "Google Play found no app with package name {package} (HTTP 404): \
                     check the package name. Google said: {google_msg}"
                ),
                Target::Developer(id) => format!(
                    "Google Play found no developer account with ID {id} (HTTP 404): \
                     check the developer ID. Google said: {google_msg}"
                ),
                Target::Account | Target::AppResource(_) | Target::DeveloperResource(_) => {
                    google_msg
                }
            },
        },
        _ => StackError::Http {
            status,
            message: google_msg,
        },
    }
}

/// Appends what was called to a [`map_google_error`] result, so the error
/// alone says the request reached Google, what it answered and which
/// resource it was: `… Google said: Not found. [NOT_FOUND] (HTTP 404 on GET
/// developers/5374/users)`. Google's own text stays first and verbatim.
/// Only [`StackError::Http`] and [`StackError::Auth`] carry Google's answer;
/// other variants are returned unchanged.
pub(super) fn with_request_context(
    err: StackError,
    status: u16,
    request: &RequestInfo,
) -> StackError {
    let context = format!(" (HTTP {status} on {})", request.resource());
    match err {
        StackError::Http { status, message } => StackError::Http {
            status,
            message: message + &context,
        },
        StackError::Auth { message } => StackError::Auth {
            message: message + &context,
        },
        other => other,
    }
}

/// A request the core refused to send because an argument cannot be valid:
/// [`StackError::Http`] with `status` (what Google answers for the same
/// mistake, so hosts handle both in one place), a message that starts with
/// `local validation failed:` and ends with `request not sent to Google`, and
/// a `Warn` line under `target`. `detail` must not carry an unmasked e-mail.
pub(super) fn local_rejection(status: u16, target: &str, detail: impl Display) -> StackError {
    let message = format!("local validation failed: {detail}; request not sent to Google");
    logging::warn(target, || message.clone());
    StackError::Http { status, message }
}

/// Whether a response reports an exhausted quota or rate limit rather than
/// missing access or a bad request:
///
/// - any **429** (`RESOURCE_EXHAUSTED`);
/// - a **403** with `error.status` `RESOURCE_EXHAUSTED`, an `ErrorInfo` reason
///   `RATE_LIMIT_EXCEEDED`, or a legacy `userRateLimitExceeded` /
///   `rateLimitExceeded` / `quotaExceeded` reason;
/// - a **403** whose `error.message` contains "quota" (any case), the only
///   sign some Android Publisher quotas give (`PERMISSION_DENIED` "Listing
///   releases quota exceeded."). A structured reason [`map_forbidden`] reads
///   (`SERVICE_DISABLED`, `ACCESS_TOKEN_SCOPE_INSUFFICIENT`, legacy
///   `accessNotConfigured`) wins over the wording, so a "requires a quota
///   project" message about a disabled API is not read as a quota.
///
/// Other statuses are never a quota here: a 400 that mentions one is a bad
/// request.
fn is_quota_error(status: u16, parsed: Option<&GoogleErrorBody>) -> bool {
    match status {
        429 => true,
        403 => parsed.is_some_and(|e| {
            let infos = error_infos(&e.details);
            let has_info = |reason: &str| infos.iter().any(|i| i.reason == reason);

            let structured_quota = e.status.as_deref().map(str::trim)
                == Some(STATUS_RESOURCE_EXHAUSTED)
                || has_info(REASON_RATE_LIMIT_EXCEEDED)
                || LEGACY_QUOTA_REASONS
                    .iter()
                    .any(|reason| e.has_legacy_reason(reason));
            let structured_other = has_info(REASON_SERVICE_DISABLED)
                || has_info(REASON_SCOPE_INSUFFICIENT)
                || e.has_legacy_reason(LEGACY_ACCESS_NOT_CONFIGURED);
            let worded_quota = e
                .message
                .as_deref()
                .is_some_and(|m| m.to_ascii_lowercase().contains(QUOTA_WORD));

            structured_quota || (worded_quota && !structured_other)
        }),
        _ => false,
    }
}

/// The quota branch of [`map_google_error`]: [`StackError::Http`]
/// [`QUOTA_STATUS`] with Google's message first, verbatim, then
/// `; this is a Google Play API quota/rate limit, not a permission problem —
/// wait and retry later`, plus ` (Retry-After: {n} s)` when Google sent the
/// header. Logged as `Warn` under `googleplay` with a `quota:` prefix and the
/// status Google actually answered.
fn quota_exceeded(status: u16, google_msg: &str, retry_after_secs: Option<u64>) -> StackError {
    let retry_after = retry_after_secs
        .map(|secs| format!(" (Retry-After: {secs} s)"))
        .unwrap_or_default();
    let message = format!(
        "{google_msg}; this is a Google Play API quota/rate limit, not a permission \
         problem — wait and retry later{retry_after}"
    );
    logging::warn(target::PLAY, || {
        format!("quota: Google answered HTTP {status} with a quota/rate limit: {message}")
    });
    StackError::Http {
        status: QUOTA_STATUS,
        message,
    }
}

/// The 403 branch of [`map_google_error`].
fn map_forbidden(
    parsed: Option<&GoogleErrorBody>,
    google_msg: &str,
    principal: Principal<'_>,
    hints: &ErrorHints<'_>,
) -> StackError {
    let infos = parsed.map(|e| error_infos(&e.details)).unwrap_or_default();
    let has_legacy_reason = |reason: &str| parsed.is_some_and(|e| e.has_legacy_reason(reason));

    if let Some(info) = infos.iter().find(|i| i.reason == REASON_SERVICE_DISABLED) {
        return service_disabled(Some(info), hints.api, principal);
    }
    if has_legacy_reason(LEGACY_ACCESS_NOT_CONFIGURED) {
        return service_disabled(None, hints.api, principal);
    }
    if infos.iter().any(|i| i.reason == REASON_SCOPE_INSUFFICIENT) {
        return StackError::auth(match principal {
            Principal::ServiceAccount(_) => {
                format!("The access token is missing a required OAuth scope: {google_msg}")
            }
            Principal::SignedInUser => format!(
                "The Google sign-in did not grant access to Google Play: sign in with \
                 Google again and allow the Google Play permissions. Google said: \
                 {google_msg}"
            ),
        });
    }

    no_access(principal, hints, google_msg)
}

/// The plain 401 branch of [`map_google_error`]: worded neutrally, since Google
/// answers 401 both for a bad token and for missing Play Console access.
fn unauthorized(principal: Principal<'_>, google_msg: &str) -> StackError {
    StackError::auth(match principal {
        Principal::ServiceAccount(_) => format!(
            "Google did not accept the service account's authorization (HTTP 401): \
             the access token may be invalid or revoked, or the service account may \
             lack access in Play Console. Google said: {google_msg}"
        ),
        Principal::SignedInUser => format!(
            "Google did not accept the signed-in Google account's authorization \
             (HTTP 401): the sign-in may have expired or been revoked (sign in with \
             Google again), or the account may lack access in Play Console. Google \
             said: {google_msg}"
        ),
    })
}

/// The "invite the service account in Play Console" message, naming the app
/// when the call targets one and the permission the call needs.
fn no_access(principal: Principal<'_>, hints: &ErrorHints<'_>, google_msg: &str) -> StackError {
    let subject = hints
        .target
        .subject()
        .unwrap_or_else(|| "this Google Play data".to_string());
    StackError::auth(match principal {
        Principal::ServiceAccount(client_email) => format!(
            "The service account {client_email} cannot access {subject}. \
             Invite it in Play Console > Users and permissions with at least \"{}\"; \
             new access can take a while to propagate. Google said: {google_msg}",
            hints.permission
        ),
        Principal::SignedInUser => format!(
            "The signed-in Google account cannot access {subject}. \
             Ask an admin to grant it at least \"{}\" in Play Console > Users and \
             permissions; new access can take a while to propagate. Google said: \
             {google_msg}",
            hints.permission
        ),
    })
}

/// The "enable the API" message, naming the service and its activation URL
/// when the `ErrorInfo` metadata carries them, else `api`.
fn service_disabled(
    info: Option<&ErrorInfo<'_>>,
    api: GoogleApi,
    principal: Principal<'_>,
) -> StackError {
    let title = info
        .and_then(|i| i.metadata("serviceTitle").or_else(|| i.metadata("service")))
        .unwrap_or(api.title());
    let action = match info.and_then(|i| i.metadata("activationUrl")) {
        Some(url) => format!("Enable it at {url}"),
        None => "Enable it in the Google Cloud console".to_string(),
    };
    let project = match principal {
        Principal::ServiceAccount(_) => "this service account's Google Cloud project",
        Principal::SignedInUser => "the Google Cloud project this Google sign-in uses",
    };
    StackError::auth(format!(
        "{title} is not enabled for {project}. {action}, wait a few minutes, then retry."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMAIL: &str = "play-bot@my-project.iam.gserviceaccount.com";
    const SA: Principal<'static> = Principal::ServiceAccount(EMAIL);

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
        map_google_error(
            status,
            body,
            None,
            Principal::ServiceAccount(client_email),
            &ErrorHints::REPORTING,
        )
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
    fn other_statuses_map_to_http_with_google_message() {
        // 429 is a quota, covered by the quota tests below.
        for (status, google_status) in [
            (400, "INVALID_ARGUMENT"),
            (404, "NOT_FOUND"),
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
            None,
            SA,
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
            None,
            SA,
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
            None,
            SA,
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
            None,
            SA,
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
            None,
            SA,
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
            None,
            SA,
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
            None,
            SA,
            &publisher(PERMISSION_REPLY_TO_REVIEWS, Target::AppResource(PACKAGE)),
        ));
        assert_eq!(status, 404);
        assert_eq!(message, "Review not found. [NOT_FOUND]");
    }

    #[test]
    fn publisher_other_statuses_stay_http() {
        for status in [400, 409, 500] {
            let body = format!(r#"{{"error":{{"code":{status},"message":"boom"}}}}"#);
            let (mapped, message) = http(map_google_error(
                status,
                &body,
                None,
                SA,
                &publisher(PERMISSION_VIEW_APP_INFO, Target::App(PACKAGE)),
            ));
            assert_eq!(mapped, status);
            assert_eq!(message, "boom");
        }
    }

    #[test]
    fn developer_404_says_the_developer_account_was_not_found() {
        let body =
            r#"{"error":{"code":404,"message":"Developer not found.","status":"NOT_FOUND"}}"#;
        let (status, message) = http(map_google_error(
            404,
            body,
            None,
            SA,
            &publisher(PERMISSION_ADMIN, Target::Developer("1234")),
        ));
        assert_eq!(status, 404);
        assert_eq!(
            message,
            "Google Play found no developer account with ID 1234 (HTTP 404): check the \
             developer ID. Google said: Developer not found. [NOT_FOUND]"
        );

        let (_, message) = http(map_google_error(
            404,
            body,
            None,
            SA,
            &publisher(PERMISSION_ADMIN, Target::DeveloperResource("1234")),
        ));
        assert_eq!(message, "Developer not found. [NOT_FOUND]");
    }

    #[test]
    fn developer_403_names_the_account_and_admin_and_keeps_googles_message() {
        let body = r#"{"error":{"code":403,"message":"The caller does not have the CAN_MANAGE_PERMISSIONS_GLOBAL permission.","status":"PERMISSION_DENIED"}}"#;
        let message = auth_message(map_google_error(
            403,
            body,
            None,
            SA,
            &publisher(PERMISSION_ADMIN, Target::DeveloperResource("1234")),
        ));
        assert_eq!(
            message,
            format!(
                "The service account {EMAIL} cannot access the developer account 1234. \
                 Invite it in Play Console > Users and permissions with at least \"Admin \
                 (all permissions)\"; new access can take a while to propagate. Google \
                 said: The caller does not have the CAN_MANAGE_PERMISSIONS_GLOBAL \
                 permission. [PERMISSION_DENIED]"
            )
        );
    }

    fn users_request() -> RequestInfo {
        RequestInfo::new(
            reqwest::Method::GET,
            reqwest::Url::parse(
                "https://androidpublisher.googleapis.com/androidpublisher/v3/developers/5374/users?pageSize=-1",
            )
            .unwrap(),
        )
    }

    #[test]
    fn request_context_is_appended_to_googles_answer_only() {
        let request = users_request();
        match with_request_context(
            StackError::Http {
                status: 404,
                message: "Not found. [NOT_FOUND]".into(),
            },
            404,
            &request,
        ) {
            StackError::Http { status, message } => {
                assert_eq!(status, 404);
                assert_eq!(
                    message,
                    "Not found. [NOT_FOUND] (HTTP 404 on GET developers/5374/users)"
                );
            }
            other => panic!("expected Http, got {other:?}"),
        }
        match with_request_context(StackError::auth("denied"), 403, &request) {
            StackError::Auth { message } => {
                assert_eq!(message, "denied (HTTP 403 on GET developers/5374/users)")
            }
            other => panic!("expected Auth, got {other:?}"),
        }
        // Not Google's answer: untouched.
        match with_request_context(StackError::decode("bad json"), 200, &request) {
            StackError::Decode { message } => assert_eq!(message, "bad json"),
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[test]
    fn local_rejection_is_worded_as_local_and_logged_as_a_warning() {
        let logs = crate::logging::testing::capture_logs();
        match local_rejection(400, "test.target", "x is wrong") {
            StackError::Http { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(
                    message,
                    "local validation failed: x is wrong; request not sent to Google"
                );
            }
            other => panic!("expected Http, got {other:?}"),
        }
        assert_eq!(
            logs.messages(crate::logging::LogLevel::Warn, "test.target"),
            ["local validation failed: x is wrong; request not sent to Google"]
        );
    }

    // -----------------------------------------------------------------------
    // Quotas and rate limits
    // -----------------------------------------------------------------------

    /// The 403 `tracks.releases.list` answered on a real account (captured
    /// 2026-11): no `details[]`, nothing but the message says "quota".
    const RELEASES_QUOTA_BODY: &str = r#"{"error":{"code":403,
 "message":"Listing releases quota exceeded.","status":"PERMISSION_DENIED"}}"#;

    const QUOTA_NOTE: &str = "; this is a Google Play API quota/rate limit, not a permission \
                              problem — wait and retry later";

    fn app_hints() -> ErrorHints<'static> {
        publisher(PERMISSION_VIEW_APP_INFO, Target::AppResource(PACKAGE))
    }

    #[test]
    fn worded_403_quota_is_http_429_not_auth() {
        let logs = crate::logging::testing::capture_logs();
        let (status, message) = http(map_google_error(
            403,
            RELEASES_QUOTA_BODY,
            None,
            SA,
            &app_hints(),
        ));
        assert_eq!(status, 429);
        assert_eq!(
            message,
            format!("Listing releases quota exceeded. [PERMISSION_DENIED]{QUOTA_NOTE}")
        );
        assert!(!message.contains(EMAIL));
        assert!(!message.contains("Users and permissions"));
        assert_eq!(
            logs.messages(crate::logging::LogLevel::Warn, "googleplay"),
            [format!(
                "quota: Google answered HTTP 403 with a quota/rate limit: Listing releases \
                 quota exceeded. [PERMISSION_DENIED]{QUOTA_NOTE}"
            )]
        );
    }

    #[test]
    fn quota_word_matches_in_any_case() {
        let body = r#"{"error":{"code":403,"message":"QUOTA Exceeded for users.list","status":"PERMISSION_DENIED"}}"#;
        let (status, _) = http(map_google_error(
            403,
            body,
            None,
            SA,
            &publisher(PERMISSION_ADMIN, Target::Developer("5374")),
        ));
        assert_eq!(status, 429);
    }

    #[test]
    fn retry_after_seconds_are_named() {
        let (status, message) = http(map_google_error(
            403,
            RELEASES_QUOTA_BODY,
            Some(30),
            SA,
            &app_hints(),
        ));
        assert_eq!(status, 429);
        assert!(
            message.ends_with(&format!("{QUOTA_NOTE} (Retry-After: 30 s)")),
            "{message}"
        );
    }

    #[test]
    fn every_429_is_a_quota() {
        for body in [
            r#"{"error":{"code":429,"message":"Too many requests.","status":"RESOURCE_EXHAUSTED"}}"#,
            r#"{"error":{"code":429,"message":"boom"}}"#,
            "Too Many Requests",
        ] {
            let (status, message) = http(map_google_error(429, body, Some(5), SA, &app_hints()));
            assert_eq!(status, 429);
            assert!(message.contains(QUOTA_NOTE), "{message}");
            assert!(message.ends_with("(Retry-After: 5 s)"), "{message}");
        }
        let (_, message) = http(map_reporting_error(
            429,
            r#"{"error":{"code":429,"message":"Too many requests.","status":"RESOURCE_EXHAUSTED"}}"#,
            EMAIL,
        ));
        assert_eq!(
            message,
            format!("Too many requests. [RESOURCE_EXHAUSTED]{QUOTA_NOTE}")
        );
    }

    #[test]
    fn structured_403_quota_signals_are_quotas_without_the_word() {
        let bodies = [
            // `error.status`.
            r#"{"error":{"code":403,"message":"Slow down.","status":"RESOURCE_EXHAUSTED"}}"#,
            // `ErrorInfo.reason`.
            r#"{"error":{"code":403,"message":"Slow down.","status":"PERMISSION_DENIED",
                "details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo",
                "reason":"RATE_LIMIT_EXCEEDED","domain":"googleapis.com"}]}}"#,
            // Legacy `errors[].reason`.
            r#"{"error":{"code":403,"message":"User Rate Limit Exceeded",
                "errors":[{"domain":"usageLimits","reason":"userRateLimitExceeded"}]}}"#,
            r#"{"error":{"code":403,"message":"Rate Limit Exceeded",
                "errors":[{"domain":"usageLimits","reason":"rateLimitExceeded"}]}}"#,
            r#"{"error":{"code":403,"message":"Slow down.",
                "errors":[{"domain":"usageLimits","reason":"quotaExceeded"}]}}"#,
        ];
        for body in bodies {
            let (status, message) = http(map_google_error(403, body, None, SA, &app_hints()));
            assert_eq!(status, 429, "{body}");
            assert!(message.ends_with(QUOTA_NOTE), "{message}");
        }
    }

    #[test]
    fn real_permission_403_is_still_auth() {
        let body = r#"{"error":{"code":403,"message":"The caller does not have permission","status":"PERMISSION_DENIED"}}"#;
        let logs = crate::logging::testing::capture_logs();
        let message = auth_message(map_google_error(403, body, Some(30), SA, &app_hints()));
        assert!(message.contains("cannot access the app com.example.app"));
        assert!(!message.contains("quota"));
        assert!(!message.contains("Retry-After"));
        assert!(logs
            .messages(crate::logging::LogLevel::Warn, "googleplay")
            .is_empty());
    }

    #[test]
    fn a_structured_reason_wins_over_the_word_quota() {
        // A disabled API whose message talks about a quota project.
        let body = r#"{"error":{"code":403,
            "message":"The androidpublisher.googleapis.com API requires a quota project, which is not set by default.",
            "status":"PERMISSION_DENIED",
            "details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"SERVICE_DISABLED",
              "metadata":{"serviceTitle":"Google Play Android Developer API"}}]}}"#;
        let message = auth_message(map_google_error(403, body, None, SA, &app_hints()));
        assert!(message.starts_with("Google Play Android Developer API is not enabled"));
    }

    #[test]
    fn quota_wording_outside_403_and_429_is_not_a_quota() {
        let body = r#"{"error":{"code":400,"message":"Invalid quota project.","status":"INVALID_ARGUMENT"}}"#;
        let (status, message) = http(map_google_error(400, body, None, SA, &app_hints()));
        assert_eq!(status, 400);
        assert_eq!(message, "Invalid quota project. [INVALID_ARGUMENT]");
    }

    #[test]
    fn quota_context_names_the_status_google_sent() {
        let releases = RequestInfo::new(
            reqwest::Method::GET,
            reqwest::Url::parse(
                "https://androidpublisher.googleapis.com/androidpublisher/v3/applications/\
                 com.example.app/tracks/production/releases",
            )
            .unwrap(),
        );
        let err = map_google_error(403, RELEASES_QUOTA_BODY, None, SA, &app_hints());
        let (status, message) = http(with_request_context(err, 403, &releases));
        assert_eq!(status, 429);
        assert_eq!(
            message,
            format!(
                "Listing releases quota exceeded. [PERMISSION_DENIED]{QUOTA_NOTE} (HTTP 403 on \
                 GET applications/com.example.app/tracks/production/releases)"
            )
        );
    }

    // -----------------------------------------------------------------------
    // Signed-in user (host-supplied token): same variants, own wording
    // -----------------------------------------------------------------------

    const USER: Principal<'static> = Principal::SignedInUser;

    #[test]
    fn principal_describes_itself() {
        assert_eq!(SA.describe(), EMAIL);
        assert_eq!(USER.describe(), "the signed-in Google account");
    }

    #[test]
    fn user_permission_denied_401_reads_as_no_access_for_the_account() {
        let hints = publisher(PERMISSION_VIEW_APP_INFO, Target::App(PACKAGE));
        let message = auth_message(map_google_error(
            401,
            PERMISSION_DENIED_401_BODY,
            None,
            USER,
            &hints,
        ));
        assert!(
            message.starts_with(
                "The signed-in Google account cannot access the app com.example.app. \
                 Ask an admin to grant it at least \"View app information (read-only)\""
            ),
            "{message}"
        );
        assert!(!message.contains("service account"), "{message}");
    }

    #[test]
    fn user_service_disabled_names_the_sign_in_project() {
        let message = auth_message(map_google_error(
            403,
            SERVICE_DISABLED_BODY,
            None,
            USER,
            &ErrorHints::REPORTING,
        ));
        assert_eq!(
            message,
            format!(
                "Google Play Developer Reporting API is not enabled for the Google Cloud \
                 project this Google sign-in uses. Enable it at {ACTIVATION_URL}, wait a \
                 few minutes, then retry."
            )
        );
    }

    #[test]
    fn user_scope_insufficient_asks_to_sign_in_again() {
        let body = serde_json::json!({
            "error": {
                "code": 403,
                "message": "Request had insufficient authentication scopes.",
                "status": "PERMISSION_DENIED",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "ACCESS_TOKEN_SCOPE_INSUFFICIENT",
                    "domain": "googleapis.com"
                }]
            }
        })
        .to_string();
        let message = auth_message(map_google_error(
            403,
            &body,
            None,
            USER,
            &ErrorHints::REPORTING,
        ));
        assert!(
            message.starts_with(
                "The Google sign-in did not grant access to Google Play: sign in with \
                 Google again"
            ),
            "{message}"
        );
        // The service-account wording is untouched.
        let sa = auth_message(map_google_error(
            403,
            &body,
            None,
            SA,
            &ErrorHints::REPORTING,
        ));
        assert!(
            sa.starts_with("The access token is missing a required OAuth scope: "),
            "{sa}"
        );
    }

    #[test]
    fn user_quota_is_the_same_http_429_as_a_service_account() {
        let body = r#"{"error":{"code":429,"message":"Too many requests.","status":"RESOURCE_EXHAUSTED"}}"#;
        let (user_status, user_message) =
            http(map_google_error(429, body, Some(5), USER, &app_hints()));
        let (sa_status, sa_message) = http(map_google_error(429, body, Some(5), SA, &app_hints()));
        assert_eq!((user_status, &user_message), (sa_status, &sa_message));
        assert_eq!(user_status, 429);
    }
}
