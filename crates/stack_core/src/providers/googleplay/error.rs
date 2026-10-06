//! Maps Google API error responses (AIP-193 `{"error": {...}}` bodies) to
//! [`StackError`].
//!
//! Classification reads only the machine-readable `reason` fields —
//! `google.rpc.ErrorInfo` entries in `error.details[]`, plus the legacy
//! `error.errors[].reason` — never the human `message`, whose wording Google may
//! change at any time.

use serde::Deserialize;
use serde_json::Value;

use crate::error::StackError;

/// The API is not enabled in the service account's Google Cloud project.
const REASON_SERVICE_DISABLED: &str = "SERVICE_DISABLED";
/// The access token was minted without a scope this call needs.
const REASON_SCOPE_INSUFFICIENT: &str = "ACCESS_TOKEN_SCOPE_INSUFFICIENT";
/// Legacy (`errors[].reason`) spelling of [`REASON_SERVICE_DISABLED`].
const LEGACY_ACCESS_NOT_CONFIGURED: &str = "accessNotConfigured";
/// `@type` suffix of the structured error detail that carries `reason`.
const ERROR_INFO_TYPE_SUFFIX: &str = "google.rpc.ErrorInfo";
/// Used when a `SERVICE_DISABLED` detail names no service.
const DEFAULT_SERVICE_TITLE: &str = "Google Play Developer Reporting API";

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
/// - **401** → [`StackError::Auth`], worded neutrally: Google answers 401 both
///   for a bad token and (on the Android Publisher API) for a service account
///   without Play Console access.
/// - **403** → [`StackError::Auth`]:
///   - `SERVICE_DISABLED` / legacy `accessNotConfigured`: the API must be
///     enabled in the service account's Cloud project (names the service and
///     its activation URL when Google sends them);
///   - `ACCESS_TOKEN_SCOPE_INSUFFICIENT`: the token lacks a scope;
///   - anything else: the service account (`client_email`) must be invited in
///     Play Console.
/// - **Anything else** (400, 404, 429, 5xx) → [`StackError::Http`] carrying
///   Google's message: not a credential problem.
pub(super) fn map_google_error(status: u16, body: &str, client_email: &str) -> StackError {
    let parsed = parse_error_body(body);
    let google_msg = google_message(parsed.as_ref(), body);

    match status {
        401 => StackError::auth(format!(
            "Google did not accept the service account's authorization (HTTP 401): \
             the access token may be invalid or revoked, or the service account may \
             lack access in Play Console. Google said: {google_msg}"
        )),
        403 => map_forbidden(parsed.as_ref(), &google_msg, client_email),
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
) -> StackError {
    let infos = parsed.map(|e| error_infos(&e.details)).unwrap_or_default();
    let has_legacy_reason = |reason: &str| {
        parsed.is_some_and(|e| {
            e.errors
                .iter()
                .any(|item| item.reason.as_deref() == Some(reason))
        })
    };

    if let Some(info) = infos.iter().find(|i| i.reason == REASON_SERVICE_DISABLED) {
        return service_disabled(Some(info));
    }
    if has_legacy_reason(LEGACY_ACCESS_NOT_CONFIGURED) {
        return service_disabled(None);
    }
    if infos.iter().any(|i| i.reason == REASON_SCOPE_INSUFFICIENT) {
        return StackError::auth(format!(
            "The access token is missing a required OAuth scope: {google_msg}"
        ));
    }

    StackError::auth(format!(
        "The service account {client_email} cannot access this Google Play data. \
         Invite it in Play Console > Users and permissions with at least \"View app \
         information (read-only)\"; new access can take a while to propagate. \
         Google said: {google_msg}"
    ))
}

/// The "enable the API" message, naming the service and its activation URL
/// when the `ErrorInfo` metadata carries them.
fn service_disabled(info: Option<&ErrorInfo<'_>>) -> StackError {
    let title = info
        .and_then(|i| i.metadata("serviceTitle").or_else(|| i.metadata("service")))
        .unwrap_or(DEFAULT_SERVICE_TITLE);
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
        auth_message(map_google_error(403, &body, EMAIL))
    }

    #[test]
    fn service_disabled_uses_title_and_activation_url() {
        let message = auth_message(map_google_error(403, SERVICE_DISABLED_BODY, EMAIL));
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
        let message = auth_message(map_google_error(403, body, EMAIL));
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
        let message = auth_message(map_google_error(403, body, EMAIL));
        assert!(message.contains("Users and permissions"));
        assert!(!message.contains("missing a required OAuth scope"));
    }

    #[test]
    fn unauthenticated_401_maps_to_auth() {
        let body = r#"{"error":{"code":401,"message":"Request had invalid authentication credentials.","status":"UNAUTHENTICATED"}}"#;
        let message = auth_message(map_google_error(401, body, EMAIL));
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
            match map_google_error(status, &body, EMAIL) {
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
        match map_google_error(502, "  <html>Bad Gateway</html>\n", EMAIL) {
            StackError::Http { status, message } => {
                assert_eq!(status, 502);
                assert_eq!(message, "<html>Bad Gateway</html>");
            }
            other => panic!("expected Http, got {other:?}"),
        }
        let message = auth_message(map_google_error(403, "Forbidden", EMAIL));
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
        let message = auth_message(map_google_error(403, body, EMAIL));
        assert!(message.starts_with(&format!("The service account {EMAIL} cannot access")));
        assert!(!message.contains("is not enabled for this service account's"));
    }
}
