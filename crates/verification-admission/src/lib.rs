//! Coordinates one local budget preflight with one durable store reservation.
//!
//! This helper runs before worker handoff. It opens no stores or payload files,
//! starts no worker, and grants no cleanup or lifecycle authority. It commits
//! registry metadata through the existing validated API. The free
//! space value is a trusted caller observation and can become stale before
//! later writes; runtime accounting must still charge observed growth.

use verification_budget::{
    BlockReason, BudgetController, BudgetError, JobId, PER_REPOSITORY_TEMP_LIMIT,
};
use verification_store_registry::{
    RegistryError, StoreRecord, StoreState, VerificationStoreRegistry,
};

/// Bound on retained local budget history before this helper admits another attempt.
pub const MAX_RETAINED_ADMISSIONS: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedAttempt {
    pub job_id: JobId,
    pub record: StoreRecord,
}

#[derive(Debug)]
pub enum AdmissionError {
    InvalidReservation,
    StoreNotFresh,
    JobHistoryLimit,
    Registry(RegistryError),
    BudgetStart(BudgetError),
    BudgetPreflight {
        job_id: JobId,
        source: BudgetError,
        settlement: Option<BudgetError>,
    },
    ReservationRefused {
        job_id: JobId,
        source: RegistryError,
        settlement: Option<BudgetError>,
    },
    ReservationOutcomeUnknown {
        job_id: JobId,
        source: RegistryError,
        settlement: Option<BudgetError>,
    },
    ReservationRecordMismatch {
        job_id: JobId,
        record: StoreRecord,
        settlement: Option<BudgetError>,
    },
}

/// Admit only a fresh Registered row. A successful return means the local
/// budget slot is Running and the exact registry reservation has committed;
/// callers may then bind their fixed worker capability and hand it off.
///
/// Failures before durable reservation settle the local job as stopped when
/// that outcome is known. If the registry result is ambiguous, the local job
/// remains failed with `worker_stopped: false`, preventing another local start
/// until its owner can establish the outcome. A committed reservation is never
/// automatically reversed or cleaned up here.
pub fn admit_registered_store(
    budget: &mut BudgetController,
    registry: &mut VerificationStoreRegistry,
    store_id: &str,
    reservation_bytes: u64,
    free_bytes_before: u64,
) -> Result<AdmittedAttempt, AdmissionError> {
    admit_with_reservation(
        budget,
        registry,
        store_id,
        reservation_bytes,
        free_bytes_before,
        |registry, id, bytes| registry.reserve(id, 0, bytes),
    )
}

// Private injection point exercises uncertain commit outcomes without causing
// real I/O failure or exposing callbacks to backend callers.
fn admit_with_reservation(
    budget: &mut BudgetController,
    registry: &mut VerificationStoreRegistry,
    store_id: &str,
    reservation_bytes: u64,
    free_bytes_before: u64,
    reserve: impl FnOnce(
        &mut VerificationStoreRegistry,
        &str,
        u64,
    ) -> Result<StoreRecord, RegistryError>,
) -> Result<AdmittedAttempt, AdmissionError> {
    if budget.retained_job_count() >= MAX_RETAINED_ADMISSIONS {
        return Err(AdmissionError::JobHistoryLimit);
    }
    if reservation_bytes == 0 || reservation_bytes > PER_REPOSITORY_TEMP_LIMIT {
        return Err(AdmissionError::InvalidReservation);
    }

    let existing = registry
        .get(store_id)
        .map_err(AdmissionError::Registry)?
        .ok_or(AdmissionError::Registry(RegistryError::NotFound))?;
    if existing.state != StoreState::Registered
        || existing.prior_state.is_some()
        || existing.reserved_bytes != 0
    {
        return Err(AdmissionError::StoreNotFresh);
    }

    let job_id = budget
        .start(free_bytes_before)
        .map_err(AdmissionError::BudgetStart)?;
    if let Err(source) = budget.check_store_admission(job_id, reservation_bytes, free_bytes_before)
    {
        let settlement = match &source {
            BudgetError::Blocked(reason) => settle_failed_without_worker(budget, job_id, *reason),
            BudgetError::UnknownJob(_) => None,
        };
        return Err(AdmissionError::BudgetPreflight {
            job_id,
            source,
            settlement,
        });
    }

    let record = match reserve(registry, store_id, reservation_bytes) {
        Ok(record) => record,
        Err(source) if is_ambiguous_registry_outcome(&source) => {
            let settlement = fail_without_stop_ack(budget, job_id, BlockReason::StoreIncomplete);
            return Err(AdmissionError::ReservationOutcomeUnknown {
                job_id,
                source,
                settlement,
            });
        }
        Err(source) => {
            let settlement = cancel_stopped_without_worker(budget, job_id);
            return Err(AdmissionError::ReservationRefused {
                job_id,
                source,
                settlement,
            });
        }
    };

    if record.store_id != existing.store_id
        || record.parent_dev != existing.parent_dev
        || record.parent_ino != existing.parent_ino
        || record.root_dev != existing.root_dev
        || record.root_ino != existing.root_ino
        || record.state != StoreState::Registered
        || record.prior_state.is_some()
        || record.reserved_bytes != reservation_bytes
    {
        let settlement = fail_without_stop_ack(budget, job_id, BlockReason::StoreIncomplete);
        return Err(AdmissionError::ReservationRecordMismatch {
            job_id,
            record,
            settlement,
        });
    }

    Ok(AdmittedAttempt { job_id, record })
}

fn settle_failed_without_worker(
    budget: &mut BudgetController,
    job_id: JobId,
    reason: BlockReason,
) -> Option<BudgetError> {
    budget
        .fail(job_id, reason)
        .err()
        .or_else(|| budget.acknowledge_worker_stopped(job_id).err())
}

fn cancel_stopped_without_worker(
    budget: &mut BudgetController,
    job_id: JobId,
) -> Option<BudgetError> {
    budget
        .cancel(job_id)
        .err()
        .or_else(|| budget.acknowledge_worker_stopped(job_id).err())
}

fn fail_without_stop_ack(
    budget: &mut BudgetController,
    job_id: JobId,
    reason: BlockReason,
) -> Option<BudgetError> {
    budget.fail(job_id, reason).err()
}

fn is_ambiguous_registry_outcome(error: &RegistryError) -> bool {
    matches!(
        error,
        RegistryError::Storage
            | RegistryError::UnsafePath
            | RegistryError::UnsupportedSchema
            | RegistryError::LeaseHeld
    )
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};
    use verification_budget::{JobState, Limits, MINIMUM_FREE_RESERVE};

    const FREE_HEADROOM: u64 = 1024 * 1024;
    const RESERVATION: u64 = 64 * 1024;

    fn setup() -> (tempfile::TempDir, VerificationStoreRegistry, String) {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let registry =
            VerificationStoreRegistry::open(dir.path().join("registry.sqlite3")).unwrap();
        let id = "0123456789abcdef0123456789abcdef".to_owned();
        (dir, registry, id)
    }

    fn budget() -> BudgetController {
        BudgetController::new(Limits {
            headroom_bytes: FREE_HEADROOM,
        })
    }

    fn free_for(bytes: u64) -> u64 {
        MINIMUM_FREE_RESERVE + FREE_HEADROOM + bytes
    }

    fn register(registry: &mut VerificationStoreRegistry, id: &str) {
        registry.register(id, 1, 2, 3, 4).unwrap();
    }

    #[test]
    fn success_returns_only_after_exact_reservation_and_running_local_job() {
        let (_dir, mut registry, id) = setup();
        register(&mut registry, &id);
        let mut budget = budget();
        let result = admit_registered_store(
            &mut budget,
            &mut registry,
            &id,
            RESERVATION,
            free_for(RESERVATION),
        )
        .unwrap();
        assert_eq!(result.record.state, StoreState::Registered);
        assert_eq!(result.record.reserved_bytes, RESERVATION);
        assert_eq!(result.job_id, JobId(1));
        let job = budget.snapshot(result.job_id).unwrap();
        assert_eq!(job.state, JobState::Running);
        assert_eq!(job.usage.stored_bytes, 0);
        assert_eq!(registry.get(&id).unwrap().unwrap(), result.record);
    }

    #[test]
    fn running_job_blocks_another_fresh_store_without_reserving_it() {
        let (_dir, mut registry, id) = setup();
        register(&mut registry, &id);
        let other_id = "0123456789abcdef0123456789abcde0";
        register(&mut registry, other_id);
        let mut budget = budget();
        let first = admit_registered_store(
            &mut budget,
            &mut registry,
            &id,
            RESERVATION,
            free_for(RESERVATION),
        )
        .unwrap();
        let before = registry.get(other_id).unwrap().unwrap();
        assert!(matches!(
            admit_registered_store(
                &mut budget,
                &mut registry,
                other_id,
                RESERVATION,
                free_for(RESERVATION)
            ),
            Err(AdmissionError::BudgetStart(BudgetError::Blocked(
                BlockReason::AlreadyAtConcurrencyLimit
            )))
        ));
        assert_eq!(budget.retained_job_count(), 1);
        assert_eq!(
            budget.snapshot(first.job_id).unwrap().state,
            JobState::Running
        );
        assert_eq!(registry.get(other_id).unwrap().unwrap(), before);
    }

    #[test]
    fn free_space_preflight_refuses_without_registry_mutation() {
        let (_dir, mut registry, id) = setup();
        register(&mut registry, &id);
        let mut no_slot_budget = budget();
        assert!(matches!(
            admit_registered_store(
                &mut no_slot_budget,
                &mut registry,
                &id,
                RESERVATION,
                MINIMUM_FREE_RESERVE + FREE_HEADROOM - 1
            ),
            Err(AdmissionError::BudgetStart(BudgetError::Blocked(
                BlockReason::FreeSpaceReserve
            )))
        ));
        assert_eq!(no_slot_budget.retained_job_count(), 0);
        assert_eq!(registry.get(&id).unwrap().unwrap().reserved_bytes, 0);

        let mut budget = budget();
        let error = admit_registered_store(
            &mut budget,
            &mut registry,
            &id,
            RESERVATION,
            free_for(RESERVATION) - 1,
        )
        .unwrap_err();
        assert!(matches!(error, AdmissionError::BudgetPreflight { .. }));
        assert_eq!(registry.get(&id).unwrap().unwrap().reserved_bytes, 0);
        let job = budget.snapshot(JobId(1)).unwrap();
        assert_eq!(
            job.state,
            JobState::Failed {
                reason: BlockReason::FreeSpaceReserve,
                worker_stopped: true,
            }
        );
    }

    #[test]
    fn registry_refusal_settles_local_slot_without_cleanup() {
        let (_dir, mut registry, id) = setup();
        register(&mut registry, &id);
        let other = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        register(&mut registry, other);
        registry
            .require_reconciliation(other, StoreState::Registered)
            .unwrap();
        let mut budget = budget();
        assert!(matches!(
            admit_registered_store(
                &mut budget,
                &mut registry,
                &id,
                RESERVATION,
                free_for(RESERVATION)
            ),
            Err(AdmissionError::ReservationRefused {
                source: RegistryError::ReconciliationRequired,
                ..
            })
        ));
        assert_eq!(registry.get(&id).unwrap().unwrap().reserved_bytes, 0);
        assert_eq!(
            budget.snapshot(JobId(1)).unwrap().state,
            JobState::Canceled {
                cleanup_confirmed: false,
                worker_stopped: true,
            }
        );
        assert!(budget.start(free_for(RESERVATION)).is_ok());
    }

    #[test]
    fn aggregate_reservation_refusal_settles_local_slot_without_durable_target_change() {
        let (_dir, mut registry, id) = setup();
        register(&mut registry, &id);
        let a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        register(&mut registry, a);
        register(&mut registry, b);
        registry.reserve(a, 0, PER_REPOSITORY_TEMP_LIMIT).unwrap();
        registry.reserve(b, 0, PER_REPOSITORY_TEMP_LIMIT).unwrap();
        let mut budget = budget();
        assert!(matches!(
            admit_registered_store(
                &mut budget,
                &mut registry,
                &id,
                RESERVATION,
                free_for(RESERVATION)
            ),
            Err(AdmissionError::ReservationRefused {
                source: RegistryError::ReservationLimit,
                ..
            })
        ));
        assert_eq!(registry.get(&id).unwrap().unwrap().reserved_bytes, 0);
        assert_eq!(
            budget.snapshot(JobId(1)).unwrap().state,
            JobState::Canceled {
                cleanup_confirmed: false,
                worker_stopped: true,
            }
        );
    }

    #[test]
    fn invalid_or_nonfresh_rows_refuse_before_budget_start() {
        let (_dir, mut registry, id) = setup();
        register(&mut registry, &id);
        let mut budget = budget();
        assert!(matches!(
            admit_registered_store(&mut budget, &mut registry, &id, 0, u64::MAX),
            Err(AdmissionError::InvalidReservation)
        ));
        assert!(matches!(
            admit_registered_store(
                &mut budget,
                &mut registry,
                "not-an-id",
                RESERVATION,
                u64::MAX
            ),
            Err(AdmissionError::Registry(RegistryError::InvalidInput))
        ));
        assert!(matches!(
            admit_registered_store(
                &mut budget,
                &mut registry,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                RESERVATION,
                u64::MAX
            ),
            Err(AdmissionError::Registry(RegistryError::NotFound))
        ));
        registry.reserve(&id, 0, RESERVATION).unwrap();
        assert!(matches!(
            admit_registered_store(&mut budget, &mut registry, &id, RESERVATION / 2, u64::MAX),
            Err(AdmissionError::StoreNotFresh)
        ));
        assert_eq!(budget.retained_job_count(), 0);

        let active = "cccccccccccccccccccccccccccccccc";
        register(&mut registry, active);
        registry
            .transition(active, StoreState::Registered, StoreState::Active)
            .unwrap();
        assert!(matches!(
            admit_registered_store(&mut budget, &mut registry, active, RESERVATION, u64::MAX),
            Err(AdmissionError::StoreNotFresh)
        ));
        assert_eq!(budget.retained_job_count(), 0);

        let reconciliation = "dddddddddddddddddddddddddddddddd";
        register(&mut registry, reconciliation);
        registry
            .require_reconciliation(reconciliation, StoreState::Registered)
            .unwrap();
        assert!(matches!(
            admit_registered_store(
                &mut budget,
                &mut registry,
                reconciliation,
                RESERVATION,
                u64::MAX
            ),
            Err(AdmissionError::StoreNotFresh)
        ));
        let completed = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        register(&mut registry, completed);
        registry
            .transition(completed, StoreState::Registered, StoreState::Active)
            .unwrap();
        registry
            .transition(completed, StoreState::Active, StoreState::Stopped)
            .unwrap();
        registry
            .transition(completed, StoreState::Stopped, StoreState::CleanupPending)
            .unwrap();
        registry
            .transition(
                completed,
                StoreState::CleanupPending,
                StoreState::CleanupComplete,
            )
            .unwrap();
        assert!(matches!(
            admit_registered_store(&mut budget, &mut registry, completed, RESERVATION, u64::MAX),
            Err(AdmissionError::StoreNotFresh)
        ));
        assert_eq!(budget.retained_job_count(), 0);
    }

    #[test]
    fn ambiguous_reservation_failure_retains_the_local_slot() {
        let (_dir, mut registry, id) = setup();
        register(&mut registry, &id);
        let before = registry.get(&id).unwrap().unwrap();
        let mut budget = budget();
        let result = admit_with_reservation(
            &mut budget,
            &mut registry,
            &id,
            RESERVATION,
            free_for(RESERVATION),
            |_, _, _| Err(RegistryError::Storage),
        );
        let job_id = match result {
            Err(AdmissionError::ReservationOutcomeUnknown {
                job_id,
                source: RegistryError::Storage,
                settlement: None,
            }) => job_id,
            other => panic!("unexpected result: {other:?}"),
        };
        assert_eq!(registry.get(&id).unwrap().unwrap(), before);
        assert_eq!(
            budget.snapshot(job_id).unwrap().state,
            JobState::Failed {
                reason: BlockReason::StoreIncomplete,
                worker_stopped: false,
            }
        );
        assert!(matches!(
            budget.start(u64::MAX),
            Err(BudgetError::Blocked(BlockReason::AlreadyAtConcurrencyLimit))
        ));
    }

    #[test]
    fn committed_reservation_is_retained_when_the_returned_record_mismatches() {
        let (_dir, mut registry, id) = setup();
        register(&mut registry, &id);
        let mut budget = budget();
        let result = admit_with_reservation(
            &mut budget,
            &mut registry,
            &id,
            RESERVATION,
            free_for(RESERVATION),
            |registry, id, bytes| {
                let mut returned = registry.reserve(id, 0, bytes)?;
                returned.root_ino += 1;
                Ok(returned)
            },
        );
        let job_id = match result {
            Err(AdmissionError::ReservationRecordMismatch {
                job_id,
                settlement: None,
                ..
            }) => job_id,
            other => panic!("unexpected result: {other:?}"),
        };
        let retained = registry.get(&id).unwrap().unwrap();
        assert_eq!(retained.reserved_bytes, RESERVATION);
        assert_eq!(retained.root_ino, 4);
        assert_eq!(
            budget.snapshot(job_id).unwrap().state,
            JobState::Failed {
                reason: BlockReason::StoreIncomplete,
                worker_stopped: false,
            }
        );
        assert!(matches!(
            budget.start(u64::MAX),
            Err(BudgetError::Blocked(BlockReason::AlreadyAtConcurrencyLimit))
        ));
    }

    #[test]
    fn retained_history_limit_is_checked_before_start_or_registry_change() {
        let (_dir, mut registry, id) = setup();
        register(&mut registry, &id);
        let mut budget = budget();
        for _ in 0..MAX_RETAINED_ADMISSIONS {
            let job = budget.start(u64::MAX).unwrap();
            budget.complete(job).unwrap();
        }
        assert!(matches!(
            admit_registered_store(&mut budget, &mut registry, &id, RESERVATION, u64::MAX),
            Err(AdmissionError::JobHistoryLimit)
        ));
        assert_eq!(budget.retained_job_count(), MAX_RETAINED_ADMISSIONS);
        assert_eq!(registry.get(&id).unwrap().unwrap().reserved_bytes, 0);
    }
}
