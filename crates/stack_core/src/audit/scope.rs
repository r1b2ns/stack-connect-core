//! Who is auditing, and why a request is being made: a task-local
//! [`AuditScope`] entered by the exported entry points (operation, sink,
//! account) and narrowed by the few internal call sites that know better
//! (the OAuth token exchange, a retry, a pre-signed URL).
//!
//! The HTTP choke point ([`crate::http_debug`]) reads the scope, so no request
//! needs to be told about auditing and none can bypass it. Without a sink
//! anywhere in the call, nothing is entered ([`operation`] and the overrides
//! just await the future), so the cost is one task-local lookup per call.

use std::future::Future;
use std::sync::{Arc, OnceLock};

use super::types::{NetworkAuditCredentialKind, NetworkAuditProvider};
use crate::ports::NetworkAuditSink;

/// Where entries go for one connected account (or one Apple ID sign-in).
#[derive(Clone)]
pub(crate) struct AuditTarget {
    pub(super) sink: Arc<dyn NetworkAuditSink>,
    pub(super) account_id: Option<String>,
    pub(super) provider: NetworkAuditProvider,
}

impl AuditTarget {
    /// `None` without a sink, so callers can pass the host's optional sink
    /// straight through.
    pub(crate) fn new(
        sink: Option<Arc<dyn NetworkAuditSink>>,
        account_id: Option<&str>,
        provider: NetworkAuditProvider,
    ) -> Option<Self> {
        Some(Self {
            sink: sink?,
            account_id: account_id.map(str::to_string),
            provider,
        })
    }
}

impl std::fmt::Debug for AuditTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditTarget")
            .field("account_id", &self.account_id)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// The audit context of the running task. Fields other than `target` are
/// only meaningful while `target` is `Some`.
#[derive(Clone, Default)]
pub(crate) struct AuditScope {
    pub(super) target: Option<AuditTarget>,
    pub(super) operation: Option<&'static str>,
    pub(super) parent_operation: Option<&'static str>,
    /// Replaces the target's provider for requests in this scope.
    pub(super) provider: Option<NetworkAuditProvider>,
    /// Replaces the credential derived from the request.
    pub(super) credential: Option<NetworkAuditCredentialKind>,
    /// `None` = first attempt.
    pub(super) attempt: Option<u32>,
}

tokio::task_local! {
    static SCOPE: AuditScope;
}

/// The current scope, only when it has a sink.
pub(super) fn current() -> Option<AuditScope> {
    SCOPE
        .try_with(|scope| scope.target.is_some().then(|| scope.clone()))
        .ok()
        .flatten()
}

/// Runs `fut` inside `edit(current scope)`. Without a sink in `target` or in
/// the current scope, runs `fut` as is.
async fn enter<F: Future>(
    target: Option<&AuditTarget>,
    edit: impl FnOnce(&mut AuditScope),
    fut: F,
) -> F::Output {
    let mut scope = match (current(), target) {
        (None, None) => return fut.await,
        (outer, _) => outer.unwrap_or_default(),
    };
    if let Some(target) = target {
        scope.target = Some(target.clone());
    }
    edit(&mut scope);
    SCOPE.scope(scope, fut).await
}

/// Runs `fut` as the operation `name` (an exported entry point, or a named
/// sub-step), auditing into `target` when given, else into the current
/// scope's sink. A surrounding operation becomes the parent; the attempt
/// count restarts.
pub(crate) async fn operation<F: Future>(
    target: Option<&AuditTarget>,
    name: &'static str,
    fut: F,
) -> F::Output {
    enter(
        target,
        |scope| {
            scope.parent_operation = scope.operation.filter(|outer| *outer != name);
            scope.operation = Some(name);
            scope.attempt = None;
        },
        fut,
    )
    .await
}

/// [`operation`] for an internal sub-step that talks to another provider or
/// with a credential the request alone does not reveal (the OAuth token
/// exchange).
pub(crate) async fn sub_operation<F: Future>(
    name: &'static str,
    provider: NetworkAuditProvider,
    credential: NetworkAuditCredentialKind,
    fut: F,
) -> F::Output {
    enter(
        None,
        |scope| {
            scope.parent_operation = scope.operation.filter(|outer| *outer != name);
            scope.operation = Some(name);
            scope.attempt = None;
            scope.provider = Some(provider);
            scope.credential = Some(credential);
        },
        fut,
    )
    .await
}

/// Runs `fut` as attempt number `attempt` (`2` for the first retry).
pub(crate) async fn attempt<F: Future>(attempt: u32, fut: F) -> F::Output {
    enter(None, |scope| scope.attempt = Some(attempt), fut).await
}

/// Runs `fut` with `credential` as the credential of its requests (e.g. a
/// pre-signed URL sent by a client that would otherwise look authorized).
pub(crate) async fn credential<F: Future>(
    credential: NetworkAuditCredentialKind,
    fut: F,
) -> F::Output {
    enter(None, |scope| scope.credential = Some(credential), fut).await
}

/// The audit target of an exported handle: set once, by whoever hands the
/// handle to the host (the [`crate::service::provider::Provider`] accessors),
/// and used by every operation the handle runs.
#[derive(Debug, Default)]
pub(crate) struct AuditBinding(OnceLock<AuditTarget>);

impl AuditBinding {
    /// A binding to `target` (unbound when `None`).
    pub(crate) fn new(target: Option<AuditTarget>) -> Self {
        let binding = Self::default();
        binding.bind(target.as_ref());
        binding
    }

    /// Binds `target` unless already bound; `None` is a no-op.
    pub(crate) fn bind(&self, target: Option<&AuditTarget>) {
        if let Some(target) = target {
            // A handle belongs to one provider, so a second bind carries the
            // same target; keeping the first is correct.
            let _ = self.0.set(target.clone());
        }
    }

    pub(crate) fn target(&self) -> Option<&AuditTarget> {
        self.0.get()
    }

    /// Runs `fut` as the operation `name` (see [`operation`]).
    pub(crate) async fn run<F: Future>(&self, name: &'static str, fut: F) -> F::Output {
        operation(self.target(), name, fut).await
    }
}
