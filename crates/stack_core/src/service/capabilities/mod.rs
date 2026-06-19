//! Capability sub-objects: each capability a provider may expose is a small,
//! independently-exported handle reachable from [`crate::service::provider::Provider`]
//! (e.g. `provider.reviews()`). This keeps the `Provider` surface from growing one
//! method per ASC family as the core scales to ~31 families: a provider that lacks
//! a capability simply returns `None` for the corresponding accessor.
//!
//! Each sub-object mirrors the `Provider`/`ProviderImpl` split: an internal async
//! trait (`*Impl`, kept off the FFI) plus a `#[derive(uniffi::Object)]` wrapper
//! whose async methods run on the tokio runtime and delegate to the inner impl.

pub(crate) mod accessibility_declarations;
pub(crate) mod app_metadata;
pub(crate) mod app_store_versions;
pub(crate) mod beta_app_localizations;
pub(crate) mod beta_app_review_detail;
pub(crate) mod beta_build_localizations;
pub(crate) mod beta_groups;
pub(crate) mod builds;
pub(crate) mod bundle_ids;
pub(crate) mod certificates;
pub(crate) mod devices;
pub(crate) mod profiles;
pub(crate) mod reviews;
pub(crate) mod users;
