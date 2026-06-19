use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::CertificateInfo;
use crate::error::StackError;

/// Internal, non-exported contract for the Certificates (App Store Connect signing
/// certificates) capability. Kept off the FFI for the same reason as
/// [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an async
/// *trait* cleanly, so the public surface is the concrete [`Certificates`] object
/// below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn CertificatesImpl>` can live inside an
/// `Arc<Certificates>` shared across the tokio runtime.
///
/// Covers reads (list the account's certificates, fetch a single certificate's
/// content) and writes (create a certificate from a CSR, revoke a certificate) —
/// see RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait CertificatesImpl: Send + Sync {
    /// Lists every certificate of the connected account, sorted by display name.
    /// The list does not include certificate content.
    async fn fetch_certificates(&self) -> Result<Vec<CertificateInfo>, StackError>;

    /// Fetches the base64 `certificateContent` of the certificate `id`, or `None`
    /// when the attribute is absent.
    async fn fetch_certificate_content(&self, id: String) -> Result<Option<String>, StackError>;

    /// Creates a certificate from `csr_content` of `certificate_type`, optionally
    /// related to a Pass Type ID or an Apple Pay merchant ID.
    async fn create_certificate(
        &self,
        csr_content: String,
        certificate_type: String,
        pass_type_id: Option<String>,
        merchant_id: Option<String>,
    ) -> Result<CertificateInfo, StackError>;

    /// Revokes (deletes) the certificate `id`.
    async fn revoke_certificate(&self, id: String) -> Result<(), StackError>;
}

/// UniFFI-exported Certificates capability handle. A thin, binding-friendly
/// wrapper around a boxed [`CertificatesImpl`]; async work runs on the tokio
/// runtime. Reached via [`crate::service::provider::Provider::certificates`].
#[derive(uniffi::Object)]
pub struct Certificates {
    inner: Box<dyn CertificatesImpl>,
}

impl Certificates {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn CertificatesImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Certificates {
    /// Lists every certificate of the connected account, sorted by display name,
    /// following pagination until exhausted. The list does not include
    /// certificate content, so every entry's `certificate_content` is `None`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx page,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_certificates(&self) -> Result<Vec<CertificateInfo>, StackError> {
        self.inner.fetch_certificates().await
    }

    /// Fetches the base64-encoded `certificateContent` of the certificate `id`,
    /// returning `None` when App Store Connect omits the attribute.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_certificate_content(
        &self,
        id: String,
    ) -> Result<Option<String>, StackError> {
        self.inner.fetch_certificate_content(id).await
    }

    /// Creates a certificate from `csr_content` (a base64/PEM CSR) of
    /// `certificate_type` (a raw ASC `CertificateType` value, forwarded verbatim —
    /// App Store Connect rejects unknown values with an HTTP error). When
    /// `pass_type_id` is `Some` and non-empty it is attached as the `passTypeId`
    /// relationship; otherwise when `merchant_id` is `Some` and non-empty it is
    /// attached as the `merchantId` relationship; otherwise no relationship is
    /// sent. The returned certificate includes its `certificate_content`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn create_certificate(
        &self,
        csr_content: String,
        certificate_type: String,
        pass_type_id: Option<String>,
        merchant_id: Option<String>,
    ) -> Result<CertificateInfo, StackError> {
        self.inner
            .create_certificate(csr_content, certificate_type, pass_type_id, merchant_id)
            .await
    }

    /// Revokes (deletes) the certificate `id`. Any 2xx → `Ok(())`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn revoke_certificate(&self, id: String) -> Result<(), StackError> {
        self.inner.revoke_certificate(id).await
    }
}
