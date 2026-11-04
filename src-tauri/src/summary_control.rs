use disk_steward_core::{InventoryItemId, SummaryAttemptId};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

#[derive(Clone, Default)]
pub(crate) struct SummaryControlState(Arc<Mutex<Option<ActiveSummary>>>);

struct ActiveSummary {
    observation_id: InventoryItemId,
    attempt_id: SummaryAttemptId,
    cancellation: Arc<AtomicBool>,
}

pub(crate) struct SummaryRegistration {
    state: Arc<Mutex<Option<ActiveSummary>>>,
    observation_id: InventoryItemId,
    attempt_id: SummaryAttemptId,
    cancellation: Arc<AtomicBool>,
}

impl SummaryControlState {
    pub(crate) fn register(
        &self,
        observation_id: InventoryItemId,
        attempt_id: SummaryAttemptId,
    ) -> Result<SummaryRegistration, &'static str> {
        let mut active = self.0.lock().map_err(|_| "summary_control_unavailable")?;
        if active.is_some() {
            return Err("summary_already_active");
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        *active = Some(ActiveSummary {
            observation_id,
            attempt_id,
            cancellation: Arc::clone(&cancellation),
        });
        Ok(SummaryRegistration {
            state: Arc::clone(&self.0),
            observation_id,
            attempt_id,
            cancellation,
        })
    }

    pub(crate) fn cancel(
        &self,
        observation_id: InventoryItemId,
        attempt_id: SummaryAttemptId,
    ) -> Result<(), &'static str> {
        let active = self.0.lock().map_err(|_| "summary_control_unavailable")?;
        let Some(active) = active.as_ref() else {
            return Err("summary_not_active");
        };
        if active.observation_id != observation_id || active.attempt_id != attempt_id {
            return Err("summary_attempt_stale");
        }
        active.cancellation.store(true, Ordering::Release);
        Ok(())
    }
}

impl SummaryRegistration {
    pub(crate) fn cancellation_flag(&self) -> &AtomicBool {
        &self.cancellation
    }

    #[cfg(test)]
    fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }
}

impl Drop for SummaryRegistration {
    fn drop(&mut self) {
        let Ok(mut active) = self.state.lock() else {
            return;
        };
        if active.as_ref().is_some_and(|active| {
            active.observation_id == self.observation_id
                && active.attempt_id == self.attempt_id
                && Arc::ptr_eq(&active.cancellation, &self.cancellation)
        }) {
            active.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_active_pair_can_cancel_and_stale_pair_cannot_cancel_next_attempt() {
        let state = SummaryControlState::default();
        let observation_id = InventoryItemId::new();
        let old_attempt = SummaryAttemptId::new();
        let first = state.register(observation_id, old_attempt).unwrap();
        assert!(state
            .cancel(observation_id, SummaryAttemptId::new())
            .is_err());
        assert!(!first.is_cancelled());
        state.cancel(observation_id, old_attempt).unwrap();
        assert!(first.is_cancelled());
        drop(first);

        let new_attempt = SummaryAttemptId::new();
        let second = state.register(observation_id, new_attempt).unwrap();
        assert!(state.cancel(observation_id, old_attempt).is_err());
        assert!(!second.is_cancelled());
        state.cancel(observation_id, new_attempt).unwrap();
        assert!(second.is_cancelled());
    }

    #[test]
    fn registration_is_exclusive_and_drop_releases_only_its_own_slot() {
        let state = SummaryControlState::default();
        let observation_id = InventoryItemId::new();
        let attempt_id = SummaryAttemptId::new();
        let active = state.register(observation_id, attempt_id).unwrap();
        assert!(state
            .register(observation_id, SummaryAttemptId::new())
            .is_err());
        assert!(state.cancel(InventoryItemId::new(), attempt_id).is_err());
        assert!(!active.is_cancelled());
        drop(active);
        assert!(state.cancel(observation_id, attempt_id).is_err());
        assert!(state
            .register(observation_id, SummaryAttemptId::new())
            .is_ok());
    }
}
