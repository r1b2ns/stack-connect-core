use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::DeviceInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the Devices (App Store Connect registered
/// development/provisioning devices) capability. Kept off the FFI for the same
/// reason as [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an
/// async *trait* cleanly, so the public surface is the concrete [`Devices`]
/// object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn DevicesImpl>` can live inside an `Arc<Devices>`
/// shared across the tokio runtime.
///
/// Covers reads (list the account's registered devices) and writes (register a
/// new device, rename a device, or disable it to remove it) — see
/// RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait DevicesImpl: Send + Sync {
    /// Lists every registered device of the connected account, sorted by name.
    async fn fetch_devices(&self) -> Result<Vec<DeviceInfo>, StackError>;

    /// Registers a new device with `name`, ASC `platform`, and `udid`.
    async fn create_device(
        &self,
        name: String,
        platform: String,
        udid: String,
    ) -> Result<DeviceInfo, StackError>;

    /// Updates the device `id`, sending only the provided attributes (`name`
    /// and/or `status`).
    async fn update_device(
        &self,
        id: String,
        name: Option<String>,
        status: Option<String>,
    ) -> Result<(), StackError>;
}

/// UniFFI-exported Devices capability handle. A thin, binding-friendly wrapper
/// around a boxed [`DevicesImpl`]; async work runs on the tokio runtime. Reached
/// via [`crate::service::provider::Provider::devices`].
#[derive(uniffi::Object)]
pub struct Devices {
    inner: Box<dyn DevicesImpl>,
}

impl Devices {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn DevicesImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Devices {
    /// Lists every registered device of the connected account, sorted by name,
    /// following pagination until exhausted.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_devices(&self) -> Result<Vec<DeviceInfo>, StackError> {
        self.inner.fetch_devices().await
    }

    /// Registers a new device with `name`, ASC `platform` (a raw
    /// `BundleIdPlatform` value such as `IOS`, `MAC_OS`, or `UNIVERSAL`, forwarded
    /// verbatim — App Store Connect rejects unknown values with an HTTP error),
    /// and `udid`, returning the created device.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_device(
        &self,
        name: String,
        platform: String,
        udid: String,
    ) -> Result<DeviceInfo, StackError> {
        self.inner.create_device(name, platform, udid).await
    }

    /// Updates the device `id`, sending only the attributes that are `Some`:
    /// `name` renames the device, and `status` (`"DISABLED"` to remove it from
    /// the account, `"ENABLED"` to re-enable it) changes its status. Attributes
    /// left `None` are omitted from the request entirely.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn update_device(
        &self,
        id: String,
        name: Option<String>,
        status: Option<String>,
    ) -> Result<(), StackError> {
        self.inner.update_device(id, name, status).await
    }
}
