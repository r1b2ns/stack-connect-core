use std::sync::Arc;

use async_trait::async_trait;

use crate::audit::{AuditBinding, AuditTarget};
use crate::domain::{BundleInfo, CommitOptions, ReleaseChange, ReleaseSummaryInfo, TrackInfo};
use crate::error::StackError;

/// Internal, non-exported contract for the Tracks capability (an app's release
/// tracks and the releases on them). Kept off the FFI for the same reason as
/// [`crate::service::provider::ProviderImpl`]: UniFFI cannot export an async
/// *trait* cleanly, so the public surface is the concrete [`Tracks`] object
/// below, which delegates here.
///
/// `Send + Sync` so a `Box<dyn TracksImpl>` can live inside an `Arc<Tracks>`
/// shared across the tokio runtime.
///
/// Google Play is the only provider exposing it.
#[async_trait]
pub(crate) trait TracksImpl: Send + Sync {
    /// Lists every release track of `app_id` with its releases.
    async fn fetch_tracks(&self, app_id: String) -> Result<Vec<TrackInfo>, StackError>;

    /// Lists the release summaries (with their review / lifecycle state) of
    /// one track of `app_id`, without opening an edit.
    async fn fetch_release_summaries(
        &self,
        app_id: String,
        track: String,
    ) -> Result<Vec<ReleaseSummaryInfo>, StackError>;

    /// Lists the app bundles uploaded to `app_id` that can go into a release.
    async fn fetch_bundles(&self, app_id: String) -> Result<Vec<BundleInfo>, StackError>;

    /// Applies one release change to `track` of `app_id` and publishes it.
    async fn apply_release_change(
        &self,
        app_id: String,
        track: String,
        change: ReleaseChange,
        options: CommitOptions,
    ) -> Result<TrackInfo, StackError>;
}

/// UniFFI-exported Tracks capability handle. A thin, binding-friendly wrapper
/// around a boxed [`TracksImpl`]; async work runs on the tokio runtime. Reached
/// via [`crate::service::provider::Provider::tracks`].
#[derive(uniffi::Object)]
pub struct Tracks {
    inner: Box<dyn TracksImpl>,
    /// The audit target the provider bound this handle to (see
    /// [`crate::audit`]).
    audit: AuditBinding,
}

impl Tracks {
    /// Wraps a concrete capability impl into the exported handle.
    pub(crate) fn new(inner: Box<dyn TracksImpl>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            audit: AuditBinding::default(),
        })
    }

    /// Audits this handle's calls into `target` (no-op for `None`).
    pub(crate) fn bind_audit(&self, target: Option<&AuditTarget>) {
        self.audit.bind(target);
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Tracks {
    /// Lists every release track of `app_id` (for Google Play, the package
    /// name, i.e. `AppInfo.id`) with its releases, in the store's order. Tracks
    /// without a release are included with an empty `releases`.
    ///
    /// Google Play reads them through a throwaway edit (insert, read, delete;
    /// nothing is committed). Google keeps one open edit per service account
    /// and app, so this invalidates any edit the same service account has open
    /// for the app elsewhere (e.g. a CI upload in progress).
    ///
    /// # Errors
    /// [`StackError::Auth`] when Google denies access or the API is disabled,
    /// [`StackError::Http`] on any other non-2xx response (`404`: app not found),
    /// [`StackError::Decode`] on malformed JSON, or [`StackError::Network`] on
    /// transport failure.
    pub async fn fetch_tracks(&self, app_id: String) -> Result<Vec<TrackInfo>, StackError> {
        self.audit
            .run("tracks.fetch_tracks", self.inner.fetch_tracks(app_id))
            .await
    }

    /// Lists the summaries of the releases on `track` of `app_id` (for Google
    /// Play, `tracks.releases.list`), in the store's order: name, raw
    /// lifecycle (review) state and version codes. Google returns at most 20
    /// and leaves out obsolete releases.
    ///
    /// **Opens no edit**, unlike [`Self::fetch_tracks`], so it never cancels an
    /// edit the service account has open elsewhere (e.g. a CI upload) and is
    /// safe to call automatically. It has no rollout fraction or status: those
    /// only come from [`Self::fetch_tracks`].
    ///
    /// A track Google does not know (`404`) yields an empty list.
    ///
    /// # Errors
    /// [`StackError::Auth`] when Google denies access or the API is disabled,
    /// [`StackError::Http`] on any other non-2xx response (`404` only for an
    /// invalid package name, checked locally), [`StackError::Decode`] on
    /// malformed JSON or a blank / `.` / `..` track name, or
    /// [`StackError::Network`] on transport failure.
    pub async fn fetch_release_summaries(
        &self,
        app_id: String,
        track: String,
    ) -> Result<Vec<ReleaseSummaryInfo>, StackError> {
        self.audit
            .run(
                "tracks.fetch_release_summaries",
                self.inner.fetch_release_summaries(app_id, track),
            )
            .await
    }

    /// Lists the app bundles uploaded to `app_id` (for Google Play,
    /// `edits.bundles.list`), in the store's order: version code and SHA-256.
    /// These are the bundles a new release can be made from.
    ///
    /// Reads through a throwaway edit like [`Self::fetch_tracks`] (insert,
    /// read, delete; nothing is committed), with the same side effect: it
    /// invalidates any edit the service account has open for the app elsewhere.
    ///
    /// # Errors
    /// As [`Self::fetch_tracks`].
    pub async fn fetch_bundles(&self, app_id: String) -> Result<Vec<BundleInfo>, StackError> {
        self.audit
            .run("tracks.fetch_bundles", self.inner.fetch_bundles(app_id))
            .await
    }

    /// Applies `change` to the release track `track` of `app_id` and
    /// **publishes it**: for Google Play, one edit that reads the track fresh,
    /// applies the change, writes the whole track back (`edits.tracks.update`),
    /// validates and commits (`edits.commit`). On any failure the edit is
    /// deleted and nothing is published. Returns the track as Google stored it.
    ///
    /// The release is identified by its version codes (see [`ReleaseChange`]);
    /// one that is no longer on the track is
    /// [`StackError::ReleaseNotFound`], and nothing is written. `options`
    /// decides whether the changes go to review right away
    /// (`send_for_review`) and what happens to changes already in review.
    ///
    /// This is a real, user-visible store change. Like every edit it also
    /// cancels any edit the service account has open for the app elsewhere
    /// (e.g. a CI upload), so hosts should only call it on an explicit,
    /// confirmed user action. Hosts should also check
    /// [`crate::service::provider::Capability::ReleaseManagement`] before offering it.
    ///
    /// # Errors
    /// [`StackError::Http`] `400` for an invalid value checked locally (a
    /// rollout fraction outside `(0, 1)`, a priority outside `0..=5`, a
    /// version code that is not a positive integer; nothing is sent) or with
    /// Google's message when it refuses the change (`edits.validate` /
    /// `edits.commit`, e.g. changes already in review, or
    /// `changesNotSentForReview` not matching the app's state);
    /// [`StackError::ReleaseNotFound`]; [`StackError::Auth`] when Google denies
    /// access (naming the release permission the track needs) or the API is
    /// disabled; [`StackError::Decode`] on a malformed response or an unusable
    /// track name; [`StackError::Network`] on transport failure.
    pub async fn apply_release_change(
        &self,
        app_id: String,
        track: String,
        change: ReleaseChange,
        options: CommitOptions,
    ) -> Result<TrackInfo, StackError> {
        self.audit
            .run(
                "tracks.apply_release_change",
                self.inner
                    .apply_release_change(app_id, track, change, options),
            )
            .await
    }
}
