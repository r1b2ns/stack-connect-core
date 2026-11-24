//! The records and enums the host receives through
//! [`crate::ports::NetworkAuditSink`]. Exported through UniFFI.
//!
//! **Append only** for every enum here: UniFFI encodes a variant by its
//! position, so inserting or reordering variants would make a host built
//! against an older binding decode the wrong value.

/// One HTTP header, exactly as sent or received (a value that is not valid
/// UTF-8 is decoded lossily). Repeated headers (`Set-Cookie`) appear once per
/// value, in wire order.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NetworkAuditHeader {
    pub name: String,
    pub value: String,
}

/// Which service an exchange talked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum NetworkAuditProvider {
    /// The App Store Connect API with an API key (also its pre-signed upload
    /// and download URLs).
    AppleApi,
    /// The Apple ID web session: the sign-in (`idmsa`), olympus, App Store
    /// Connect's web API (`iris`) and the Developer Portal.
    AppleWebSession,
    /// The Google Play APIs (Android Publisher, Play Developer Reporting) and
    /// the public Play Store page.
    GooglePlay,
    /// Google's OAuth 2.0 token endpoint (a service account's token exchange).
    GoogleOauth,
    /// Firebase (reserved: the core makes no Firebase calls today).
    Firebase,
    /// Anything else.
    Other,
}

/// Which credential an exchange carried. A hint for the host's masking and
/// cURL export: the core never removes or alters a credential in the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum NetworkAuditCredentialKind {
    /// An App Store Connect API JWT in `Authorization: Bearer`
    /// (`token_expires_at_ms` is its `exp`).
    AscJwt,
    /// A Google OAuth access token in `Authorization: Bearer` (a service
    /// account's exchanged token or the host-supplied user token; opaque, so
    /// `token_expires_at_ms` is `None` unless the token is a JWT).
    GoogleAccessToken,
    /// The Apple ID web session's cookies (added by the cookie jar below
    /// request building, so they are **not** in `request_headers`) plus its
    /// session / CSRF headers.
    AppleSessionCookie,
    /// A signed JWT-bearer assertion in the form body of an OAuth token
    /// request (`token_expires_at_ms` is the assertion's `exp`).
    OauthAssertion,
    /// No credential (a pre-signed URL, a public page).
    Unauthenticated,
}

/// What became of a request or response body in an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum NetworkAuditBodyState {
    /// The whole body is in the entry, as text.
    Captured,
    /// There was no body (zero bytes).
    Empty,
    /// Only the first part of the body is in the entry: it was larger than
    /// the capture limit, or the core stopped reading it early.
    Truncated,
    /// The body is not text (an image, a gzip archive): omitted, size kept.
    Binary,
    /// The sink's [`crate::ports::NetworkAuditSink::capture_bodies`] returned
    /// `false`: omitted, size kept.
    NotCaptured,
    /// The body could not be read (a transport failure, a streamed request
    /// body, a response that failed to decode).
    Unavailable,
}

/// One finished HTTP exchange the core performed, as handed to
/// [`crate::ports::NetworkAuditSink::record`]. Every attempt of a retried
/// call is its own entry (see `attempt`).
///
/// **Raw and unredacted**: headers, URL and bodies carry live credentials
/// (bearer tokens, OAuth assertions and access tokens, CSRF tokens,
/// `Set-Cookie`). The host must store entries encrypted and mask them for
/// display and export.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NetworkAuditEntry {
    /// A random UUID (v4), unique per entry.
    pub id: String,
    /// When the request was handed to the transport, in Unix milliseconds.
    pub started_at_ms: i64,
    /// From sending the request to the end of reading the response (or the
    /// failure), in milliseconds.
    pub duration_ms: u64,
    /// `1` for the first attempt; a retry of the same call is `2`, ….
    pub attempt: u32,
    /// The core operation that caused the call (`reviews.fetch_customer_reviews`,
    /// `google.oauth.token`, …; see the README's *Network audit* section).
    /// `unknown` only for a call outside every documented operation.
    pub operation: String,
    /// The operation that started `operation`, when it ran inside another one
    /// (e.g. `google.oauth.token` inside `reviews.fetch_customer_reviews`).
    pub parent_operation: Option<String>,
    pub provider: NetworkAuditProvider,
    /// The host's account id when the core knows it (`None` during an Apple
    /// ID sign-in, which happens before an account exists).
    pub account_id: Option<String>,
    pub credential_kind: NetworkAuditCredentialKind,
    /// For a JWT credential (App Store Connect bearer, OAuth assertion): its
    /// `exp`, in Unix **milliseconds**. `None` for an opaque token, a token
    /// without `exp`, or no credential.
    pub token_expires_at_ms: Option<i64>,
    /// `GET`, `POST`, ….
    pub method: String,
    /// The full URL, query included (pre-signed URLs carry their signature
    /// there).
    pub url: String,
    /// `HTTP/1.1`, `HTTP/2.0`, … when a response arrived.
    pub http_version: Option<String>,
    /// The headers the core set on the request, in order. Headers the HTTP
    /// stack adds while sending (`Host`, `Content-Length`, the client's
    /// `User-Agent`, the session cookie jar's `Cookie`) are not included.
    pub request_headers: Vec<NetworkAuditHeader>,
    pub request_body: Option<String>,
    pub request_body_state: NetworkAuditBodyState,
    /// The request body's size in bytes (`0` without a body).
    pub request_size_bytes: u64,
    /// `None` for a transport failure before any response.
    pub response_status: Option<u16>,
    /// Every response header, in wire order (`Set-Cookie` included).
    pub response_headers: Vec<NetworkAuditHeader>,
    /// The response body as text, decompressed where the core decompresses it.
    pub response_body: Option<String>,
    pub response_body_state: NetworkAuditBodyState,
    /// The bytes of response body the core read (after decompression where
    /// the core decompresses).
    pub response_size_bytes: u64,
    /// The transport or decoding failure, when the exchange did not complete.
    pub error: Option<String>,
}
