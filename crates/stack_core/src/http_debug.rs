//! The single HTTP send/log choke point shared by every provider client and
//! authenticator.
//!
//! Lives at the crate root (not under `providers/`) because authenticators also
//! perform HTTP — e.g. an OAuth token exchange — and must trace through the same
//! function, while `auth` must not depend on `providers`.
//!
//! It is also where the network audit ([`crate::audit`]) records each
//! exchange: every send function below hands the request and its outcome to
//! [`crate::audit::recorder`], which emits an entry only when the running
//! task is audited.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::audit::recorder::{Body, Exchange, StreamAudit};
use crate::error::StackError;
use crate::logging::http::{
    millis, request_id, summarize_error_body, transport_failure, RequestInfo,
};
use crate::logging::redact::redact_bearer;
use crate::logging::{self, target, LogLevel};
use crate::ports::DebugLogger;

/// A finished HTTP exchange: what came back, and what was sent (for error
/// messages).
pub(crate) struct HttpReply {
    pub(crate) status: reqwest::StatusCode,
    pub(crate) body: String,
    pub(crate) request: RequestInfo,
    /// The response headers (the App Store Connect web session reads its
    /// CSRF tokens and `Set-Cookie` from them; nothing logs them).
    pub(crate) headers: reqwest::header::HeaderMap,
    /// The `Retry-After` header in seconds, when the response carries one in
    /// its delta-seconds form (see [`retry_after_secs`]).
    pub(crate) retry_after_secs: Option<u64>,
}

/// The `Retry-After` header as a number of seconds (RFC 9110 §10.2.3
/// delta-seconds, e.g. `Retry-After: 30`). `None` when it is absent, empty or
/// in its HTTP-date form, which Google APIs do not send.
fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Sends `builder` and reads the whole body.
///
/// Logs, in this order:
/// - to the opt-in `logger` ([`DebugLogger`], unredacted): the request as a
///   runnable cURL before sending, the status and pretty-printed body after;
/// - to the host log sink ([`crate::logging`], redacted): one line per request
///   with method, host, path (never the query), status and duration —
///   `Debug` for a 2xx, `Warn` for a 4xx, `Error` for a 5xx or a transport
///   failure. A non-2xx line also carries the response's request id and a
///   summary of the error body (Google's `code` / `status` / `message` /
///   `details[]`, an OAuth error, App Store Connect `errors[]`).
///
/// When the call is audited (see [`crate::audit`]), one entry is recorded
/// for the exchange, including a transport failure.
///
/// This is the single choke point every HTTP call routes through, so logging
/// lives in one place and the send/read boilerplate is not duplicated.
///
/// # Errors
/// [`StackError::Network`] on transport failure (building, sending or reading
/// the body).
pub(crate) async fn send(
    builder: reqwest::RequestBuilder,
    logger: Option<&dyn DebugLogger>,
) -> Result<HttpReply, StackError> {
    let (client, request) = builder.build_split();
    let request = request.map_err(|e| StackError::network(e.to_string()))?;
    if let Some(logger) = logger {
        logger.log(render_curl(&request));
    }
    let info = RequestInfo::of(&request);
    let mut audit = Exchange::begin(&request);
    let started = Instant::now();

    let response = match client.execute(request).await {
        Ok(response) => response,
        Err(err) => {
            return Err(audited_transport_error(
                audit,
                &info,
                started.elapsed(),
                err,
            ))
        }
    };
    audit.response(&response);
    let status = response.status();
    let request_id = request_id(response.headers());
    let retry_after_secs = retry_after_secs(response.headers());
    let headers = response.headers().clone();
    // Exactly what `Response::text` does without reqwest's `charset` feature
    // (off in this crate): lossy UTF-8 of the whole body. Reading the bytes
    // first lets the audit see the real size and spot a binary body.
    let raw = match response.bytes().await {
        Ok(raw) => raw,
        Err(err) => {
            return Err(audited_transport_error(
                audit,
                &info,
                started.elapsed(),
                err,
            ))
        }
    };
    audit.finish(Body::Bytes(&raw));
    let body = String::from_utf8_lossy(&raw).into_owned();

    if let Some(logger) = logger {
        logger.log(render_response(status, &body));
    }
    log_response(
        &info,
        status,
        started.elapsed(),
        request_id.as_deref(),
        Some(&body),
    );

    Ok(HttpReply {
        status,
        body,
        request: info,
        headers,
        retry_after_secs,
    })
}

/// [`send`], with the body bytes passed through `decode` (e.g. undoing a
/// `Content-Encoding` the server applied unasked) before they become text
/// (lossy UTF-8, as [`reqwest::Response::text`] reads a UTF-8 body). Logs
/// exactly what [`send`] logs, the decoded body included.
///
/// # Errors
/// [`StackError::Network`] on transport failure; whatever `decode` returns.
pub(crate) async fn send_decoding(
    builder: reqwest::RequestBuilder,
    logger: Option<&dyn DebugLogger>,
    decode: impl FnOnce(&reqwest::header::HeaderMap, Vec<u8>) -> Result<Vec<u8>, StackError>,
) -> Result<HttpReply, StackError> {
    let (client, request) = builder.build_split();
    let request = request.map_err(|e| StackError::network(e.to_string()))?;
    if let Some(logger) = logger {
        logger.log(render_curl(&request));
    }
    let info = RequestInfo::of(&request);
    let mut audit = Exchange::begin(&request);
    let started = Instant::now();

    let response = match client.execute(request).await {
        Ok(response) => response,
        Err(err) => {
            return Err(audited_transport_error(
                audit,
                &info,
                started.elapsed(),
                err,
            ))
        }
    };
    audit.response(&response);
    let status = response.status();
    let request_id = request_id(response.headers());
    let retry_after_secs = retry_after_secs(response.headers());
    let headers = response.headers().clone();
    let raw = match response.bytes().await {
        Ok(raw) => raw.to_vec(),
        Err(err) => {
            return Err(audited_transport_error(
                audit,
                &info,
                started.elapsed(),
                err,
            ))
        }
    };
    let decoded = match decode(&headers, raw) {
        Ok(decoded) => decoded,
        Err(err) => {
            audit.fail(&err.to_string());
            return Err(err);
        }
    };
    audit.finish(Body::Bytes(&decoded));
    let body = String::from_utf8_lossy(&decoded).into_owned();

    if let Some(logger) = logger {
        logger.log(render_response(status, &body));
    }
    log_response(
        &info,
        status,
        started.elapsed(),
        request_id.as_deref(),
        Some(&body),
    );

    Ok(HttpReply {
        status,
        body,
        request: info,
        headers,
        retry_after_secs,
    })
}

/// [`send`], returning only the status and body.
///
/// # Errors
/// As [`send`].
pub(crate) async fn send_and_read(
    builder: reqwest::RequestBuilder,
    logger: Option<&dyn DebugLogger>,
) -> Result<(reqwest::StatusCode, String), StackError> {
    send(builder, logger)
        .await
        .map(|reply| (reply.status, reply.body))
}

/// Like [`send_and_read`], for a response that is large and not worth tracing,
/// such as an HTML page: at most `max_bytes` of the body are read (the rest is
/// left unread, so a huge or endless body costs bounded memory), the bytes are
/// decoded as lossy UTF-8, and the response trace carries only the status and
/// how many bytes were read, never the body. The host log line carries no body
/// summary either.
///
/// # Errors
/// [`StackError::Network`] on transport failure (sending or reading the body).
pub(crate) async fn send_and_read_capped(
    builder: reqwest::RequestBuilder,
    logger: Option<&dyn DebugLogger>,
    max_bytes: usize,
) -> Result<(reqwest::StatusCode, String), StackError> {
    let mut sent = send_unbuffered(builder, |request| {
        if let Some(logger) = logger {
            logger.log(render_curl(request));
        }
    })
    .await?;
    let status = sent.status();

    let mut body: Vec<u8> = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = sent.chunk().await? {
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

/// A response whose body the caller streams itself (an HTML page, a binary
/// download or upload answer), sent through [`send_unbuffered`]. Its host log
/// line (status, duration, request id; never a body summary) is already
/// written; reading the body through it logs a failure the same way [`send`]
/// does.
///
/// When the call is audited, the entry is emitted once the body has been read
/// ([`Self::text`], or [`Self::chunk`] reaching the end) or when the response
/// is dropped (a caller that stops reading early).
pub(crate) struct Unbuffered {
    response: reqwest::Response,
    info: RequestInfo,
    started: Instant,
    audit: StreamAudit,
}

impl Unbuffered {
    pub(crate) fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }

    pub(crate) fn content_length(&self) -> Option<u64> {
        self.response.content_length()
    }

    /// The response headers (never logged here). Only the Apple ID session
    /// needs them (`scnt`, `X-Apple-Id-Session-Id`, the hashcash headers).
    pub(crate) fn headers(&self) -> &reqwest::header::HeaderMap {
        self.response.headers()
    }

    /// The next body chunk, `None` at the end.
    ///
    /// # Errors
    /// [`StackError::Network`] when reading fails.
    pub(crate) async fn chunk(
        &mut self,
    ) -> Result<Option<impl std::ops::Deref<Target = [u8]>>, StackError> {
        match self.response.chunk().await {
            Ok(Some(chunk)) => {
                self.audit.chunk(&chunk);
                Ok(Some(chunk))
            }
            Ok(None) => {
                self.audit.end();
                Ok(None)
            }
            Err(err) => {
                let audit = std::mem::replace(&mut self.audit, StreamAudit::new(Exchange::none()));
                audit.fail(&err.to_string());
                Err(transport_error(&self.info, self.started.elapsed(), err))
            }
        }
    }

    /// The rest of the body as text.
    ///
    /// # Errors
    /// [`StackError::Network`] when reading fails.
    pub(crate) async fn text(self) -> Result<String, StackError> {
        let Self {
            response,
            info,
            started,
            audit,
        } = self;
        match response.text().await {
            Ok(text) => {
                audit.finish_text(&text);
                Ok(text)
            }
            Err(err) => {
                audit.fail(&err.to_string());
                Err(transport_error(&info, started.elapsed(), err))
            }
        }
    }
}

/// Sends `builder` without reading the body, for callers that stream it.
/// `trace` sees the built request first (the opt-in debug trace decides what
/// of it to print: never a binary body). The host log line is written as soon
/// as the status arrives.
///
/// # Errors
/// [`StackError::Network`] on transport failure (building or sending).
pub(crate) async fn send_unbuffered(
    builder: reqwest::RequestBuilder,
    trace: impl FnOnce(&reqwest::Request),
) -> Result<Unbuffered, StackError> {
    let (client, request) = builder.build_split();
    let request = request.map_err(|e| StackError::network(e.to_string()))?;
    trace(&request);
    let info = RequestInfo::of(&request);
    let mut audit = Exchange::begin(&request);
    let started = Instant::now();

    let response = match client.execute(request).await {
        Ok(response) => response,
        Err(err) => {
            return Err(audited_transport_error(
                audit,
                &info,
                started.elapsed(),
                err,
            ))
        }
    };
    audit.response(&response);
    log_response(
        &info,
        response.status(),
        started.elapsed(),
        request_id(response.headers()).as_deref(),
        None,
    );
    Ok(Unbuffered {
        response,
        info,
        started,
        audit: StreamAudit::new(audit),
    })
}

/// [`transport_error`], also emitting the audit entry of the failed exchange
/// (with reqwest's own error text, as [`StackError::Network`] carries it).
fn audited_transport_error(
    audit: Exchange,
    info: &RequestInfo,
    elapsed: Duration,
    err: reqwest::Error,
) -> StackError {
    audit.fail(&err.to_string());
    transport_error(info, elapsed, err)
}

/// Logs a transport failure (without the URL's query) and maps it to
/// [`StackError::Network`], whose message is unchanged: reqwest's own text.
fn transport_error(info: &RequestInfo, elapsed: Duration, err: reqwest::Error) -> StackError {
    logging::error(target::HTTP, || {
        format!(
            "{} failed after {}: {}; no response from the server",
            info.line(),
            millis(elapsed),
            transport_failure(&err)
        )
    });
    StackError::network(err.to_string())
}

/// The host log line for a response (see [`send`]). `body` is summarized on a
/// non-2xx when given.
fn log_response(
    info: &RequestInfo,
    status: reqwest::StatusCode,
    elapsed: Duration,
    request_id: Option<&str>,
    body: Option<&str>,
) {
    let level = if status.is_server_error() {
        LogLevel::Error
    } else if status.is_success() || status.is_informational() || status.is_redirection() {
        LogLevel::Debug
    } else {
        LogLevel::Warn
    };
    logging::log(level, target::HTTP, || {
        let mut line = format!("{} → {status} ({})", info.line(), millis(elapsed));
        if level != LogLevel::Debug {
            if let Some(id) = request_id {
                line.push_str(&format!(" [{id}]"));
            }
            if let Some(body) = body {
                line.push_str(&format!("; {}", summarize_error_body(body)));
            }
        }
        line
    });
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

/// A [`DebugLogger`] decorator that replaces every `Bearer <token>` with
/// `Bearer [redacted]` before forwarding, and changes nothing else (bodies,
/// URLs and other headers stay verbatim). For auth paths whose token the core
/// must never print, even to the opt-in debug sink: a host-supplied user
/// access token.
pub(crate) struct BearerRedactingLogger {
    inner: Arc<dyn DebugLogger>,
}

impl BearerRedactingLogger {
    pub(crate) fn new(inner: Arc<dyn DebugLogger>) -> Self {
        Self { inner }
    }
}

impl DebugLogger for BearerRedactingLogger {
    fn log(&self, message: String) {
        self.inner.log(redact_bearer(&message));
    }
}

/// A [`DebugLogger`] decorator for an App Store Connect account that
/// authorizes with an Apple ID web session: before forwarding, it redacts the
/// session itself (cookie headers, the `DES…` trust cookie, the `csrf` /
/// `csrf_ts` tokens and the sign-in headers; see
/// [`crate::logging::redact::redact_session_secrets`]) and changes nothing
/// else. The session's cookies never reach a rendered request anyway (the
/// jar adds them below request building); this is the second line of defense,
/// and the one that keeps the CSRF tokens out.
pub(crate) struct SessionRedactingLogger {
    inner: Arc<dyn DebugLogger>,
}

impl SessionRedactingLogger {
    pub(crate) fn new(inner: Arc<dyn DebugLogger>) -> Self {
        Self { inner }
    }
}

impl DebugLogger for SessionRedactingLogger {
    fn log(&self, message: String) {
        self.inner
            .log(crate::logging::redact::redact_session_secrets(&message));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::testing::capture_logs;
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
    fn retry_after_reads_delta_seconds_only() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

        let with = |value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(RETRY_AFTER, HeaderValue::from_static(value));
            retry_after_secs(&headers)
        };
        assert_eq!(with("30"), Some(30));
        assert_eq!(with(" 0 "), Some(0));
        assert_eq!(with("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(with(""), None);
        assert_eq!(with("-5"), None);
        assert_eq!(with("99999999999999999999999"), None);
        assert_eq!(retry_after_secs(&HeaderMap::new()), None);
    }

    #[tokio::test]
    async fn send_carries_the_retry_after_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/limited"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "17"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/plain"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let limited = send(client.get(format!("{}/limited", server.uri())), None)
            .await
            .unwrap();
        assert_eq!(limited.retry_after_secs, Some(17));
        let plain = send(client.get(format!("{}/plain", server.uri())), None)
            .await
            .unwrap();
        assert_eq!(plain.retry_after_secs, None);
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
    fn bearer_redacting_logger_redacts_only_the_bearer_token() {
        #[derive(Default)]
        struct Sink(std::sync::Mutex<Vec<String>>);
        impl DebugLogger for Sink {
            fn log(&self, message: String) {
                self.0.lock().unwrap().push(message);
            }
        }

        let sink = Arc::new(Sink::default());
        let logger = BearerRedactingLogger::new(sink.clone());
        let req = reqwest::Client::new()
            .get("https://api.example.test/v1/apps?limit=1")
            .bearer_auth("ya29.user-token")
            .header("x-custom", "v")
            .build()
            .unwrap();
        logger.log(render_curl(&req));

        let lines = sink.0.lock().unwrap().clone();
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains("ya29.user-token"), "{}", lines[0]);
        assert!(lines[0].contains("-H 'authorization: Bearer [redacted]'"));
        // Everything else stays verbatim.
        assert!(lines[0].contains("curl -X GET 'https://api.example.test/v1/apps?limit=1'"));
        assert!(lines[0].contains("-H 'x-custom: v'"));
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

    /// Strips the variable duration so lines compare exactly.
    fn without_duration(line: &str) -> String {
        let Some(open) = line.find(" (") else {
            return line.to_string();
        };
        let Some(close) = line[open..].find(" ms)") else {
            return line.to_string();
        };
        format!("{} (N ms){}", &line[..open], &line[open + close + 4..])
    }

    fn host(server: &MockServer) -> String {
        server.uri().trim_start_matches("http://").to_string()
    }

    #[tokio::test]
    async fn host_log_has_one_debug_line_per_successful_request_without_query_or_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let logs = capture_logs();
        let builder = reqwest::Client::new()
            .get(format!("{}/v1/apps?pageToken=page-secret", server.uri()))
            .bearer_auth("tok-123");
        send_and_read(builder, None).await.unwrap();

        let records = logs.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].level, LogLevel::Debug);
        assert_eq!(records[0].target, "http");
        assert_eq!(
            without_duration(&records[0].message),
            format!("GET {}/v1/apps → 200 OK (N ms)", host(&server))
        );
        let text = logs.all_text();
        assert!(
            !text.contains("tok-123") && !text.contains("page-secret"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn host_log_warns_on_4xx_with_request_id_and_google_error_summary() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/androidpublisher/v3/developers/5374/users"))
            .respond_with(
                ResponseTemplate::new(404)
                    .insert_header("x-goog-request-id", "req-42")
                    .set_body_string(
                        r#"{"error":{"code":404,"message":"Requested entity was not found.","status":"NOT_FOUND"}}"#,
                    ),
            )
            .mount(&server)
            .await;

        let logs = capture_logs();
        let builder = reqwest::Client::new().get(format!(
            "{}/androidpublisher/v3/developers/5374/users",
            server.uri()
        ));
        let (status, _) = send_and_read(builder, None).await.unwrap();
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

        assert_eq!(
            logs.messages(LogLevel::Warn, "http")
                .iter()
                .map(|m| without_duration(m))
                .collect::<Vec<_>>(),
            [format!(
                "GET {}/androidpublisher/v3/developers/5374/users → 404 Not Found (N ms) \
                 [x-goog-request-id: req-42]; google error: code=404 status=NOT_FOUND \
                 message=\"Requested entity was not found.\"",
                host(&server)
            )]
        );
    }

    #[tokio::test]
    async fn host_log_reports_5xx_as_error_and_masks_emails_in_the_path() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream down"))
            .mount(&server)
            .await;

        let logs = capture_logs();
        let builder = reqwest::Client::new().delete(format!(
            "{}/androidpublisher/v3/developers/5374/users/dev%40agency.example",
            server.uri()
        ));
        send_and_read(builder, None).await.unwrap();

        let errors = logs.messages(LogLevel::Error, "http");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(
            without_duration(&errors[0]),
            format!(
                "DELETE {}/androidpublisher/v3/developers/5374/users/d***@agency.example \
                 → 503 Service Unavailable (N ms); body (13 bytes): upstream down",
                host(&server)
            )
        );
        assert!(!logs.all_text().contains("dev@"));
    }

    #[tokio::test]
    async fn host_log_reports_a_transport_failure_without_the_query() {
        let logs = capture_logs();
        let builder = reqwest::Client::new().get("http://127.0.0.1:1/unreachable?token=abc");
        let result = send_and_read(builder, None).await;
        assert!(matches!(result, Err(StackError::Network { .. })));

        let errors = logs.messages(LogLevel::Error, "http");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].starts_with("GET 127.0.0.1:1/unreachable failed after "),
            "{}",
            errors[0]
        );
        assert!(errors[0].contains("connection failed"), "{}", errors[0]);
        assert!(errors[0].ends_with("; no response from the server"));
        assert!(!errors[0].contains("token=abc"));
    }

    #[tokio::test]
    async fn host_log_for_a_capped_read_never_summarizes_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_string("<html>secret page</html>"))
            .mount(&server)
            .await;

        let logs = capture_logs();
        let builder = reqwest::Client::new().get(format!("{}/store/apps/details", server.uri()));
        send_and_read_capped(builder, None, 1024).await.unwrap();

        let warns = logs.messages(LogLevel::Warn, "http");
        assert_eq!(
            warns
                .iter()
                .map(|m| without_duration(m))
                .collect::<Vec<_>>(),
            [format!(
                "GET {}/store/apps/details → 404 Not Found (N ms)",
                host(&server)
            )]
        );
        assert!(!logs.all_text().contains("secret page"));
    }

    #[tokio::test]
    async fn unbuffered_logs_on_arrival_and_reads_text() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_body_string("done"))
            .mount(&server)
            .await;

        let logs = capture_logs();
        let mut traced = None;
        let sent = send_unbuffered(
            reqwest::Client::new()
                .put(format!("{}/upload?X-Amz-Signature=sig", server.uri()))
                .body(vec![0u8; 8]),
            |request| traced = Some(request.method().clone()),
        )
        .await
        .unwrap();
        assert_eq!(traced, Some(reqwest::Method::PUT));
        assert_eq!(sent.status(), reqwest::StatusCode::OK);
        assert_eq!(sent.text().await.unwrap(), "done");

        let debug = logs.messages(LogLevel::Debug, "http");
        assert_eq!(
            debug
                .iter()
                .map(|m| without_duration(m))
                .collect::<Vec<_>>(),
            [format!("PUT {}/upload → 200 OK (N ms)", host(&server))]
        );
        assert!(!logs.all_text().contains("Signature"));
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
