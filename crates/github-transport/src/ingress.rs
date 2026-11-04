//! Read-only live ingress telemetry for one transport attempt.

use crate::{attempt_context::AttemptContext, safe_error, PackTransferReport};
use std::{
    io,
    sync::{Arc, Mutex},
};

/// Read-only access to cumulative socket bytes observed beneath TLS for one
/// transport attempt. This telemetry provides no limits setter, network,
/// filesystem, credential, transport-control, or cleanup access.
#[derive(Clone)]
pub struct TransportIngressObserver {
    used: Arc<Mutex<u64>>,
    context: AttemptContext,
}

impl TransportIngressObserver {
    pub(crate) fn new(used: Arc<Mutex<u64>>, context: AttemptContext) -> Self {
        Self { used, context }
    }

    /// Return the cumulative ingress total observed so far.
    ///
    /// This snapshot may wait for an in-flight transport read because the
    /// counter mutex is held during that read. The transport's own deadline or
    /// cancellation checks govern that read; this is not a nonblocking API or
    /// an OS scheduling/latency guarantee.
    pub fn observed_bytes(&self) -> io::Result<u64> {
        self.used
            .lock()
            .map(|used| *used)
            .map_err(|_| safe_error(io::ErrorKind::Other, "ingress observation unavailable"))
    }

    /// Try to return the cumulative ingress total without waiting for the
    /// counter mutex. `None` means the counter is busy; it says nothing about
    /// whether bytes are zero or whether the attempt is complete. This method
    /// does not retry and makes no guarantee about operating-system latency.
    pub fn try_observed_bytes(&self) -> io::Result<Option<u64>> {
        match self.used.try_lock() {
            Ok(used) => Ok(Some(*used)),
            Err(std::sync::TryLockError::WouldBlock) => Ok(None),
            Err(std::sync::TryLockError::Poisoned(_)) => Err(safe_error(
                io::ErrorKind::Other,
                "ingress observation unavailable",
            )),
        }
    }

    /// Whether `report` belongs to this exact historical attempt identity.
    /// This does not establish freshness, transfer completion, or approval.
    pub fn matches_transfer(&self, report: &PackTransferReport) -> bool {
        self.context == *report.context()
    }
}
