use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{PlayGrantInfo, PlayUserInfo};
use crate::error::StackError;

/// Internal, non-exported contract for the PlayUsers capability: the users of
/// a Google Play developer account and their per-app grants. Kept off the FFI
/// for the same reason as [`crate::service::provider::ProviderImpl`].
///
/// Google Play's user model (account permissions, per-app grants, expiry, no
/// names, no roles) does not fit the App Store-shaped
/// [`crate::service::capabilities::users::Users`], hence a capability of its
/// own. Permissions are raw strings, so a permission Google adds later never
/// breaks decoding; the host owns the typed catalog.
#[async_trait]
pub(crate) trait PlayUsersImpl: Send + Sync {
    async fn fetch_users(&self, developer_id: String) -> Result<Vec<PlayUserInfo>, StackError>;

    async fn invite_user(
        &self,
        developer_id: String,
        email: String,
        account_permissions: Vec<String>,
        grants: Vec<PlayGrantInfo>,
        expiration_time: Option<String>,
    ) -> Result<PlayUserInfo, StackError>;

    async fn update_user(
        &self,
        developer_id: String,
        email: String,
        account_permissions: Vec<String>,
        expiration_time: Option<String>,
    ) -> Result<PlayUserInfo, StackError>;

    async fn delete_user(&self, developer_id: String, email: String) -> Result<(), StackError>;

    async fn set_grant(
        &self,
        developer_id: String,
        email: String,
        package_name: String,
        app_level_permissions: Vec<String>,
    ) -> Result<PlayGrantInfo, StackError>;

    async fn delete_grant(
        &self,
        developer_id: String,
        email: String,
        package_name: String,
    ) -> Result<(), StackError>;
}

/// UniFFI-exported PlayUsers capability handle, reached via
/// [`crate::service::provider::Provider::play_users`]. Every call addresses
/// one Play developer account by its ID (the number in a Play Console URL
/// after `/developers/`), which the API cannot discover: the host asks the
/// user for it. None of these calls opens an edit.
///
/// The calling service account needs the "Admin (all permissions)" account
/// permission (`CAN_MANAGE_PERMISSIONS_GLOBAL`) for all of them.
#[derive(uniffi::Object)]
pub struct PlayUsers {
    inner: Box<dyn PlayUsersImpl>,
}

impl PlayUsers {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn PlayUsersImpl>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl PlayUsers {
    /// Lists every user of the developer account (all pages), in Google's
    /// order, with their account permissions and per-app grants.
    ///
    /// # Errors
    /// [`StackError::Http`] `404` for a developer account Google does not know
    /// (or an ID that cannot be one, checked locally), [`StackError::Auth`]
    /// when Google denies access (Google's message, which may name the missing
    /// permission, is kept), [`StackError::Decode`] on malformed JSON or
    /// endless paging, [`StackError::Network`] on transport failure.
    pub async fn fetch_users(&self, developer_id: String) -> Result<Vec<PlayUserInfo>, StackError> {
        self.inner.fetch_users(developer_id).await
    }

    /// Invites `email` to the developer account with `account_permissions`,
    /// then gives it each of `grants` (Google only accepts grants after the
    /// user exists). `expiration_time` (RFC 3339, in the future) limits the
    /// access; `None` never expires. If a grant fails, the just-created
    /// invitation is removed again (best effort) and the grant's error is
    /// returned, so an invite is all or nothing. Returns the invited user.
    ///
    /// # Errors
    /// As [`Self::fetch_users`]; [`StackError::Http`] `400` for an e-mail or
    /// package name that cannot be valid (checked locally) or with Google's
    /// message when it refuses the invitation (e.g. the user already exists).
    pub async fn invite_user(
        &self,
        developer_id: String,
        email: String,
        account_permissions: Vec<String>,
        grants: Vec<PlayGrantInfo>,
        expiration_time: Option<String>,
    ) -> Result<PlayUserInfo, StackError> {
        self.inner
            .invite_user(
                developer_id,
                email,
                account_permissions,
                grants,
                expiration_time,
            )
            .await
    }

    /// Replaces the user's account permissions **and** expiry (both are in the
    /// update mask): `expiration_time: None` removes the expiry. Grants are
    /// changed with [`Self::set_grant`] / [`Self::delete_grant`].
    ///
    /// # Errors
    /// As [`Self::invite_user`]; `404` (Google's message) for an unknown user.
    pub async fn update_user(
        &self,
        developer_id: String,
        email: String,
        account_permissions: Vec<String>,
        expiration_time: Option<String>,
    ) -> Result<PlayUserInfo, StackError> {
        self.inner
            .update_user(developer_id, email, account_permissions, expiration_time)
            .await
    }

    /// Removes the user (or cancels the invitation) from the developer account.
    ///
    /// # Errors
    /// As [`Self::update_user`].
    pub async fn delete_user(&self, developer_id: String, email: String) -> Result<(), StackError> {
        self.inner.delete_user(developer_id, email).await
    }

    /// Sets the user's permissions for one app: updates the grant when it
    /// exists, creates it otherwise. Returns the stored grant.
    ///
    /// # Errors
    /// As [`Self::update_user`].
    pub async fn set_grant(
        &self,
        developer_id: String,
        email: String,
        package_name: String,
        app_level_permissions: Vec<String>,
    ) -> Result<PlayGrantInfo, StackError> {
        self.inner
            .set_grant(developer_id, email, package_name, app_level_permissions)
            .await
    }

    /// Removes the user's access to one app.
    ///
    /// # Errors
    /// As [`Self::update_user`]; `404` (Google's message) for a grant that does
    /// not exist.
    pub async fn delete_grant(
        &self,
        developer_id: String,
        email: String,
        package_name: String,
    ) -> Result<(), StackError> {
        self.inner
            .delete_grant(developer_id, email, package_name)
            .await
    }
}
