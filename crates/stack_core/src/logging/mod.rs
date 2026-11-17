//! Diagnostic logging: one process-wide, leveled, **redacted** sink the host
//! installs with [`crate::facade::set_core_logger`].
//!
//! This is separate from the per-connection [`crate::ports::DebugLogger`]: that
//! one is an opt-in developer trace that prints runnable cURLs with bearer
//! tokens verbatim, and is meant to stay off outside a debug launch flag. The
//! [`CoreLogger`] sink is meant to stay **on** in production builds, so it never
//! receives a token, an `Authorization` header, a private key, a JSON key or a
//! JWT, and user e-mails reach it masked (`d***@agency.example`):
//!
//! - call sites only format non-secret facts (method, host and path without the
//!   query, status, duration, ids, counts, Google's error fields);
//! - every message then goes through [`redact::scrub`] before it reaches the
//!   host, as a safety net for anything a remote error message echoes back.
//!
//! Levels (most to least severe): [`LogLevel::Error`] (5xx, transport
//! failures), [`LogLevel::Warn`] (4xx, local validation rejections, failed
//! best-effort cleanups), [`LogLevel::Info`] (writes and their outcome, token
//! retries), [`LogLevel::Debug`] (every request line, reads, paging).
//!
//! Formatting is lazy: a message closure only runs when its level is enabled
//! and a sink is installed, so disabled logging costs one atomic load.

pub(crate) mod http;
pub(crate) mod redact;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use crate::ports::CoreLogger;

/// Severity of a diagnostic line, most severe first. A sink installed with a
/// minimum level receives that level and every more severe one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, uniffi::Enum)]
pub enum LogLevel {
    /// A call failed in a way the user will see: a 5xx, a transport failure.
    Error,
    /// Something was refused or skipped: a 4xx, a local validation failure
    /// (nothing sent), a best-effort cleanup that failed.
    Warn,
    /// What changed and why: writes, their ids and outcome, token retries.
    Info,
    /// Everything else: one line per HTTP request, reads, paging.
    Debug,
}

impl LogLevel {
    fn as_u8(self) -> u8 {
        match self {
            Self::Error => 0,
            Self::Warn => 1,
            Self::Info => 2,
            Self::Debug => 3,
        }
    }
}

/// Stable `target` strings hosts can map to a logging category.
pub(crate) mod target {
    /// The shared HTTP choke point (every provider and authenticator).
    pub(crate) const HTTP: &str = "http";
    /// Access tokens: minting, caching, invalidation (both services).
    pub(crate) const AUTH: &str = "auth";
    /// Google Play: transport-level checks shared by every call (package
    /// names), and the apps listing (`apps:search`).
    pub(crate) const PLAY: &str = "googleplay";
    /// Google Play: track reads, release summaries and release changes.
    pub(crate) const PLAY_TRACKS: &str = "googleplay.tracks";
    /// Google Play: developer-account users and grants.
    pub(crate) const PLAY_USERS: &str = "googleplay.users";
}

/// The installed host sink. Read on every emitted line; written only by
/// [`install`]. The `Arc` is cloned out before the host is called, so a host
/// that re-installs from inside its callback cannot deadlock.
static SINK: RwLock<Option<Arc<dyn CoreLogger>>> = RwLock::new(None);

/// The minimum enabled level, as [`LogLevel::as_u8`]. Default: `Info`.
static MIN_LEVEL: AtomicU8 = AtomicU8::new(2);

/// Installs (`Some`) or removes (`None`) the process-wide sink and sets the
/// most verbose level it receives.
pub(crate) fn install(logger: Option<Arc<dyn CoreLogger>>, min_level: LogLevel) {
    MIN_LEVEL.store(min_level.as_u8(), Ordering::Relaxed);
    *SINK.write().unwrap_or_else(PoisonError::into_inner) = logger;
}

/// Emits `message()` at `level` under `target`, scrubbed, when a sink is
/// installed and `level` is enabled.
pub(crate) fn log(level: LogLevel, target: &str, message: impl FnOnce() -> String) {
    #[cfg(test)]
    if testing::is_capturing() {
        testing::push(level, target, message());
        return;
    }
    if level.as_u8() > MIN_LEVEL.load(Ordering::Relaxed) {
        return;
    }
    let sink = SINK
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(Arc::clone);
    if let Some(sink) = sink {
        sink.log(level, target.to_string(), redact::scrub(&message()));
    }
}

/// [`log`] at [`LogLevel::Error`].
pub(crate) fn error(target: &str, message: impl FnOnce() -> String) {
    log(LogLevel::Error, target, message);
}

/// [`log`] at [`LogLevel::Warn`].
pub(crate) fn warn(target: &str, message: impl FnOnce() -> String) {
    log(LogLevel::Warn, target, message);
}

/// [`log`] at [`LogLevel::Info`].
pub(crate) fn info(target: &str, message: impl FnOnce() -> String) {
    log(LogLevel::Info, target, message);
}

/// [`log`] at [`LogLevel::Debug`].
pub(crate) fn debug(target: &str, message: impl FnOnce() -> String) {
    log(LogLevel::Debug, target, message);
}

/// Test-only capture: a per-thread recorder that takes precedence over the
/// global sink, so parallel tests never see each other's lines. Every
/// `#[tokio::test]` uses a current-thread runtime, so all the code under test
/// logs on the test's own thread.
#[cfg(test)]
pub(crate) mod testing {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{redact, LogLevel};

    /// One captured line, already scrubbed like the host would get it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Record {
        pub(crate) level: LogLevel,
        pub(crate) target: String,
        pub(crate) message: String,
    }

    impl std::fmt::Display for Record {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?} [{}] {}", self.level, self.target, self.message)
        }
    }

    thread_local! {
        static ACTIVE: RefCell<Option<Rc<RefCell<Vec<Record>>>>> = const { RefCell::new(None) };
    }

    /// Records every line logged on this thread (all levels) until dropped.
    pub(crate) struct Capture {
        records: Rc<RefCell<Vec<Record>>>,
    }

    impl Capture {
        pub(crate) fn records(&self) -> Vec<Record> {
            self.records.borrow().clone()
        }

        /// Captured lines of `target` at `level`.
        pub(crate) fn messages(&self, level: LogLevel, target: &str) -> Vec<String> {
            self.records()
                .into_iter()
                .filter(|r| r.level == level && r.target == target)
                .map(|r| r.message)
                .collect()
        }

        /// Every captured line rendered as `Level [target] message`, joined by
        /// newlines, for "no secret anywhere" assertions.
        pub(crate) fn all_text(&self) -> String {
            self.records()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            ACTIVE.with(|active| active.borrow_mut().take());
        }
    }

    /// Starts capturing on this thread.
    pub(crate) fn capture_logs() -> Capture {
        let records = Rc::new(RefCell::new(Vec::new()));
        ACTIVE.with(|active| *active.borrow_mut() = Some(Rc::clone(&records)));
        Capture { records }
    }

    /// Whether this thread is capturing.
    pub(super) fn is_capturing() -> bool {
        ACTIVE.with(|active| active.borrow().is_some())
    }

    /// Records one line on this thread's capture, scrubbed like the host gets it.
    pub(super) fn push(level: LogLevel, target: &str, message: String) {
        ACTIVE.with(|active| {
            if let Some(records) = active.borrow().as_ref() {
                records.borrow_mut().push(Record {
                    level,
                    target: target.to_string(),
                    message: redact::scrub(&message),
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::testing::capture_logs;
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recording {
        lines: Mutex<Vec<(LogLevel, String, String)>>,
    }

    impl CoreLogger for Recording {
        fn log(&self, level: LogLevel, target: String, message: String) {
            self.lines.lock().unwrap().push((level, target, message));
        }
    }

    #[test]
    fn levels_order_from_most_to_least_severe() {
        assert!(LogLevel::Error < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Debug);
        assert_eq!(
            [
                LogLevel::Error,
                LogLevel::Warn,
                LogLevel::Info,
                LogLevel::Debug
            ]
            .map(LogLevel::as_u8),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn capture_records_scrubbed_lines_with_level_and_target() {
        let logs = capture_logs();
        warn(target::HTTP, || {
            "invite ana@example.com with Bearer ya29.secret-token".to_string()
        });
        debug(target::AUTH, || "page 1".to_string());

        let records = logs.records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].level, LogLevel::Warn);
        assert_eq!(records[0].target, "http");
        assert_eq!(
            records[0].message,
            "invite a***@example.com with Bearer [redacted]"
        );
        assert_eq!(logs.messages(LogLevel::Debug, "auth"), ["page 1"]);
    }

    #[test]
    fn capture_ends_when_dropped() {
        let logs = capture_logs();
        drop(logs);
        // No capture and (in this test) whatever global state: must not panic.
        info(target::HTTP, || "after".to_string());
    }

    /// The only test touching the global sink. Other tests on other threads
    /// may log into it while it is installed, so it filters on its own target.
    #[test]
    fn global_sink_filters_by_level_scrubs_and_can_be_removed() {
        const TARGET: &str = "test.global-sink";
        let sink = Arc::new(Recording::default());
        install(Some(sink.clone()), LogLevel::Info);

        let mut formatted_debug = false;
        // Run off the capturing path: this thread has no capture installed.
        error(TARGET, || {
            "boom key=-----BEGIN PRIVATE KEY-----abc-----END PRIVATE KEY-----".into()
        });
        info(TARGET, || "kept".to_string());
        debug(TARGET, || {
            formatted_debug = true;
            "dropped".to_string()
        });

        install(None, LogLevel::Debug);
        info(TARGET, || "after removal".to_string());

        let lines: Vec<_> = sink
            .lines
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, target, _)| target == TARGET)
            .cloned()
            .collect();
        assert_eq!(
            lines,
            vec![
                (
                    LogLevel::Error,
                    TARGET.to_string(),
                    "boom key=[redacted PEM]".to_string()
                ),
                (LogLevel::Info, TARGET.to_string(), "kept".to_string()),
            ]
        );
        assert!(!formatted_debug, "a disabled level must not format");

        // Leave the default level behind for any later test.
        install(None, LogLevel::Info);
    }
}
