//! The single HTTP send/log choke point shared by every provider client and
//! authenticator.
//!
//! Lives at the crate root (not under `providers/`) because authenticators also
//! perform HTTP — e.g. an OAuth token exchange — and must trace through the same
//! function, while `auth` must not depend on `providers`.

use crate::error::StackError;
use crate::ports::DebugLogger;

/// Sends `builder`, returning the response status and body text. When a
/// `logger` is present, logs the outgoing request as a runnable cURL (with
/// pretty-printed JSON body) before sending and the response (status +
/// pretty-printed JSON body) after.
///
/// This is the single choke point every HTTP call routes through, so logging
/// lives in one place and the send/read boilerplate is not duplicated.
///
/// # Errors
/// [`StackError::Network`] on transport failure (sending or reading the body).
pub(crate) async fn send_and_read(
    builder: reqwest::RequestBuilder,
    logger: Option<&dyn DebugLogger>,
) -> Result<(reqwest::StatusCode, String), StackError> {
    trace_request(&builder, logger);

    let response = builder
        .send()
        .await
        .map_err(|e| StackError::network(e.to_string()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| StackError::network(e.to_string()))?;

    if let Some(logger) = logger {
        logger.log(render_response(status, &body));
    }

    Ok((status, body))
}

/// Like [`send_and_read`], for a response that is large and not worth tracing,
/// such as an HTML page: at most `max_bytes` of the body are read (the rest is
/// left unread, so a huge or endless body costs bounded memory), the bytes are
/// decoded as lossy UTF-8, and the response trace carries only the status and
/// how many bytes were read, never the body.
///
/// # Errors
/// [`StackError::Network`] on transport failure (sending or reading the body).
pub(crate) async fn send_and_read_capped(
    builder: reqwest::RequestBuilder,
    logger: Option<&dyn DebugLogger>,
    max_bytes: usize,
) -> Result<(reqwest::StatusCode, String), StackError> {
    trace_request(&builder, logger);

    let mut response = builder
        .send()
        .await
        .map_err(|e| StackError::network(e.to_string()))?;
    let status = response.status();

    let mut body: Vec<u8> = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| StackError::network(e.to_string()))?
    {
        // `body.len() <= max_bytes` holds on every iteration.
        let room = max_bytes.saturating_sub(body.len());
        if chunk.len() > room {
            body.extend_from_slice(chunk.get(..room).unwrap_or_default());
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }

    if let Some(logger) = logger {
        let cut = if truncated { ", truncated" } else { "" };
        logger.log(format!(
            "{}[{} bytes read{cut}; body not logged]",
            render_response(status, ""),
            body.len()
        ));
    }

    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Logs the request `builder` would send as a runnable cURL, if a `logger` is
/// present.
fn trace_request(builder: &reqwest::RequestBuilder, logger: Option<&dyn DebugLogger>) {
    if let Some(logger) = logger {
        // `try_clone` fails only for streaming bodies; callers use in-memory
        // `.json()`/`.form()`/empty bodies, so this clones in practice.
        if let Some(req) = builder.try_clone().and_then(|b| b.build().ok()) {
            logger.log(render_curl(&req));
        }
    }
}

/// Renders a `reqwest::Request` as a runnable, multi-line cURL command for the
/// debug logger: the method/URL, one `-H` line per header (in reqwest's stored
/// order, Authorization included verbatim — this is an opt-in, debug-only sink),
/// and a `-d` line carrying the body when one is present (pretty-printed when it
/// is JSON, raw otherwise — e.g. a form-encoded body).
pub(crate) fn render_curl(req: &reqwest::Request) -> String {
    let mut out = format!(
        "[RustCore] → request\n→ curl -X {} '{}'",
        req.method(),
        req.url()
    );

    for (name, value) in req.headers() {
        let value = value.to_str().unwrap_or("<non-utf8>");
        out.push_str(&format!(" \\\n  -H '{name}: {value}'"));
    }

    if let Some(bytes) = req.body().and_then(reqwest::Body::as_bytes) {
        if !bytes.is_empty() {
            out.push_str(&format!(" \\\n  -d '{}'", pretty_json(bytes)));
        }
    }

    out
}

/// Renders a response status line plus its body (pretty-printed when the body is
/// JSON, raw otherwise) for the debug logger.
pub(crate) fn render_response(status: reqwest::StatusCode, body: &str) -> String {
    format!("[RustCore] ← {status}\n{}", pretty_json(body.as_bytes()))
}

/// Pretty-prints `bytes` as JSON; on any parse/serialize failure falls back to
/// the lossy UTF-8 of the raw bytes. Never panics.
pub(crate) fn pretty_json(bytes: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Default)]
    struct CapturingLogger {
        messages: Mutex<Vec<String>>,
    }

    impl CapturingLogger {
        fn messages(&self) -> Vec<String> {
            self.messages.lock().unwrap().clone()
        }
    }

    impl DebugLogger for CapturingLogger {
        fn log(&self, message: String) {
            self.messages.lock().unwrap().push(message);
        }
    }

    #[test]
    fn pretty_json_falls_back_to_raw_for_non_json() {
        // Non-JSON bytes must not panic and must round-trip as lossy UTF-8.
        assert_eq!(pretty_json(b"not json"), "not json");
        // Valid JSON is re-rendered in pretty (multi-line) form.
        let pretty = pretty_json(br#"{"a":1}"#);
        assert!(pretty.contains("\"a\": 1"));
        assert!(pretty.contains('\n'));
    }

    #[test]
    fn render_curl_renders_form_body_raw() {
        let req = reqwest::Client::new()
            .post("https://oauth.example.test/token")
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", "aaa.bbb.ccc"),
            ])
            .build()
            .unwrap();

        let curl = render_curl(&req);
        assert!(curl.starts_with("[RustCore] → request\n→ curl -X POST "));
        assert!(curl.contains("-H 'content-type: application/x-www-form-urlencoded'"));
        // A form body is not JSON, so it is printed verbatim (percent-encoded as
        // sent), not mangled by the pretty-printer.
        assert!(curl.contains(
            "-d 'grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer\
             &assertion=aaa.bbb.ccc'"
        ));
    }

    #[test]
    fn render_curl_lists_headers_and_url() {
        let req = reqwest::Client::new()
            .get("https://api.example.test/v1/apps?limit=1")
            .bearer_auth("tok-123")
            .header("x-custom", "v")
            .build()
            .unwrap();

        let curl = render_curl(&req);
        assert!(curl.contains("curl -X GET 'https://api.example.test/v1/apps?limit=1'"));
        // Authorization is logged verbatim (opt-in debug sink).
        assert!(curl.contains(" \\\n  -H 'authorization: Bearer tok-123'"));
        assert!(curl.contains(" \\\n  -H 'x-custom: v'"));
        // A GET has no body → no `-d` line.
        assert!(!curl.contains("-d '"));
    }

    #[test]
    fn render_response_prefixes_status() {
        let rendered = render_response(reqwest::StatusCode::OK, r#"{"a":1}"#);
        assert!(rendered.starts_with("[RustCore] ← 200 OK\n"));
        assert!(rendered.contains("\"a\": 1"));
    }

    #[tokio::test]
    async fn send_and_read_returns_status_and_body_and_logs_request_then_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ping"))
            .respond_with(ResponseTemplate::new(202).set_body_string("pong"))
            .expect(1)
            .mount(&server)
            .await;

        let logger = CapturingLogger::default();
        let builder = reqwest::Client::new().get(format!("{}/ping", server.uri()));
        let (status, body) = send_and_read(builder, Some(&logger)).await.unwrap();

        assert_eq!(status, reqwest::StatusCode::ACCEPTED);
        assert_eq!(body, "pong");

        let messages = logger.messages();
        assert_eq!(messages.len(), 2);
        assert!(messages[0].starts_with("[RustCore] → request"));
        assert!(messages[0].contains(&format!("{}/ping", server.uri())));
        assert!(messages[1].starts_with("[RustCore] ← 202"));
        assert!(messages[1].ends_with("pong"));
    }

    #[tokio::test]
    async fn send_and_read_without_logger_still_returns_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ping"))
            .respond_with(ResponseTemplate::new(200).set_body_string("pong"))
            .expect(1)
            .mount(&server)
            .await;

        let builder = reqwest::Client::new().get(format!("{}/ping", server.uri()));
        let (status, body) = send_and_read(builder, None).await.unwrap();
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_eq!(body, "pong");
    }

    #[tokio::test]
    async fn send_and_read_maps_transport_error_to_network() {
        // Nothing listens on port 1, so the connection is refused.
        let logger = CapturingLogger::default();
        let builder = reqwest::Client::new().get("http://127.0.0.1:1/unreachable");
        let result = send_and_read(builder, Some(&logger)).await;

        assert!(matches!(result, Err(StackError::Network { .. })));
        // The request is traced before sending; no response line follows a
        // transport failure.
        let messages = logger.messages();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].starts_with("[RustCore] → request"));
    }

    #[tokio::test]
    async fn send_and_read_capped_returns_whole_body_under_the_cap_without_logging_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>secret</html>"))
            .expect(1)
            .mount(&server)
            .await;

        let logger = CapturingLogger::default();
        let builder = reqwest::Client::new().get(format!("{}/page", server.uri()));
        let (status, body) = send_and_read_capped(builder, Some(&logger), 1024)
            .await
            .unwrap();

        assert_eq!(status, reqwest::StatusCode::OK);
        assert_eq!(body, "<html>secret</html>");

        let messages = logger.messages();
        assert_eq!(messages.len(), 2);
        assert!(messages[0].starts_with("[RustCore] → request"));
        assert!(messages[0].contains(&format!("curl -X GET '{}/page'", server.uri())));
        assert_eq!(
            messages[1],
            "[RustCore] ← 200 OK\n[19 bytes read; body not logged]"
        );
    }

    #[tokio::test]
    async fn send_and_read_capped_stops_reading_at_the_cap() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_string("a".repeat(10_000)))
            .mount(&server)
            .await;

        let logger = CapturingLogger::default();
        let builder = reqwest::Client::new().get(format!("{}/big", server.uri()));
        let (status, body) = send_and_read_capped(builder, Some(&logger), 100)
            .await
            .unwrap();

        assert_eq!(status, reqwest::StatusCode::OK);
        assert_eq!(body, "a".repeat(100));
        assert_eq!(
            logger.messages()[1],
            "[RustCore] ← 200 OK\n[100 bytes read, truncated; body not logged]"
        );
    }

    #[tokio::test]
    async fn send_and_read_capped_exactly_at_the_cap_is_not_truncated() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/exact"))
            .respond_with(ResponseTemplate::new(404).set_body_string("x".repeat(64)))
            .mount(&server)
            .await;

        let logger = CapturingLogger::default();
        let builder = reqwest::Client::new().get(format!("{}/exact", server.uri()));
        let (status, body) = send_and_read_capped(builder, Some(&logger), 64)
            .await
            .unwrap();

        assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
        assert_eq!(body.len(), 64);
        assert_eq!(
            logger.messages()[1],
            "[RustCore] ← 404 Not Found\n[64 bytes read; body not logged]"
        );
    }

    #[tokio::test]
    async fn send_and_read_capped_decodes_invalid_utf8_lossily() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/bytes"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'o', 0xFF, b'k']))
            .mount(&server)
            .await;

        let builder = reqwest::Client::new().get(format!("{}/bytes", server.uri()));
        let (_, body) = send_and_read_capped(builder, None, 1024).await.unwrap();
        assert_eq!(body, "o\u{FFFD}k");
    }

    #[tokio::test]
    async fn send_and_read_capped_maps_transport_error_to_network() {
        let logger = CapturingLogger::default();
        let builder = reqwest::Client::new().get("http://127.0.0.1:1/unreachable");
        let result = send_and_read_capped(builder, Some(&logger), 1024).await;

        assert!(matches!(result, Err(StackError::Network { .. })));
        let messages = logger.messages();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].starts_with("[RustCore] → request"));
    }
}
