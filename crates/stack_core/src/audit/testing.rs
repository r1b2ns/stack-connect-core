//! Test support: a [`NetworkAuditSink`] that keeps every entry.

use std::sync::{Arc, Mutex};

use super::scope::AuditTarget;
use super::types::{NetworkAuditEntry, NetworkAuditProvider};
use crate::ports::NetworkAuditSink;

/// Records every entry; `capture_bodies` answers `capture`.
pub(crate) struct RecordingSink {
    capture: bool,
    entries: Mutex<Vec<NetworkAuditEntry>>,
}

impl RecordingSink {
    pub(crate) fn new(capture: bool) -> Arc<Self> {
        Arc::new(Self {
            capture,
            entries: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn entries(&self) -> Vec<NetworkAuditEntry> {
        self.entries.lock().unwrap().clone()
    }

    /// A target for this sink.
    pub(crate) fn target(
        self: &Arc<Self>,
        account_id: Option<&str>,
        provider: NetworkAuditProvider,
    ) -> AuditTarget {
        let sink: Arc<dyn NetworkAuditSink> = self.clone();
        AuditTarget::new(Some(sink), account_id, provider).unwrap()
    }
}

impl NetworkAuditSink for RecordingSink {
    fn record(&self, entry: NetworkAuditEntry) {
        self.entries.lock().unwrap().push(entry);
    }

    fn capture_bodies(&self) -> bool {
        self.capture
    }
}

/// A sink whose every method panics.
pub(crate) struct PanickingSink;

impl NetworkAuditSink for PanickingSink {
    fn record(&self, _entry: NetworkAuditEntry) {
        panic!("sink failure");
    }

    fn capture_bodies(&self) -> bool {
        panic!("sink failure");
    }
}
