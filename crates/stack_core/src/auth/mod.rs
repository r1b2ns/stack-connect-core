//! Pluggable authenticators. Each external service that needs a distinct signing
//! scheme gets a module here: App Store Connect uses ES256 team JWTs
//! ([`es256`]). Cache primitives shared by every authenticator live in
//! [`token`].

pub(crate) mod es256;
pub(crate) mod token;
