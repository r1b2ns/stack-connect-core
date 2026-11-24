//! Builds one [`NetworkAuditEntry`] per exchange at the HTTP choke point and
//! hands it to the sink. Every method is a no-op when the call is not being
//! audited ([`Exchange::begin`] found no sink), so the choke point calls them
//! unconditionally.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, CONTENT_ENCODING, CONTENT_TYPE};

use super::credential::credential_of;
use super::scope::{self, AuditTarget};
use super::types::{
    NetworkAuditBodyState, NetworkAuditCredentialKind, NetworkAuditEntry, NetworkAuditHeader,
    NetworkAuditProvider,
};
use crate::logging::{self, target};

/// At most this many bytes of a body go into an entry; the rest is cut
/// ([`NetworkAuditBodyState::Truncated`]). Bounds what auditing adds to a
/// call's memory, whatever the server sends.
pub(crate) const MAX_CAPTURED_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Operation name of a call made outside every documented operation.
const UNKNOWN_OPERATION: &str = "unknown";

/// What arrived before the body.
struct ResponseMeta {
    status: u16,
    http_version: String,
    headers: Vec<NetworkAuditHeader>,
    content_type: String,
    content_encoding: String,
}

/// An exchange being audited.
struct Recorder {
    target: AuditTarget,
    capture: bool,
    operation: &'static str,
    parent_operation: Option<&'static str>,
    provider: NetworkAuditProvider,
    credential_kind: NetworkAuditCredentialKind,
    token_expires_at_ms: Option<i64>,
    attempt: u32,
    started_at_ms: i64,
    started: Instant,
    method: String,
    url: String,
    request_headers: Vec<NetworkAuditHeader>,
    request_body: Option<String>,
    request_body_state: NetworkAuditBodyState,
    request_size_bytes: u64,
    response: Option<ResponseMeta>,
}

/// A response body, as the choke point has it.
pub(crate) enum Body<'a> {
    /// Already decoded text.
    Text(&'a str),
    /// Bytes (decoded where the caller decodes).
    Bytes(&'a [u8]),
}

/// The audit of one exchange (inactive without a sink).
pub(crate) struct Exchange(Option<Box<Recorder>>);

impl Exchange {
    /// An exchange that is not audited.
    pub(crate) fn none() -> Self {
        Self(None)
    }

    /// Starts auditing `request` when the current scope has a sink: asks the
    /// sink whether to keep bodies and snapshots the request.
    pub(crate) fn begin(request: &reqwest::Request) -> Self {
        let Some(scope) = scope::current() else {
            return Self(None);
        };
        let Some(target) = scope.target else {
            return Self(None);
        };
        let sink = target.sink.clone();
        // A panicking sink must not break the request: keep no bodies then.
        let capture = catch_unwind(AssertUnwindSafe(|| sink.capture_bodies())).unwrap_or(false);
        let provider = scope.provider.unwrap_or(target.provider);
        let (credential_kind, token_expires_at_ms) =
            credential_of(request, provider, scope.credential);

        let headers = request.headers();
        let (request_body, request_body_state, request_size_bytes) = match request.body() {
            None => (None, NetworkAuditBodyState::Empty, 0),
            Some(body) => match body.as_bytes() {
                Some(bytes) => {
                    let (text, state) = capture_body(
                        bytes,
                        capture,
                        &header_text(headers, CONTENT_TYPE.as_str()),
                        true,
                    );
                    (text, state, len_u64(bytes.len()))
                }
                // A streamed body: never read here; its size only when declared.
                None => (
                    None,
                    NetworkAuditBodyState::Unavailable,
                    header_text(headers, "content-length").parse().unwrap_or(0),
                ),
            },
        };

        Self(Some(Box::new(Recorder {
            capture,
            operation: scope.operation.unwrap_or(UNKNOWN_OPERATION),
            parent_operation: scope.parent_operation,
            provider,
            credential_kind,
            token_expires_at_ms,
            attempt: scope.attempt.unwrap_or(1),
            started_at_ms: unix_ms_now(),
            started: Instant::now(),
            method: request.method().to_string(),
            url: request.url().to_string(),
            request_headers: header_list(headers),
            request_body,
            request_body_state,
            request_size_bytes,
            response: None,
            target,
        })))
    }

    /// Whether this exchange is audited (and wants the body bytes kept).
    pub(crate) fn wants_body(&self) -> bool {
        self.0.as_ref().is_some_and(|r| r.capture)
    }

    /// Notes the status line and headers.
    pub(crate) fn response(&mut self, response: &reqwest::Response) {
        if let Some(recorder) = self.0.as_mut() {
            let headers = response.headers();
            recorder.response = Some(ResponseMeta {
                status: response.status().as_u16(),
                http_version: format!("{:?}", response.version()),
                headers: header_list(headers),
                content_type: header_text(headers, CONTENT_TYPE.as_str()),
                content_encoding: header_text(headers, CONTENT_ENCODING.as_str()),
            });
        }
    }

    /// The `Content-Encoding` of the response, lowercase (empty when none or
    /// when not audited).
    pub(crate) fn content_encoding(&self) -> &str {
        self.0
            .as_ref()
            .and_then(|r| r.response.as_ref())
            .map_or("", |m| m.content_encoding.as_str())
    }

    /// Emits the entry for a completed exchange whose whole body is `body`.
    pub(crate) fn finish(self, body: Body<'_>) {
        let bytes = match body {
            Body::Text(text) => text.as_bytes(),
            Body::Bytes(bytes) => bytes,
        };
        self.finish_read(Some(bytes), len_u64(bytes.len()), true);
    }

    /// Emits the entry for a streamed body: `captured` is what was kept of
    /// it (`None` when bodies are not captured), `read_bytes` how much was
    /// read, `complete` whether it was read to the end.
    pub(crate) fn finish_read(self, captured: Option<&[u8]>, read_bytes: u64, complete: bool) {
        let Some(recorder) = self.0 else {
            return;
        };
        let content_type = recorder
            .response
            .as_ref()
            .map(|m| m.content_type.as_str())
            .unwrap_or_default();
        let (text, state) = match captured {
            _ if read_bytes == 0 && complete => (None, NetworkAuditBodyState::Empty),
            Some(bytes) if recorder.capture => {
                match capture_body(bytes, true, content_type, complete) {
                    (text, NetworkAuditBodyState::Captured) if !complete => {
                        (text, NetworkAuditBodyState::Truncated)
                    }
                    (_, NetworkAuditBodyState::Empty) => (None, NetworkAuditBodyState::Truncated),
                    other => other,
                }
            }
            _ => (None, NetworkAuditBodyState::NotCaptured),
        };
        emit(*recorder, text, state, read_bytes, None);
    }

    /// Emits the entry for an exchange that failed with `error`: a transport
    /// failure (no status when no response arrived) or a body that could not
    /// be read or decoded.
    pub(crate) fn fail(self, error: &str) {
        if let Some(recorder) = self.0 {
            emit(
                *recorder,
                None,
                NetworkAuditBodyState::Unavailable,
                0,
                Some(error.to_string()),
            );
        }
    }
}

/// The audit of a response whose body the caller streams
/// ([`crate::http_debug::Unbuffered`]): counts (and, when capturing, keeps up
/// to [`MAX_CAPTURED_BODY_BYTES`] of) the chunks, and emits the entry once,
/// when the body has been read or the response is dropped.
pub(crate) struct StreamAudit {
    exchange: Exchange,
    kept: Vec<u8>,
    read_bytes: u64,
    ended: bool,
}

impl StreamAudit {
    pub(crate) fn new(exchange: Exchange) -> Self {
        Self {
            exchange,
            kept: Vec::new(),
            read_bytes: 0,
            ended: false,
        }
    }

    /// Notes one body chunk.
    pub(crate) fn chunk(&mut self, chunk: &[u8]) {
        if self.exchange.0.is_none() {
            return;
        }
        self.read_bytes = self.read_bytes.saturating_add(len_u64(chunk.len()));
        if self.exchange.wants_body() {
            // One byte over the cap is kept so the cut is detected.
            let room = (MAX_CAPTURED_BODY_BYTES + 1).saturating_sub(self.kept.len());
            self.kept
                .extend_from_slice(chunk.get(..room.min(chunk.len())).unwrap_or_default());
        }
    }

    /// Notes the end of the body.
    pub(crate) fn end(&mut self) {
        self.ended = true;
    }

    /// Emits the entry for a body read as `text` in one go.
    pub(crate) fn finish_text(mut self, text: &str) {
        self.take().finish(Body::Text(text));
    }

    /// Emits the entry for a body that failed to read.
    pub(crate) fn fail(mut self, error: &str) {
        self.take().fail(error);
    }

    fn take(&mut self) -> Exchange {
        std::mem::replace(&mut self.exchange, Exchange(None))
    }
}

impl Drop for StreamAudit {
    fn drop(&mut self) {
        let exchange = self.take();
        if exchange.0.is_none() {
            return;
        }
        if !exchange.wants_body() {
            exchange.finish_read(None, self.read_bytes, self.ended);
            return;
        }
        let kept = std::mem::take(&mut self.kept);
        // Undo a `Content-Encoding` the way the core's own readers do, so the
        // entry shows text rather than a gzip archive.
        let decoded = if self.ended && kept.len() <= MAX_CAPTURED_BODY_BYTES {
            crate::auth::apple_session::transport::inflate_by_encoding(
                exchange.content_encoding(),
                &kept,
                MAX_CAPTURED_BODY_BYTES,
            )
        } else {
            None
        };
        match decoded {
            Some(text) => exchange.finish_read(Some(&text), len_u64(text.len()), true),
            None => exchange.finish_read(Some(&kept), self.read_bytes, self.ended),
        }
    }
}

fn emit(
    recorder: Recorder,
    response_body: Option<String>,
    response_body_state: NetworkAuditBodyState,
    response_size_bytes: u64,
    error: Option<String>,
) {
    let duration_ms = u64::try_from(recorder.started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (response_status, http_version, response_headers) = match recorder.response {
        Some(meta) => (Some(meta.status), Some(meta.http_version), meta.headers),
        None => (None, None, Vec::new()),
    };
    let entry = NetworkAuditEntry {
        id: uuid::Uuid::new_v4().to_string(),
        started_at_ms: recorder.started_at_ms,
        duration_ms,
        attempt: recorder.attempt,
        operation: recorder.operation.to_string(),
        parent_operation: recorder.parent_operation.map(str::to_string),
        provider: recorder.provider,
        account_id: recorder.target.account_id.clone(),
        credential_kind: recorder.credential_kind,
        token_expires_at_ms: recorder.token_expires_at_ms,
        method: recorder.method,
        url: recorder.url,
        http_version,
        request_headers: recorder.request_headers,
        request_body: recorder.request_body,
        request_body_state: recorder.request_body_state,
        request_size_bytes: recorder.request_size_bytes,
        response_status,
        response_headers,
        response_body,
        response_body_state,
        response_size_bytes,
        error,
    };
    let sink = recorder.target.sink;
    if catch_unwind(AssertUnwindSafe(|| sink.record(entry))).is_err() {
        // Nothing of the entry is logged: it is raw and unredacted.
        logging::warn(target::HTTP, || {
            "the network audit sink panicked; one entry was dropped".to_string()
        });
    }
}

/// The text to keep of `bytes`, and what became of the body. `complete` is
/// `false` when `bytes` may end mid-character (a partial read).
fn capture_body(
    bytes: &[u8],
    capture: bool,
    content_type: &str,
    complete: bool,
) -> (Option<String>, NetworkAuditBodyState) {
    if bytes.is_empty() {
        return (None, NetworkAuditBodyState::Empty);
    }
    if !capture {
        return (None, NetworkAuditBodyState::NotCaptured);
    }
    if is_binary_type(content_type) {
        return (None, NetworkAuditBodyState::Binary);
    }
    let over = bytes.len() > MAX_CAPTURED_BODY_BYTES;
    let kept = bytes.get(..MAX_CAPTURED_BODY_BYTES).unwrap_or(bytes);
    let text = match std::str::from_utf8(kept) {
        Ok(text) => text,
        // A cut (by the cap or a partial read) may split the last character.
        Err(err) if (over || !complete) && err.error_len().is_none() => kept
            .get(..err.valid_up_to())
            .and_then(|valid| std::str::from_utf8(valid).ok())
            .unwrap_or_default(),
        Err(_) => return (None, NetworkAuditBodyState::Binary),
    };
    if text.contains('\0') {
        return (None, NetworkAuditBodyState::Binary);
    }
    let state = if over {
        NetworkAuditBodyState::Truncated
    } else {
        NetworkAuditBodyState::Captured
    };
    (Some(text.to_string()), state)
}

/// Media types whose bodies are never text.
fn is_binary_type(content_type: &str) -> bool {
    let media = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    ["image/", "audio/", "video/", "font/"]
        .iter()
        .any(|prefix| media.starts_with(prefix))
        || matches!(
            media.as_str(),
            "application/octet-stream"
                | "application/gzip"
                | "application/x-gzip"
                | "application/zip"
                | "application/pdf"
                | "application/x-bplist"
                | "application/protobuf"
                | "application/x-protobuf"
        )
}

fn header_list(headers: &HeaderMap) -> Vec<NetworkAuditHeader> {
    headers
        .iter()
        .map(|(name, value)| NetworkAuditHeader {
            name: name.as_str().to_string(),
            value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
        })
        .collect()
}

fn header_text(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

fn len_u64(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

fn unix_ms_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_are_kept_as_text_cut_at_the_cap_or_flagged_binary() {
        assert_eq!(
            capture_body(b"", true, "", true),
            (None, NetworkAuditBodyState::Empty)
        );
        assert_eq!(
            capture_body(b"{}", false, "", true),
            (None, NetworkAuditBodyState::NotCaptured)
        );
        assert_eq!(
            capture_body(b"{\"a\":1}", true, "application/json", true),
            (Some("{\"a\":1}".into()), NetworkAuditBodyState::Captured)
        );
        assert_eq!(
            capture_body(b"\x89PNG", true, "image/png", true),
            (None, NetworkAuditBodyState::Binary)
        );
        assert_eq!(
            capture_body(&[0x1f, 0x8b, 0x08, 0xff], true, "", true),
            (None, NetworkAuditBodyState::Binary)
        );
        assert_eq!(
            capture_body(b"a\0b", true, "text/plain", true),
            (None, NetworkAuditBodyState::Binary)
        );

        // Exactly at the cap is whole; one over is cut.
        let at_cap = "a".repeat(MAX_CAPTURED_BODY_BYTES);
        assert_eq!(
            capture_body(at_cap.as_bytes(), true, "", true).1,
            NetworkAuditBodyState::Captured
        );
        let over = "a".repeat(MAX_CAPTURED_BODY_BYTES + 1);
        let (text, state) = capture_body(over.as_bytes(), true, "", true);
        assert_eq!(state, NetworkAuditBodyState::Truncated);
        assert_eq!(text.unwrap().len(), MAX_CAPTURED_BODY_BYTES);

        // A cut through a multi-byte character keeps the valid prefix.
        let mut split = "a".repeat(MAX_CAPTURED_BODY_BYTES - 1);
        split.push('é');
        let (text, state) = capture_body(split.as_bytes(), true, "", true);
        assert_eq!(state, NetworkAuditBodyState::Truncated);
        assert_eq!(text.unwrap().len(), MAX_CAPTURED_BODY_BYTES - 1);
        // A partial read may end mid-character too.
        assert_eq!(
            capture_body("é".as_bytes().get(..1).unwrap(), true, "", false),
            (Some(String::new()), NetworkAuditBodyState::Captured)
        );
    }
}
