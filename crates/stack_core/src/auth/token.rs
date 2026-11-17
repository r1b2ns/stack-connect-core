//! Bearer-token cache primitives shared by every authenticator: the cached
//! token shape, the wall-clock read, and the refresh-margin rule.

use std::time::{SystemTime, UNIX_EPOCH};

/// Refresh this many seconds before `exp` to avoid handing out a token that
/// expires mid-flight.
pub(crate) const REFRESH_MARGIN_SECS: i64 = 60;

/// A bearer token held in an authenticator's cache.
#[derive(Clone)]
pub(crate) struct CachedToken {
    pub(crate) token: String,
    /// Unix-seconds `exp` of the cached token.
    pub(crate) expires_at: i64,
}

/// Current Unix time in seconds; `0` if the system clock reads before the epoch.
pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A cached token must be refreshed once `now` reaches
/// `exp - REFRESH_MARGIN_SECS`.
pub(crate) fn needs_refresh(expires_at: i64, now: i64) -> bool {
    now + REFRESH_MARGIN_SECS >= expires_at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_fresh_outside_margin() {
        // exp 120s out, now=0 → no refresh.
        assert!(!needs_refresh(120, 0));
    }

    #[test]
    fn token_stale_inside_margin() {
        // exp 30s out, now=0 → within the 60s margin → refresh.
        assert!(needs_refresh(30, 0));
    }

    #[test]
    fn token_stale_at_exact_margin() {
        // exp exactly 60s out → the boundary is inclusive → refresh. Literals
        // (not the constant) so a changed margin fails here.
        assert!(needs_refresh(60, 0));
        // One second further out is still fresh.
        assert!(!needs_refresh(61, 0));
    }
}
