//! Pluggable authenticators. Each external service that needs a distinct signing
//! scheme gets a module here; App Store Connect uses ES256 team JWTs.

pub(crate) mod es256;
