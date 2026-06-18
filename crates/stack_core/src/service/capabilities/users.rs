use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{TeamMemberInfo, UserInfo};
use crate::error::StackError;

/// Internal, non-exported contract for the Users (App Store Connect account
/// team members and invitations) capability. Kept off the FFI for the same
/// reason as [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an
/// async *trait* cleanly, so the public surface is the concrete [`Users`] object
/// below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn UsersImpl>` can live inside an `Arc<Users>`
/// shared across the tokio runtime.
///
/// Covers reads (the lightweight team-member list, plus the unified active +
/// pending user list) and writes (invite a new user, delete an active user or
/// cancel a pending invitation) — see RUST_CORE_PLAN.md Phase 2.
#[async_trait]
pub(crate) trait UsersImpl: Send + Sync {
    /// Lists the team members of the connected account (active `users` only),
    /// reading only the lightweight fields.
    async fn fetch_team_members(&self) -> Result<Vec<TeamMemberInfo>, StackError>;

    /// Lists every user of the connected account: active members merged with
    /// outstanding invitations, discriminated by `is_pending`.
    async fn fetch_users(&self) -> Result<Vec<UserInfo>, StackError>;

    /// Invites a new user to the connected account.
    async fn invite_user(
        &self,
        email: String,
        first_name: String,
        last_name: String,
        roles: Vec<String>,
        all_apps_visible: bool,
        provisioning_allowed: bool,
    ) -> Result<(), StackError>;

    /// Deletes the user `id`: cancels the invitation when `is_pending`, otherwise
    /// removes the active member.
    async fn delete_user(&self, id: String, is_pending: bool) -> Result<(), StackError>;
}

/// UniFFI-exported Users capability handle. A thin, binding-friendly wrapper
/// around a boxed [`UsersImpl`]; async work runs on the tokio runtime. Reached
/// via [`crate::service::provider::Provider::users`].
#[derive(uniffi::Object)]
pub struct Users {
    inner: Box<dyn UsersImpl>,
}

impl Users {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn UsersImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Users {
    /// Lists the team members of the connected account — the lightweight
    /// projection of the active `users` resources (no pending invitations),
    /// carrying only `first_name`/`last_name`/`username` and the raw ASC `roles`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_team_members(&self) -> Result<Vec<TeamMemberInfo>, StackError> {
        self.inner.fetch_team_members().await
    }

    /// Lists every user of the connected account: the active members (`users`)
    /// followed by the outstanding invitations (`userInvitations`), unified into
    /// one list and discriminated by `is_pending`. For active members `email` is
    /// taken from the `username` attribute; pending invitations carry their own
    /// `email` and `expiration_date`.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response,
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_users(&self) -> Result<Vec<UserInfo>, StackError> {
        self.inner.fetch_users().await
    }

    /// Invites a new user to the connected account, granting the raw ASC `roles`
    /// (e.g. `"ADMIN"`, `"DEVELOPER"`, `"APP_MANAGER"`), passed through verbatim.
    /// `all_apps_visible` and `provisioning_allowed` set the corresponding
    /// invitation flags. App Store Connect emails the invitation; nothing
    /// meaningful is returned.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn invite_user(
        &self,
        email: String,
        first_name: String,
        last_name: String,
        roles: Vec<String>,
        all_apps_visible: bool,
        provisioning_allowed: bool,
    ) -> Result<(), StackError> {
        self.inner
            .invite_user(
                email,
                first_name,
                last_name,
                roles,
                all_apps_visible,
                provisioning_allowed,
            )
            .await
    }

    /// Deletes the user `id`. When `is_pending` is `true` the id is an
    /// outstanding `userInvitations` resource and the invitation is cancelled;
    /// otherwise the id is an active `users` resource and the member is removed.
    ///
    /// # Errors
    /// [`StackError::PendingAgreements`] when App Store Connect reports pending
    /// agreements, [`StackError::Http`] on any other non-2xx response, or
    /// [`StackError::Network`] on transport failure.
    pub async fn delete_user(&self, id: String, is_pending: bool) -> Result<(), StackError> {
        self.inner.delete_user(id, is_pending).await
    }
}
