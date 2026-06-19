use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::ProvisioningProfileInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the Profiles (App Store Connect
/// provisioning profiles) capability. Kept off the FFI for the same reason as
/// [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an async
/// *trait* cleanly, so the public surface is the concrete [`Profiles`] object
/// below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn ProfilesImpl>` can live inside an
/// `Arc<Profiles>` shared across the tokio runtime.
///
/// Covers reads (list the account's profiles with their resolved bundle ID,
/// fetch a single profile's base64 content) and writes (create a profile from a
/// bundle ID + certificates + optional devices, delete a profile) — see
/// RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait ProfilesImpl: Send + Sync {
    /// Lists every provisioning profile of the connected account, sorted by
    /// name, resolving each profile's bundle identifier from the response's
    /// `included[]` bundleIds. The list does not include profile content.
    async fn fetch_profiles(&self) -> Result<Vec<ProvisioningProfileInfo>, StackError>;

    /// Creates a provisioning profile named `name` of `profile_type`, related to
    /// the bundle ID `bundle_id_id`, the certificates `certificate_ids`, and the
    /// devices `device_ids`. Returns the created profile (content populated).
    async fn create_profile(
        &self,
        name: String,
        profile_type: String,
        bundle_id_id: String,
        certificate_ids: Vec<String>,
        device_ids: Vec<String>,
    ) -> Result<ProvisioningProfileInfo, StackError>;

    /// Deletes the profile `id`.
    async fn delete_profile(&self, id: String) -> Result<(), StackError>;

    /// Fetches the base64 `profileContent` of the profile `id`, or `None` when
    /// the attribute is absent.
    async fn fetch_profile_content(&self, id: String) -> Result<Option<String>, StackError>;
}

/// UniFFI-exported Profiles capability handle. A thin, binding-friendly wrapper
/// around a boxed [`ProfilesImpl`]; async work runs on the tokio runtime.
/// Reached via [`crate::service::provider::Provider::profiles`].
#[derive(uniffi::Object)]
pub struct Profiles {
    inner: Box<dyn ProfilesImpl>,
}

impl Profiles {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn ProfilesImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Profiles {
    /// Lists every provisioning profile of the connected account, sorted by
    /// name, following pagination until exhausted. Each profile's `bundle_id` is
    /// resolved to the referenced bundle ID's `identifier` string via the
    /// response's `included[]` bundleIds (or `None` when the relationship is
    /// missing or the bundle ID is absent from `included[]`). The list does not
    /// include profile content, so every entry's `profile_content` is `None`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_profiles(&self) -> Result<Vec<ProvisioningProfileInfo>, StackError> {
        self.inner.fetch_profiles().await
    }

    /// Creates a provisioning profile named `name` of `profile_type` (a raw ASC
    /// `ProfileType` value such as `IOS_APP_DEVELOPMENT`, `IOS_APP_STORE`, or
    /// `MAC_APP_STORE`, forwarded verbatim — App Store Connect rejects unknown
    /// values with an HTTP error), related to the bundle ID `bundle_id_id` and
    /// the signing certificates `certificate_ids`. When `device_ids` is non-empty
    /// the `devices` relationship is attached; when empty it is omitted entirely
    /// (App Store Connect rejects an empty `devices` array for App Store
    /// profiles). The `certificates` relationship is always sent, even when
    /// `certificate_ids` is empty. The returned profile includes its
    /// `profile_content`; its `bundle_id` is `None` (not resolved on create).
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_profile(
        &self,
        name: String,
        profile_type: String,
        bundle_id_id: String,
        certificate_ids: Vec<String>,
        device_ids: Vec<String>,
    ) -> Result<ProvisioningProfileInfo, StackError> {
        self.inner
            .create_profile(
                name,
                profile_type,
                bundle_id_id,
                certificate_ids,
                device_ids,
            )
            .await
    }

    /// Deletes the profile `id`. Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_profile(&self, id: String) -> Result<(), StackError> {
        self.inner.delete_profile(id).await
    }

    /// Fetches the base64-encoded `profileContent` of the profile `id`, returning
    /// `None` when App Store Connect omits the attribute.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_profile_content(&self, id: String) -> Result<Option<String>, StackError> {
        self.inner.fetch_profile_content(id).await
    }
}
