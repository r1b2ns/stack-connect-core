//! What the HTTP choke point ([`crate::http_debug`]) writes to the host log
//! sink: a request's method and resource (never its query, headers or body),
//! the response's request id, a summary of an error body, and a transport
//! failure without its URL.

use std::error::Error as _;
use std::time::Duration;

use serde_json::Value;

use super::redact;

/// Response headers that carry a server-side request id, in lookup order.
/// Quoting one lets a support request point at the exact call.
const REQUEST_ID_HEADERS: [&str; 4] = [
    "x-request-id",
    "x-goog-request-id",
    "x-apple-request-uuid",
    "x-cloud-trace-context",
];

/// At most this many characters of a non-JSON error body are logged.
const MAX_BODY_EXCERPT_CHARS: usize = 200;
/// At most this many App Store Connect `errors[]` entries are logged.
const MAX_ASC_ERRORS: usize = 3;

/// The loggable identity of a request: method and URL. The query is never
/// rendered (it can hold page tokens, and pre-signed URLs carry their
/// signature there).
#[derive(Debug, Clone)]
pub(crate) struct RequestInfo {
    method: reqwest::Method,
    url: reqwest::Url,
}

impl RequestInfo {
    pub(crate) fn new(method: reqwest::Method, url: reqwest::Url) -> Self {
        Self { method, url }
    }

    pub(crate) fn of(request: &reqwest::Request) -> Self {
        Self::new(request.method().clone(), request.url().clone())
    }

    /// `GET androidpublisher.googleapis.com/androidpublisher/v3/developers/5374/users`:
    /// method, host (with a non-default port) and the percent-decoded path,
    /// e-mails masked.
    pub(crate) fn line(&self) -> String {
        let host = self.url.host_str().unwrap_or("");
        let port = self.url.port().map(|p| format!(":{p}")).unwrap_or_default();
        format!(
            "{} {host}{port}{}",
            self.method,
            redact::scrub(&decoded_path(&self.url))
        )
    }
}

fn decoded_path(url: &reqwest::Url) -> String {
    percent_encoding::percent_decode_str(url.path())
        .decode_utf8_lossy()
        .into_owned()
}

/// The first request-id header the response carries, as `name: value`.
pub(crate) fn request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    REQUEST_ID_HEADERS.iter().find_map(|name| {
        let value = headers.get(*name)?.to_str().ok()?.trim();
        (!value.is_empty()).then(|| format!("{name}: {value}"))
    })
}

/// `153 ms`.
pub(crate) fn millis(elapsed: Duration) -> String {
    format!("{} ms", elapsed.as_millis())
}

/// A transport failure without its URL (reqwest's `Display` embeds the full
/// URL, query included): the failure kind plus the innermost cause.
pub(crate) fn transport_failure(err: &reqwest::Error) -> String {
    let kind = if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connection failed"
    } else if err.is_body() || err.is_decode() {
        "reading the body failed"
    } else if err.is_redirect() {
        "redirect failed"
    } else if err.is_builder() {
        "invalid request"
    } else {
        "transport error"
    };
    let mut innermost = None;
    let mut source = err.source();
    while let Some(cause) = source {
        innermost = Some(cause.to_string());
        source = cause.source();
    }
    match innermost {
        Some(cause) => format!("{kind}: {cause}"),
        None => kind.to_string(),
    }
}

/// A one-line summary of an error response body:
///
/// - Google AIP-193 (`{"error": {...}}`): `code`, `status`, `message`, each
///   `details[]` entry's `@type` (short name) with its `reason` / `domain`,
///   and legacy `errors[].reason`;
/// - Google OAuth (`{"error": "...", "error_description": "..."}`);
/// - App Store Connect JSON:API (`{"errors": [...]}`): `status`, `code`,
///   `title`, `detail` of the first entries;
/// - anything else: its size and a whitespace-collapsed excerpt.
pub(crate) fn summarize_error_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "empty body".to_string();
    }
    let json = serde_json::from_str::<Value>(trimmed).ok();
    match json.as_ref() {
        Some(Value::Object(root)) => match (root.get("error"), root.get("errors")) {
            (Some(Value::Object(error)), _) => summarize_google(error),
            (Some(Value::String(error)), _) => {
                summarize_oauth(error, root.get("error_description"))
            }
            (_, Some(Value::Array(errors))) => summarize_asc(errors),
            _ => excerpt(trimmed),
        },
        _ => excerpt(trimmed),
    }
}

fn summarize_google(error: &serde_json::Map<String, Value>) -> String {
    let mut parts = vec!["google error:".to_string()];
    if let Some(code) = error.get("code").and_then(Value::as_i64) {
        parts.push(format!("code={code}"));
    }
    if let Some(status) = str_field(error, "status") {
        parts.push(format!("status={status}"));
    }
    if let Some(message) = str_field(error, "message") {
        parts.push(format!("message={message:?}"));
    }

    let details: Vec<String> = error
        .get("details")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .map(|detail| {
            let kind = str_field(detail, "@type")
                .map(|t| t.rsplit(['/', '.']).next().unwrap_or(t).to_string())
                .unwrap_or_else(|| "?".to_string());
            let mut entry = kind;
            if let Some(reason) = str_field(detail, "reason") {
                entry.push_str(&format!(" reason={reason}"));
            }
            if let Some(domain) = str_field(detail, "domain") {
                entry.push_str(&format!(" domain={domain}"));
            }
            entry
        })
        .collect();
    if !details.is_empty() {
        parts.push(format!("details=[{}]", details.join("; ")));
    }

    let legacy: Vec<&str> = error
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("reason").and_then(Value::as_str))
        .collect();
    if !legacy.is_empty() {
        parts.push(format!("reasons=[{}]", legacy.join(", ")));
    }
    parts.join(" ")
}

fn summarize_oauth(error: &str, description: Option<&Value>) -> String {
    match description.and_then(Value::as_str) {
        Some(description) => format!("oauth error: {error} ({description})"),
        None => format!("oauth error: {error}"),
    }
}

fn summarize_asc(errors: &[Value]) -> String {
    let entries: Vec<String> = errors
        .iter()
        .take(MAX_ASC_ERRORS)
        .filter_map(Value::as_object)
        .map(|entry| {
            ["status", "code", "title", "detail"]
                .iter()
                .filter_map(|key| str_field(entry, key).map(|v| format!("{key}={v:?}")))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    let more = errors.len().saturating_sub(MAX_ASC_ERRORS);
    let suffix = if more > 0 {
        format!(" (+{more} more)")
    } else {
        String::new()
    };
    format!("api errors: [{}]{suffix}", entries.join("; "))
}

fn excerpt(body: &str) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let head: String = chars.by_ref().take(MAX_BODY_EXCERPT_CHARS).collect();
    let cut = if chars.next().is_some() { "…" } else { "" };
    format!("body ({} bytes): {head}{cut}", body.len())
}

fn str_field<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(method: &str, url: &str) -> RequestInfo {
        RequestInfo::new(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
            reqwest::Url::parse(url).unwrap(),
        )
    }

    #[test]
    fn line_has_host_and_decoded_path_without_query_and_masks_emails() {
        let request = info(
            "PATCH",
            "https://androidpublisher.googleapis.com/androidpublisher/v3/developers/5374/users/ana%40agency.example?updateMask=x&pageToken=secret",
        );
        assert_eq!(
            request.line(),
            "PATCH androidpublisher.googleapis.com/androidpublisher/v3/developers/5374/users/a***@agency.example"
        );
    }

    #[test]
    fn line_keeps_a_non_default_port() {
        let request = info(
            "GET",
            "http://127.0.0.1:8080/v1beta1/apps:search?pageSize=1",
        );
        assert_eq!(request.line(), "GET 127.0.0.1:8080/v1beta1/apps:search");
    }

    #[test]
    fn request_id_takes_the_first_known_header() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(request_id(&headers), None);
        headers.insert("x-goog-request-id", "goog-1".parse().unwrap());
        headers.insert("x-request-id", " ".parse().unwrap());
        // A blank value is skipped.
        assert_eq!(
            request_id(&headers).as_deref(),
            Some("x-goog-request-id: goog-1")
        );
        headers.insert("x-request-id", "req-9".parse().unwrap());
        assert_eq!(request_id(&headers).as_deref(), Some("x-request-id: req-9"));
    }

    #[test]
    fn summarizes_a_google_error_with_details_and_legacy_reasons() {
        let body = r#"{
          "error": {
            "code": 403,
            "message": "Google Play Android Developer API has not been used in project 1 before or it is disabled.",
            "status": "PERMISSION_DENIED",
            "errors": [{"reason": "accessNotConfigured", "domain": "usageLimits"}],
            "details": [
              {"@type": "type.googleapis.com/google.rpc.ErrorInfo",
               "reason": "SERVICE_DISABLED", "domain": "googleapis.com",
               "metadata": {"activationUrl": "https://console.developers.google.com/apis/api/x"}},
              {"@type": "type.googleapis.com/google.rpc.Help", "links": []}
            ]
          }
        }"#;
        assert_eq!(
            summarize_error_body(body),
            "google error: code=403 status=PERMISSION_DENIED message=\"Google Play Android \
             Developer API has not been used in project 1 before or it is disabled.\" \
             details=[ErrorInfo reason=SERVICE_DISABLED domain=googleapis.com; Help] \
             reasons=[accessNotConfigured]"
        );
    }

    #[test]
    fn summarizes_a_minimal_google_error() {
        assert_eq!(
            summarize_error_body(
                r#"{"error":{"code":404,"message":"Not found","status":"NOT_FOUND"}}"#
            ),
            "google error: code=404 status=NOT_FOUND message=\"Not found\""
        );
    }

    #[test]
    fn summarizes_an_oauth_error() {
        assert_eq!(
            summarize_error_body(
                r#"{"error":"invalid_grant","error_description":"Invalid JWT Signature."}"#
            ),
            "oauth error: invalid_grant (Invalid JWT Signature.)"
        );
        assert_eq!(
            summarize_error_body(r#"{"error":"invalid_client"}"#),
            "oauth error: invalid_client"
        );
    }

    #[test]
    fn summarizes_app_store_connect_errors() {
        let body = r#"{"errors":[
            {"status":"409","code":"ENTITY_ERROR","title":"t1","detail":"d1"},
            {"status":"409","code":"ENTITY_ERROR.ATTRIBUTE","title":"t2"},
            {"status":"409"},
            {"status":"409"}
        ]}"#;
        assert_eq!(
            summarize_error_body(body),
            "api errors: [status=\"409\" code=\"ENTITY_ERROR\" title=\"t1\" detail=\"d1\"; \
             status=\"409\" code=\"ENTITY_ERROR.ATTRIBUTE\" title=\"t2\"; status=\"409\"] (+1 more)"
        );
    }

    #[test]
    fn summarizes_other_bodies_as_an_excerpt() {
        assert_eq!(summarize_error_body("  "), "empty body");
        assert_eq!(
            summarize_error_body("<html>\n  <b>Not   Found</b>\n</html>"),
            "body (35 bytes): <html> <b>Not Found</b> </html>"
        );
        let long = "y".repeat(250);
        assert_eq!(
            summarize_error_body(&long),
            format!("body (250 bytes): {}…", "y".repeat(200))
        );
        // JSON of another shape.
        assert_eq!(summarize_error_body("[1,2]"), "body (5 bytes): [1,2]");
    }

    #[test]
    fn millis_formats_whole_milliseconds() {
        assert_eq!(millis(Duration::from_micros(153_900)), "153 ms");
    }

    #[tokio::test]
    async fn transport_failure_omits_the_url() {
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/secret-path?token=abc")
            .send()
            .await
            .unwrap_err();
        let summary = transport_failure(&err);
        assert!(summary.starts_with("connection failed"), "{summary}");
        assert!(!summary.contains("secret-path"), "{summary}");
        assert!(!summary.contains("token=abc"), "{summary}");
    }
}
