//! Pluggable authenticators. Each external service that needs a distinct signing
//! scheme gets a module here: App Store Connect uses ES256 team JWTs
//! ([`es256`]); Google services use the service-account OAuth2 JWT-bearer grant
//! ([`oauth_jwt`]) or, for accounts added with the user's own Google sign-in, a
//! bearer token the host supplies ([`host_token`]). Cache primitives shared by
//! the signing authenticators live in [`token`]. [`apple_session`] signs in
//! with an Apple ID (email, password, two-factor code) and keeps the web
//! session that App Store Connect accounts added that way authorize with.

pub(crate) mod apple_session;
pub(crate) mod es256;
pub(crate) mod host_token;
pub(crate) mod oauth_jwt;
pub(crate) mod token;
