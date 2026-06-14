//! The service contract: the uniform `Provider` surface, the `ServiceKind` /
//! `Capability` / `CredentialField` value types, and the registry that maps a
//! kind to a concrete plugin. Adding a service touches only `registry` and
//! `providers/`; the FFI surface stays stable.

pub(crate) mod capabilities;
pub(crate) mod kind;
pub(crate) mod provider;
pub(crate) mod registry;
pub(crate) mod sync;
