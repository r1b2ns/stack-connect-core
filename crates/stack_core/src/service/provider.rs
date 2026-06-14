use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::AppInfo;
use crate::error::StackError;
use crate::service::kind::ServiceKind;

/// A capability a provider may expose. The host calls [`Provider::capabilities`]
/// to learn what a connected account can do; capabilities a provider lacks make
/// the corresponding methods return [`StackError::Unsupported`]. Grows over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum Capability {
    Apps,
}

/// Internal, non-exported contract every concrete plugin implements. Kept off the
/// FFI on purpose: UniFFI cannot export an async *trait* cleanly, so the public
/// surface is the concrete [`Provider`] object below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn ProviderImpl>` can live inside an `Arc<Provider>`
/// shared across the tokio runtime.
#[async_trait]
pub(crate) trait ProviderImpl: Send + Sync {
    /// Which service this provider speaks to.
    fn kind(&self) -> ServiceKind;

    /// The capabilities this provider exposes for the connected account.
    fn capabilities(&self) -> Vec<Capability>;

    /// Cheaply verifies the stored credentials against the live service.
    async fn validate(&self) -> Result<(), StackError>;

    /// Lists the apps visible to the connected account.
    async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError>;
}

/// UniFFI-exported provider handle. A thin, binding-friendly wrapper around a
/// boxed [`ProviderImpl`]: synchronous metadata is exported directly, async work
/// runs on the tokio runtime. Adding a *service* never changes this surface —
/// only adding a *capability* would add a method here.
#[derive(uniffi::Object)]
pub struct Provider {
    inner: Box<dyn ProviderImpl>,
}

impl Provider {
    /// Wraps a concrete plugin built by the registry into the exported handle.
    pub(crate) fn new(inner: Box<dyn ProviderImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export]
impl Provider {
    /// Which service this provider speaks to.
    pub fn kind(&self) -> ServiceKind {
        self.inner.kind()
    }

    /// The capabilities exposed for the connected account.
    pub fn capabilities(&self) -> Vec<Capability> {
        self.inner.capabilities()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Provider {
    /// Verifies the stored credentials against the live service.
    ///
    /// # Errors
    /// [`StackError::Auth`] when the credentials are rejected (including App Store
    /// Connect "pending agreements"), or a transport/decoding error.
    pub async fn validate(&self) -> Result<(), StackError> {
        self.inner.validate().await
    }

    /// Lists the apps visible to the connected account.
    ///
    /// # Errors
    /// [`StackError::Unsupported`] if the provider lacks [`Capability::Apps`];
    /// otherwise a transport, HTTP, or decoding error.
    pub async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        self.inner.fetch_apps().await
    }
}
