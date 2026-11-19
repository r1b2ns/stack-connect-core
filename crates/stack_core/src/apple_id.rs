//! UniFFI-exported Apple ID sign-in ("Sign in with Apple ID" for App Store
//! Connect): the host drives [`AppleIdSignIn`] through the password and the
//! two-factor code, keeps the session blob [`AppleIdSignIn::finish`] returns in
//! its secure storage, and connects accounts with
//! [`crate::facade::connect_with_apple_session`].
//!
//! The core state machine is [`crate::auth::apple_session::flow::AppleSignIn`]
//! (its methods take `&mut self`); this object wraps it in a `tokio` mutex so
//! the host can hold one `Arc` and call it from any thread. Calls are
//! serialized: a second call waits for the first.
//!
//! The password crosses the FFI once, in [`AppleIdSignIn::start`], is moved
//! into a zeroizing wrapper without a copy, and is wiped once the SRP key is
//! derived. Nothing here logs it, the code, a cookie or a session header.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::auth::apple_session::flow::{AppleCodeMethod, AppleSignIn, AppleSignInStep};
use crate::auth::apple_session::session::{AppleSession, AppleTeam, SessionBlob};
use crate::auth::apple_session::srp::Password;
use crate::auth::apple_session::AppleEndpoints;
use crate::error::StackError;

/// One Apple ID sign-in, from the password to the saved session.
///
/// Errors (every method): [`StackError::InvalidCredentials`] when Apple
/// rejects the password (`start`) or the code (`submit_code`; the sign-in
/// then still waits for a code); [`StackError::SessionExpired`] when App
/// Store Connect refuses the new session; [`StackError::Http`] for 429 / 5xx;
/// [`StackError::Network`] for transport failures; [`StackError::Auth`] for
/// everything else (an unsupported second factor, a locked Apple ID, a call
/// out of order), with a message naming the cause.
#[derive(uniffi::Object)]
pub struct AppleIdSignIn {
    /// `None` once [`Self::finish`] handed the session over.
    inner: Mutex<Option<AppleSignIn>>,
}

impl AppleIdSignIn {
    fn wrap(sign_in: AppleSignIn) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Some(sign_in)),
        })
    }

    fn with_endpoints(endpoints: AppleEndpoints) -> Result<Arc<Self>, StackError> {
        Ok(Self::wrap(AppleSignIn::new(endpoints)?))
    }

    /// The object over a prepared flow (tests: fixed SRP ephemeral and clock).
    #[cfg(test)]
    pub(crate) fn from_flow(flow: AppleSignIn) -> Arc<Self> {
        Self::wrap(flow)
    }
}

fn finished() -> StackError {
    StackError::auth("this sign-in is finished; start a new one")
}

#[uniffi::export(async_runtime = "tokio")]
impl AppleIdSignIn {
    /// A fresh sign-in against Apple's production hosts. Offline.
    ///
    /// # Errors
    /// [`StackError::Network`] when the HTTP client cannot be built.
    #[uniffi::constructor]
    pub fn new() -> Result<Arc<Self>, StackError> {
        Self::with_endpoints(AppleEndpoints::default())
    }

    /// A sign-in over a saved session blob: its cookies (the two-factor trust
    /// cookie lets Apple skip the code) and its widget key. Use it to sign in
    /// again after [`StackError::SessionExpired`]. Offline.
    ///
    /// # Errors
    /// [`StackError::Decode`] when `blob` is not a session this core wrote;
    /// [`StackError::Network`] when the HTTP client cannot be built.
    #[uniffi::constructor]
    pub fn with_saved_session(blob: Vec<u8>) -> Result<Arc<Self>, StackError> {
        let session =
            AppleSession::restore(&SessionBlob::from_bytes(blob), AppleEndpoints::default())?;
        Ok(Self::wrap(AppleSignIn::from_session(session)))
    }

    /// Starts the sign-in with the Apple ID `email` and its `password`. The
    /// password is used for the SRP exchange only (it never goes on the wire)
    /// and wiped afterwards; the core never stores it.
    ///
    /// # Errors
    /// See the type docs.
    pub async fn start(
        &self,
        email: String,
        password: String,
    ) -> Result<AppleSignInStep, StackError> {
        let password = Password::new(password);
        let mut guard = self.inner.lock().await;
        let flow = guard.as_mut().ok_or_else(finished)?;
        Ok(flow.start(&email, password).await?)
    }

    /// Asks Apple to text a code to the trusted phone `phone_id` (one of the
    /// `phones` of [`AppleSignInStep::NeedsCode`]).
    ///
    /// # Errors
    /// See the type docs.
    pub async fn request_sms(&self, phone_id: u64) -> Result<AppleSignInStep, StackError> {
        let mut guard = self.inner.lock().await;
        let flow = guard.as_mut().ok_or_else(finished)?;
        Ok(flow.request_sms(phone_id).await?)
    }

    /// Submits the two-factor `code` the way `method` says (the trusted
    /// devices, or the phone a text message went to).
    ///
    /// # Errors
    /// [`StackError::InvalidCredentials`] for a wrong code (try again with
    /// the same object); otherwise see the type docs.
    pub async fn submit_code(
        &self,
        code: String,
        method: AppleCodeMethod,
    ) -> Result<AppleSignInStep, StackError> {
        let mut guard = self.inner.lock().await;
        let flow = guard.as_mut().ok_or_else(finished)?;
        Ok(flow.submit_code(&code, method).await?)
    }

    /// Every team the Apple ID belongs to, once signed in (empty before).
    pub async fn teams(&self) -> Vec<AppleTeam> {
        self.inner
            .lock()
            .await
            .as_ref()
            .and_then(AppleSignIn::teams)
            .map(<[AppleTeam]>::to_vec)
            .unwrap_or_default()
    }

    /// Ends the sign-in and returns the session blob for the host's secure
    /// storage (the bytes [`crate::ports::SessionStore::load`] hands back).
    /// One blob serves every team of the Apple ID: each account picks its team
    /// at connect. The object cannot be used afterwards.
    ///
    /// # Errors
    /// [`StackError::Auth`] unless signed in (the object stays usable then).
    pub async fn finish(&self) -> Result<Vec<u8>, StackError> {
        let mut guard = self.inner.lock().await;
        let flow = guard.take().ok_or_else(finished)?;
        if flow.teams().is_none() {
            let err = StackError::auth(
                "cannot finish: the sign-in is not complete (submit the password and the code first)",
            );
            *guard = Some(flow);
            return Err(err);
        }
        let session = flow.into_session()?;
        let blob = session.to_blob()?;
        Ok(blob.as_bytes().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_foreign_blob_is_a_decode_error_and_a_fresh_object_is_not_finished() {
        assert!(matches!(
            AppleIdSignIn::with_saved_session(b"not a blob".to_vec()),
            Err(StackError::Decode { .. })
        ));
        let object = AppleIdSignIn::new().unwrap();
        assert!(object.teams().await.is_empty());
        // Finishing too early fails and leaves the object usable.
        for _ in 0..2 {
            assert!(matches!(
                object.finish().await,
                Err(StackError::Auth { message }) if message.contains("not complete")
            ));
        }
    }

    /// The futures cross UniFFI's async export, which needs `Send`.
    #[test]
    fn exported_futures_are_send() {
        fn assert_send<T: Send>(_: &T) {}
        let object = AppleIdSignIn::new().unwrap();
        assert_send(&object.start("a@b.c".into(), "pw".into()));
        assert_send(&object.request_sms(1));
        assert_send(&object.submit_code("123456".into(), AppleCodeMethod::TrustedDevice));
        assert_send(&object.teams());
        assert_send(&object.finish());
    }
}
