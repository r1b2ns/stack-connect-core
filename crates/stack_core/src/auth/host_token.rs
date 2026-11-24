//! Host-supplied bearer tokens: for accounts added with the user's own OAuth
//! sign-in ("Sign in with Google"), where the host owns the refresh token and
//! hands the core a ready-to-use access token through
//! [`AccessTokenProvider`].
//!
//! Unlike [`super::oauth_jwt`], nothing is signed, exchanged or cached here:
//! the host already caches and refreshes, so the core asks for the token before
//! every request and forgets it after. Service-agnostic on purpose, so other
//! Google services (e.g. Firebase) can reuse it.

use std::sync::Arc;
use std::time::Duration;

use crate::error::StackError;
use crate::logging::redact::mask_email;
use crate::logging::{self, target};
use crate::ports::AccessTokenProvider;

/// Bound on one [`AccessTokenProvider::access_token`] call. A host refresh is
/// at most one token exchange, so this matches the per-request HTTP timeout
/// the Google Play provider uses; a stalled host must not hang the request
/// forever.
const DEFAULT_PROVIDER_TIMEOUT: Duration = Duration::from_secs(30);

/// Asks the host for the current access token of one account.
///
/// Deliberately not `Debug`-printing anything secret: it holds no token, only
/// the account id and the host callback.
pub(crate) struct HostTokenAuthenticator {
    account_id: String,
    provider: Arc<dyn AccessTokenProvider>,
    timeout: Duration,
}

impl HostTokenAuthenticator {
    /// An authenticator that asks `provider` for `account_id`'s token.
    pub(crate) fn new(
        account_id: impl Into<String>,
        provider: Arc<dyn AccessTokenProvider>,
    ) -> Self {
        Self::with_timeout(account_id, provider, DEFAULT_PROVIDER_TIMEOUT)
    }

    /// Same as [`Self::new`] with a custom bound on the host call, for tests.
    pub(crate) fn with_timeout(
        account_id: impl Into<String>,
        provider: Arc<dyn AccessTokenProvider>,
        timeout: Duration,
    ) -> Self {
        Self {
            account_id: account_id.into(),
            provider,
            timeout,
        }
    }

    /// Returns the host's current access token for the account.
    ///
    /// The token is trimmed; it must then be non-empty and contain no
    /// whitespace or control characters (it goes into an HTTP header verbatim).
    ///
    /// # Errors
    /// - The host returned [`StackError::Auth`] → returned unchanged, so a host
    ///   marker in the message (e.g. `invalid_grant`) round-trips.
    /// - The host returned [`StackError::Network`] → returned unchanged: an
    ///   offline device is not a sign-in problem.
    /// - Any other host error, a blank or malformed token → [`StackError::Auth`].
    /// - The host did not answer within the timeout → [`StackError::Network`].
    ///
    /// No message ever contains the token.
    pub(crate) async fn bearer_token(&self) -> Result<String, StackError> {
        let call = self.provider.access_token(self.account_id.clone());
        let result = match tokio::time::timeout(self.timeout, call).await {
            Ok(result) => result,
            Err(_) => {
                let err = StackError::network(format!(
                    "the host did not supply an access token within {} s",
                    self.timeout.as_secs_f64()
                ));
                self.log_failure(&err);
                return Err(err);
            }
        };

        let outcome = match result {
            Ok(token) => validate_token(token),
            Err(err @ (StackError::Auth { .. } | StackError::Network { .. })) => Err(err),
            Err(other) => Err(StackError::auth(format!(
                "the host's access token provider failed: {other}"
            ))),
        };
        if let Err(err) = &outcome {
            self.log_failure(err);
        }
        outcome
    }

    fn log_failure(&self, err: &StackError) {
        logging::warn(target::AUTH, || {
            format!(
                "Google sign-in: no access token for {}: {err}",
                mask_email(&self.account_id)
            )
        });
    }
}

/// Trims `token` and rejects a blank one or one an HTTP header cannot carry.
fn validate_token(token: String) -> Result<String, StackError> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(StackError::auth(
            "the host's access token provider returned an empty token",
        ));
    }
    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(StackError::auth(
            "the host's access token provider returned a malformed token \
             (whitespace or control characters)",
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
pub(crate) mod testing {
    //! A scriptable [`AccessTokenProvider`] shared by the tests of every module
    //! that authenticates with a host token.

    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    /// One scripted answer.
    pub(crate) enum Answer {
        Token(String),
        Fail(StackError),
        /// Never answers (to exercise the timeout).
        Hang,
    }

    /// Answers each call with the next scripted [`Answer`], repeating the last
    /// one once the script runs out, and records the account ids asked for.
    pub(crate) struct ScriptedTokenProvider {
        answers: Mutex<VecDeque<Answer>>,
        last: Mutex<Option<Answer>>,
        calls: Mutex<Vec<String>>,
    }

    impl ScriptedTokenProvider {
        pub(crate) fn new(answers: Vec<Answer>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into()),
                last: Mutex::new(None),
                calls: Mutex::new(Vec::new()),
            })
        }

        /// Always answers `token`.
        pub(crate) fn token(token: &str) -> Arc<Self> {
            Self::new(vec![Answer::Token(token.to_string())])
        }

        /// Always fails with `err`.
        pub(crate) fn failing(err: StackError) -> Arc<Self> {
            Self::new(vec![Answer::Fail(err)])
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn next(&self) -> Answer {
            let mut answers = self.answers.lock().unwrap();
            let mut last = self.last.lock().unwrap();
            if let Some(answer) = answers.pop_front() {
                *last = Some(clone_answer(&answer));
                return answer;
            }
            last.as_ref().map(clone_answer).unwrap_or(Answer::Hang)
        }
    }

    fn clone_answer(answer: &Answer) -> Answer {
        match answer {
            Answer::Token(token) => Answer::Token(token.clone()),
            Answer::Fail(err) => Answer::Fail(clone_error(err)),
            Answer::Hang => Answer::Hang,
        }
    }

    /// `StackError` is not `Clone`; the scripted variants are enough here.
    fn clone_error(err: &StackError) -> StackError {
        match err {
            StackError::Auth { message } => StackError::auth(message.clone()),
            StackError::Network { message } => StackError::network(message.clone()),
            StackError::InvalidCredentials { message } => {
                StackError::invalid_credentials(message.clone())
            }
            StackError::Http { status, message } => StackError::Http {
                status: *status,
                message: message.clone(),
            },
            other => StackError::decode(other.to_string()),
        }
    }

    #[async_trait::async_trait]
    impl AccessTokenProvider for ScriptedTokenProvider {
        async fn access_token(&self, account_id: String) -> Result<String, StackError> {
            self.calls.lock().unwrap().push(account_id);
            match self.next() {
                Answer::Token(token) => Ok(token),
                Answer::Fail(err) => Err(err),
                Answer::Hang => std::future::pending().await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Answer, ScriptedTokenProvider};
    use super::*;
    use crate::logging::testing::capture_logs;
    use crate::logging::LogLevel;

    const ACCOUNT: &str = "ana.dev@example.com";
    const TOKEN: &str = "ya29.a0-host-token";

    fn auth(provider: Arc<ScriptedTokenProvider>) -> HostTokenAuthenticator {
        HostTokenAuthenticator::new(ACCOUNT, provider)
    }

    #[tokio::test]
    async fn returns_the_host_token_for_the_account() {
        let provider = ScriptedTokenProvider::token(TOKEN);
        let token = auth(provider.clone()).bearer_token().await.unwrap();
        assert_eq!(token, TOKEN);
        assert_eq!(provider.calls(), vec![ACCOUNT.to_string()]);
    }

    #[tokio::test]
    async fn asks_the_host_on_every_call_without_caching() {
        let provider = ScriptedTokenProvider::new(vec![
            Answer::Token("tok-1".into()),
            Answer::Token("tok-2".into()),
        ]);
        let auth = auth(provider.clone());
        assert_eq!(auth.bearer_token().await.unwrap(), "tok-1");
        assert_eq!(auth.bearer_token().await.unwrap(), "tok-2");
        assert_eq!(provider.calls().len(), 2);
    }

    #[tokio::test]
    async fn trims_surrounding_whitespace() {
        let provider = ScriptedTokenProvider::token("  tok-1\n");
        assert_eq!(auth(provider).bearer_token().await.unwrap(), "tok-1");
    }

    #[tokio::test]
    async fn blank_token_is_auth() {
        for blank in ["", " \n\t "] {
            let err = auth(ScriptedTokenProvider::token(blank))
                .bearer_token()
                .await
                .unwrap_err();
            assert!(
                matches!(&err, StackError::Auth { message } if message.contains("empty token")),
                "{err:?}"
            );
        }
    }

    #[tokio::test]
    async fn token_with_inner_whitespace_or_control_is_auth_and_not_echoed() {
        for bad in ["s3cr3t one", "s3cr3t\r\nX-Injected: 1", "s3cr3t\u{0}"] {
            let err = auth(ScriptedTokenProvider::token(bad))
                .bearer_token()
                .await
                .unwrap_err();
            match err {
                StackError::Auth { message } => {
                    assert!(message.contains("malformed token"), "{message}");
                    assert!(!message.contains("s3cr3t"), "token echoed: {message}");
                }
                other => panic!("expected Auth, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn host_auth_error_passes_through_verbatim() {
        let provider =
            ScriptedTokenProvider::failing(StackError::auth("invalid_grant: sign in again"));
        let err = auth(provider).bearer_token().await.unwrap_err();
        assert!(
            matches!(&err, StackError::Auth { message } if message == "invalid_grant: sign in again"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn host_network_error_passes_through() {
        let provider = ScriptedTokenProvider::failing(StackError::network("offline"));
        let err = auth(provider).bearer_token().await.unwrap_err();
        assert!(
            matches!(&err, StackError::Network { message } if message == "offline"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn other_host_errors_become_auth() {
        for host_err in [
            StackError::invalid_credentials("no login"),
            StackError::Http {
                status: 400,
                message: "bad".into(),
            },
        ] {
            let err = auth(ScriptedTokenProvider::failing(host_err))
                .bearer_token()
                .await
                .unwrap_err();
            assert!(
                matches!(&err, StackError::Auth { message }
                    if message.starts_with("the host's access token provider failed: ")),
                "{err:?}"
            );
        }
    }

    #[tokio::test]
    async fn unexpected_callback_error_converts_to_auth() {
        let err = StackError::from(uniffi::UnexpectedUniFFICallbackError::new("boom"));
        assert!(
            matches!(&err, StackError::Auth { message }
                if message == "the host's access token provider failed: boom"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn stalled_host_times_out_as_network() {
        let provider = ScriptedTokenProvider::new(vec![Answer::Hang]);
        let auth =
            HostTokenAuthenticator::with_timeout(ACCOUNT, provider, Duration::from_millis(20));
        let started = std::time::Instant::now();
        let err = auth.bearer_token().await.unwrap_err();
        // Lower bound: the timeout really waited (an inverted check would not).
        assert!(started.elapsed() >= Duration::from_millis(20));
        assert!(
            matches!(&err, StackError::Network { message } if message.contains("did not supply an access token")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn failure_is_logged_with_masked_account_and_never_the_token() {
        let logs = capture_logs();
        let _ = auth(ScriptedTokenProvider::failing(StackError::auth("revoked")))
            .bearer_token()
            .await;
        let _ = auth(ScriptedTokenProvider::token(TOKEN))
            .bearer_token()
            .await;

        let warns = logs.messages(LogLevel::Warn, target::AUTH);
        assert_eq!(
            warns,
            vec!["Google sign-in: no access token for a***@example.com: authentication failed: revoked"]
        );
        assert!(!logs.all_text().contains(TOKEN));
        assert!(!logs.all_text().contains(ACCOUNT));
    }
}
