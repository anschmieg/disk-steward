//! Pure accounting and state transitions for bounded Git verification.
//!
//! This crate performs no I/O and does not implement a transport, worker, or
//! cleanup. Callers must report observed ingress, storage, free space, elapsed
//! time, and confirmed cleanup. Source repositories are never represented as
//! writable resources here.

use std::collections::BTreeMap;

pub const GIB: u64 = 1024 * 1024 * 1024;
pub const PER_REPOSITORY_INGRESS_LIMIT: u64 = 2 * GIB;
pub const PER_REPOSITORY_TEMP_LIMIT: u64 = 2 * GIB;
pub const AGGREGATE_STORE_LIMIT: u64 = 4 * GIB;
pub const MINIMUM_FREE_RESERVE: u64 = 2 * GIB;
pub const MAX_ATTEMPT_MILLIS: u64 = 5 * 60 * 1000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct JobId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockReason {
    AlreadyAtConcurrencyLimit,
    IngressLimit,
    RepositoryStorageLimit,
    AggregateStorageLimit,
    FreeSpaceReserve,
    ArithmeticOverflow,
    AttemptTimedOut,
    JobNotRunning,
    /// The cumulative ingress counter moved backwards during an attempt.
    IngressCounterRegressed,
    /// A caller could not finish a store operation or observe its outcome.
    StoreIncomplete,
    CleanupNotPending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobState {
    Running,
    Failed {
        reason: BlockReason,
        worker_stopped: bool,
    },
    Canceled {
        cleanup_confirmed: bool,
        worker_stopped: bool,
    },
    Completed {
        cleanup_confirmed: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Usage {
    pub ingress_bytes: u64,
    /// Bytes currently present in the app-owned temporary store, including
    /// partial data left by a failed or canceled attempt.
    pub stored_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobSnapshot {
    pub id: JobId,
    pub state: JobState,
    pub usage: Usage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Extra free-space headroom for journaling/checkpoints, in addition to the
    /// fixed minimum reserve.
    pub headroom_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BudgetError {
    Blocked(BlockReason),
    UnknownJob(JobId),
}

#[derive(Clone, Debug)]
pub struct BudgetController {
    limits: Limits,
    next_id: u64,
    jobs: BTreeMap<JobId, JobSnapshot>,
}

impl BudgetController {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            next_id: 1,
            jobs: BTreeMap::new(),
        }
    }

    /// Preflight and reserve one running slot. `free_bytes` is the observed
    /// volume free space before the caller creates a store.
    pub fn start(&mut self, free_bytes: u64) -> Result<JobId, BudgetError> {
        if self.jobs.values().any(|j| match j.state {
            JobState::Running => true,
            JobState::Failed { worker_stopped, .. } | JobState::Canceled { worker_stopped, .. } => {
                !worker_stopped
            }
            JobState::Completed { .. } => false,
        }) {
            return Err(BudgetError::Blocked(BlockReason::AlreadyAtConcurrencyLimit));
        }
        let protected = MINIMUM_FREE_RESERVE
            .checked_add(self.limits.headroom_bytes)
            .ok_or(BudgetError::Blocked(BlockReason::ArithmeticOverflow))?;
        if free_bytes < protected {
            return Err(BudgetError::Blocked(BlockReason::FreeSpaceReserve));
        }
        let id = JobId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(BudgetError::Blocked(BlockReason::ArithmeticOverflow))?;
        self.jobs.insert(
            id,
            JobSnapshot {
                id,
                state: JobState::Running,
                usage: Usage {
                    ingress_bytes: 0,
                    stored_bytes: 0,
                },
            },
        );
        Ok(id)
    }

    /// Account a received network chunk. Charge observed bytes even when they
    /// exceed the ingress limit or the counter overflows. Observations remain
    /// admissible after failure or cancellation while the worker may still be
    /// receiving data; stopped and completed jobs reject further observations.
    pub fn record_ingress(&mut self, id: JobId, bytes: u64) -> Result<(), BudgetError> {
        let job = self.jobs.get_mut(&id).ok_or(BudgetError::UnknownJob(id))?;
        let may_still_receive = matches!(
            job.state,
            JobState::Running
                | JobState::Failed {
                    worker_stopped: false,
                    ..
                }
                | JobState::Canceled {
                    worker_stopped: false,
                    ..
                }
        );
        if !may_still_receive {
            return Err(BudgetError::Blocked(BlockReason::JobNotRunning));
        }
        let overflow = job.usage.ingress_bytes.checked_add(bytes).is_none();
        job.usage.ingress_bytes = job.usage.ingress_bytes.saturating_add(bytes);
        let reason = if overflow {
            Some(BlockReason::ArithmeticOverflow)
        } else if job.usage.ingress_bytes > PER_REPOSITORY_INGRESS_LIMIT {
            Some(BlockReason::IngressLimit)
        } else {
            None
        };
        if let Some(reason) = reason {
            if job.state == JobState::Running {
                job.state = JobState::Failed {
                    reason,
                    worker_stopped: false,
                };
            }
            return Err(BudgetError::Blocked(reason));
        }
        Ok(())
    }

    /// Account a cumulative transport ingress observation for the same job
    /// attempt and counter domain. Only the newly observed delta is charged,
    /// so repeating a total never charges it twice. The caller is responsible for
    /// binding totals to the correct job, attempt, and counter domain; do not
    /// mix incompatible totals or chunk deltas for one job.
    ///
    /// Totals may still be charged after failure or cancellation while the
    /// worker may be receiving data. A regressed total leaves usage unchanged;
    /// it fails a running job with `IngressCounterRegressed`, while preserving
    /// any earlier failed or canceled state. This method does not acknowledge
    /// worker stop or authorize cleanup.
    pub fn record_ingress_total(&mut self, id: JobId, total: u64) -> Result<(), BudgetError> {
        let job = self.jobs.get_mut(&id).ok_or(BudgetError::UnknownJob(id))?;
        let may_still_receive = matches!(
            job.state,
            JobState::Running
                | JobState::Failed {
                    worker_stopped: false,
                    ..
                }
                | JobState::Canceled {
                    worker_stopped: false,
                    ..
                }
        );
        if !may_still_receive {
            return Err(BudgetError::Blocked(BlockReason::JobNotRunning));
        }

        if total < job.usage.ingress_bytes {
            if job.state == JobState::Running {
                job.state = JobState::Failed {
                    reason: BlockReason::IngressCounterRegressed,
                    worker_stopped: false,
                };
            }
            return Err(BudgetError::Blocked(BlockReason::IngressCounterRegressed));
        }

        let delta = total - job.usage.ingress_bytes;
        self.record_ingress(id, delta)
    }

    /// Check whether a running job may attempt a proposed store growth delta.
    /// This is advisory only: it reserves no capacity and does not change job
    /// state or usage. Callers must still account bytes actually observed with
    /// `record_store`, including partial writes and growth that exceeds limits.
    /// `free_bytes_before` is the observed free space before the proposed write.
    pub fn check_store_admission(
        &self,
        id: JobId,
        proposed_growth_delta: u64,
        free_bytes_before: u64,
    ) -> Result<(), BudgetError> {
        let job = self.jobs.get(&id).ok_or(BudgetError::UnknownJob(id))?;
        if job.state != JobState::Running {
            return Err(BudgetError::Blocked(BlockReason::JobNotRunning));
        }

        let repository_next = job
            .usage
            .stored_bytes
            .checked_add(proposed_growth_delta)
            .ok_or(BudgetError::Blocked(BlockReason::ArithmeticOverflow))?;
        if repository_next > PER_REPOSITORY_TEMP_LIMIT {
            return Err(BudgetError::Blocked(BlockReason::RepositoryStorageLimit));
        }

        let aggregate_before = self.jobs.values().try_fold(0u64, |sum, j| {
            sum.checked_add(j.usage.stored_bytes)
                .ok_or(BudgetError::Blocked(BlockReason::ArithmeticOverflow))
        })?;
        let aggregate_next = aggregate_before
            .checked_add(proposed_growth_delta)
            .ok_or(BudgetError::Blocked(BlockReason::ArithmeticOverflow))?;
        if aggregate_next > AGGREGATE_STORE_LIMIT {
            return Err(BudgetError::Blocked(BlockReason::AggregateStorageLimit));
        }

        let required_free = MINIMUM_FREE_RESERVE
            .checked_add(self.limits.headroom_bytes)
            .and_then(|reserve| reserve.checked_add(proposed_growth_delta))
            .ok_or(BudgetError::Blocked(BlockReason::ArithmeticOverflow))?;
        if free_bytes_before < required_free {
            return Err(BudgetError::Blocked(BlockReason::FreeSpaceReserve));
        }
        Ok(())
    }

    /// Account bytes observed in a temporary store. The observation is charged
    /// even when it exceeds a limit or violates the free-space reserve. Supply
    /// actual growth, not a proposed reservation. Saturation preserves a
    /// conservative upper bound if the byte counter itself overflows.
    pub fn record_store(
        &mut self,
        id: JobId,
        bytes: u64,
        free_bytes_after: u64,
    ) -> Result<(), BudgetError> {
        let reserve = MINIMUM_FREE_RESERVE.checked_add(self.limits.headroom_bytes);
        let aggregate_before = self
            .jobs
            .values()
            .fold(0u64, |sum, j| sum.saturating_add(j.usage.stored_bytes));
        let job = self.jobs.get_mut(&id).ok_or(BudgetError::UnknownJob(id))?;
        let may_still_write = matches!(
            job.state,
            JobState::Running
                | JobState::Failed {
                    worker_stopped: false,
                    ..
                }
                | JobState::Canceled {
                    worker_stopped: false,
                    ..
                }
        );
        if !may_still_write {
            return Err(BudgetError::Blocked(BlockReason::JobNotRunning));
        }
        let repo_overflow = job.usage.stored_bytes.checked_add(bytes).is_none();
        let repo_next = job.usage.stored_bytes.saturating_add(bytes);
        let aggregate_overflow = aggregate_before.checked_add(bytes).is_none();
        let aggregate_next = aggregate_before.saturating_add(bytes);
        job.usage.stored_bytes = repo_next;
        let reason = if repo_overflow || aggregate_overflow {
            Some(BlockReason::ArithmeticOverflow)
        } else if repo_next > PER_REPOSITORY_TEMP_LIMIT {
            Some(BlockReason::RepositoryStorageLimit)
        } else if aggregate_next > AGGREGATE_STORE_LIMIT {
            Some(BlockReason::AggregateStorageLimit)
        } else if reserve.is_none() {
            Some(BlockReason::ArithmeticOverflow)
        } else if free_bytes_after < reserve.unwrap() {
            Some(BlockReason::FreeSpaceReserve)
        } else {
            None
        };
        if let Some(reason) = reason {
            if job.state == JobState::Running {
                job.state = JobState::Failed {
                    reason,
                    worker_stopped: false,
                };
            }
            return Err(BudgetError::Blocked(reason));
        }
        Ok(())
    }

    /// Check caller-supplied monotonic elapsed time. No wall clock is consulted.
    pub fn check_elapsed(&mut self, id: JobId, elapsed_millis: u64) -> Result<(), BudgetError> {
        let job = self.running_mut(id)?;
        if elapsed_millis > MAX_ATTEMPT_MILLIS {
            return Self::block_job(job, BlockReason::AttemptTimedOut);
        }
        Ok(())
    }

    pub fn cancel(&mut self, id: JobId) -> Result<(), BudgetError> {
        let job = self.running_mut(id)?;
        job.state = JobState::Canceled {
            cleanup_confirmed: false,
            worker_stopped: false,
        };
        Ok(())
    }

    pub fn fail(&mut self, id: JobId, reason: BlockReason) -> Result<(), BudgetError> {
        let job = self.running_mut(id)?;
        job.state = JobState::Failed {
            reason,
            worker_stopped: false,
        };
        Ok(())
    }

    pub fn complete(&mut self, id: JobId) -> Result<(), BudgetError> {
        let job = self.running_mut(id)?;
        job.state = JobState::Completed {
            cleanup_confirmed: false,
        };
        Ok(())
    }

    /// Acknowledge that the worker can no longer write to the app-owned store.
    /// Failed and canceled attempts retain the concurrency slot until this call.
    pub fn acknowledge_worker_stopped(&mut self, id: JobId) -> Result<(), BudgetError> {
        let job = self.jobs.get_mut(&id).ok_or(BudgetError::UnknownJob(id))?;
        match job.state {
            JobState::Failed { reason, .. } => {
                job.state = JobState::Failed {
                    reason,
                    worker_stopped: true,
                }
            }
            JobState::Canceled {
                cleanup_confirmed, ..
            } => {
                job.state = JobState::Canceled {
                    cleanup_confirmed,
                    worker_stopped: true,
                }
            }
            _ => return Err(BudgetError::Blocked(BlockReason::CleanupNotPending)),
        }
        Ok(())
    }

    /// Release accounted store bytes only after the caller confirms that the
    /// owned partial/full store has been removed or otherwise no longer exists.
    pub fn confirm_cleanup(&mut self, id: JobId) -> Result<(), BudgetError> {
        let job = self.jobs.get_mut(&id).ok_or(BudgetError::UnknownJob(id))?;
        match job.state {
            JobState::Canceled {
                worker_stopped: false,
                ..
            }
            | JobState::Failed {
                worker_stopped: false,
                ..
            } => {
                return Err(BudgetError::Blocked(BlockReason::CleanupNotPending));
            }
            JobState::Canceled {
                worker_stopped: true,
                ..
            } => {
                job.state = JobState::Canceled {
                    cleanup_confirmed: true,
                    worker_stopped: true,
                }
            }
            JobState::Failed { .. } => {
                job.usage.stored_bytes = 0;
                return Ok(());
            }
            JobState::Completed { .. } => {
                job.state = JobState::Completed {
                    cleanup_confirmed: true,
                }
            }
            JobState::Running => return Err(BudgetError::Blocked(BlockReason::CleanupNotPending)),
        }
        job.usage.stored_bytes = 0;
        Ok(())
    }

    pub fn snapshot(&self, id: JobId) -> Result<&JobSnapshot, BudgetError> {
        self.jobs.get(&id).ok_or(BudgetError::UnknownJob(id))
    }

    /// Number of retained attempt records, including completed and cleaned jobs.
    pub fn retained_job_count(&self) -> usize {
        self.jobs.len()
    }

    pub fn aggregate_stored(&self) -> Result<u64, BudgetError> {
        Ok(self
            .jobs
            .values()
            .fold(0u64, |sum, j| sum.saturating_add(j.usage.stored_bytes)))
    }

    fn running_mut(&mut self, id: JobId) -> Result<&mut JobSnapshot, BudgetError> {
        let job = self.jobs.get_mut(&id).ok_or(BudgetError::UnknownJob(id))?;
        if job.state != JobState::Running {
            return Err(BudgetError::Blocked(BlockReason::JobNotRunning));
        }
        Ok(job)
    }

    fn block_job<T>(job: &mut JobSnapshot, reason: BlockReason) -> Result<T, BudgetError> {
        job.state = JobState::Failed {
            reason,
            worker_stopped: false,
        };
        Err(BudgetError::Blocked(reason))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn controller() -> BudgetController {
        BudgetController::new(Limits {
            headroom_bytes: 512 * 1024 * 1024,
        })
    }
    fn free() -> u64 {
        MINIMUM_FREE_RESERVE + 512 * 1024 * 1024
    }

    #[test]
    fn permits_exact_limits_and_one_running_job() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        assert_eq!(
            b.start(free()),
            Err(BudgetError::Blocked(BlockReason::AlreadyAtConcurrencyLimit))
        );
        b.record_ingress(id, PER_REPOSITORY_INGRESS_LIMIT).unwrap();
        b.record_store(id, PER_REPOSITORY_TEMP_LIMIT, free())
            .unwrap();
        assert_eq!(b.aggregate_stored().unwrap(), PER_REPOSITORY_TEMP_LIMIT);
        b.check_elapsed(id, MAX_ATTEMPT_MILLIS).unwrap();
    }

    #[test]
    fn rejects_oversize_and_timeout_with_explicit_block() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        assert_eq!(
            b.record_ingress(id, PER_REPOSITORY_INGRESS_LIMIT + 1),
            Err(BudgetError::Blocked(BlockReason::IngressLimit))
        );
        assert_eq!(
            b.snapshot(id).unwrap().state,
            JobState::Failed {
                reason: BlockReason::IngressLimit,
                worker_stopped: false
            }
        );
        b.acknowledge_worker_stopped(id).unwrap();
        let id2 = b.start(free()).unwrap();
        assert_eq!(
            b.check_elapsed(id2, MAX_ATTEMPT_MILLIS + 1),
            Err(BudgetError::Blocked(BlockReason::AttemptTimedOut))
        );
    }

    #[test]
    fn excess_observed_store_bytes_remain_charged_until_cleanup() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        assert_eq!(
            b.record_store(id, PER_REPOSITORY_TEMP_LIMIT + 1, free()),
            Err(BudgetError::Blocked(BlockReason::RepositoryStorageLimit))
        );
        assert_eq!(
            b.record_store(id, 100, free()),
            Err(BudgetError::Blocked(BlockReason::RepositoryStorageLimit))
        );
        assert_eq!(
            b.snapshot(id).unwrap().usage.stored_bytes,
            PER_REPOSITORY_TEMP_LIMIT + 101
        );
        assert_eq!(
            b.aggregate_stored().unwrap(),
            PER_REPOSITORY_TEMP_LIMIT + 101
        );
        assert_eq!(
            b.start(free()),
            Err(BudgetError::Blocked(BlockReason::AlreadyAtConcurrencyLimit))
        );
        b.acknowledge_worker_stopped(id).unwrap();
        assert_eq!(
            b.record_store(id, 10, free()),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        b.confirm_cleanup(id).unwrap();
        assert_eq!(b.aggregate_stored().unwrap(), 0);
    }

    #[test]
    fn store_growth_below_reserve_is_still_charged() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        assert_eq!(
            b.record_store(id, 77, MINIMUM_FREE_RESERVE),
            Err(BudgetError::Blocked(BlockReason::FreeSpaceReserve))
        );
        assert_eq!(b.snapshot(id).unwrap().usage.stored_bytes, 77);
    }

    #[test]
    fn store_admission_allows_repository_limit_and_accounts_completed_jobs_in_aggregate() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        assert_eq!(
            b.check_store_admission(id, PER_REPOSITORY_TEMP_LIMIT, u64::MAX),
            Ok(())
        );
        b.record_store(id, PER_REPOSITORY_TEMP_LIMIT, u64::MAX)
            .unwrap();
        b.complete(id).unwrap();

        let second = b.start(free()).unwrap();
        assert_eq!(
            b.check_store_admission(second, PER_REPOSITORY_TEMP_LIMIT, u64::MAX),
            Ok(())
        );
        b.record_store(second, PER_REPOSITORY_TEMP_LIMIT, u64::MAX)
            .unwrap();
        b.complete(second).unwrap();

        let third = b.start(free()).unwrap();
        assert_eq!(
            b.check_store_admission(third, 1, free()),
            Err(BudgetError::Blocked(BlockReason::AggregateStorageLimit))
        );
        assert_eq!(b.snapshot(third).unwrap().usage.stored_bytes, 0);
    }

    #[test]
    fn store_admission_checks_limits_and_free_space_without_mutation() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        let snapshot = b.snapshot(id).unwrap().clone();
        let aggregate = b.aggregate_stored().unwrap();
        assert_eq!(
            b.check_store_admission(id, PER_REPOSITORY_TEMP_LIMIT + 1, free()),
            Err(BudgetError::Blocked(BlockReason::RepositoryStorageLimit))
        );
        let needed = MINIMUM_FREE_RESERVE + b.limits.headroom_bytes + 77;
        assert_eq!(b.check_store_admission(id, 77, needed), Ok(()));
        assert_eq!(
            b.check_store_admission(id, 77, needed - 1),
            Err(BudgetError::Blocked(BlockReason::FreeSpaceReserve))
        );
        assert_eq!(b.snapshot(id).unwrap(), &snapshot);
        assert_eq!(b.aggregate_stored().unwrap(), aggregate);
    }

    #[test]
    fn store_admission_rejects_overflow_without_mutation() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        b.jobs.get_mut(&id).unwrap().usage.stored_bytes = u64::MAX;
        let snapshot = b.snapshot(id).unwrap().clone();
        let aggregate = b.aggregate_stored().unwrap();
        assert_eq!(
            b.check_store_admission(id, 1, free()),
            Err(BudgetError::Blocked(BlockReason::ArithmeticOverflow))
        );
        assert_eq!(b.snapshot(id).unwrap(), &snapshot);
        assert_eq!(b.aggregate_stored().unwrap(), aggregate);

        let mut b = BudgetController::new(Limits { headroom_bytes: 0 });
        let id = b.start(MINIMUM_FREE_RESERVE).unwrap();
        b.limits.headroom_bytes = u64::MAX;
        let snapshot = b.snapshot(id).unwrap().clone();
        assert_eq!(
            b.check_store_admission(id, 1, u64::MAX),
            Err(BudgetError::Blocked(BlockReason::ArithmeticOverflow))
        );
        assert_eq!(b.snapshot(id).unwrap(), &snapshot);
    }

    #[test]
    fn store_admission_rejects_nonrunning_or_unknown_jobs_without_mutation() {
        let mut b = controller();
        let running = b.start(free()).unwrap();
        let failed = b.start(free()).unwrap_err();
        assert_eq!(
            failed,
            BudgetError::Blocked(BlockReason::AlreadyAtConcurrencyLimit)
        );
        b.fail(running, BlockReason::AttemptTimedOut).unwrap();
        let failed_snapshot = b.snapshot(running).unwrap().clone();
        assert_eq!(
            b.check_store_admission(running, 10, free()),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(b.snapshot(running).unwrap(), &failed_snapshot);
        assert_eq!(
            b.check_store_admission(JobId(999), 10, free()),
            Err(BudgetError::UnknownJob(JobId(999)))
        );
        assert_eq!(b.snapshot(running).unwrap(), &failed_snapshot);

        b.acknowledge_worker_stopped(running).unwrap();
        let canceled = b.start(free()).unwrap();
        b.cancel(canceled).unwrap();
        let canceled_snapshot = b.snapshot(canceled).unwrap().clone();
        assert_eq!(
            b.check_store_admission(canceled, 10, free()),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(b.snapshot(canceled).unwrap(), &canceled_snapshot);

        b.acknowledge_worker_stopped(canceled).unwrap();
        let completed = b.start(free()).unwrap();
        b.complete(completed).unwrap();
        let completed_snapshot = b.snapshot(completed).unwrap().clone();
        assert_eq!(
            b.check_store_admission(completed, 10, free()),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(b.snapshot(completed).unwrap(), &completed_snapshot);
    }

    #[test]
    fn protects_free_space_reserve_and_rejects_overflow() {
        let mut b = controller();
        assert_eq!(
            b.start(free() - 1),
            Err(BudgetError::Blocked(BlockReason::FreeSpaceReserve))
        );
        let id = b.start(free()).unwrap();
        assert_eq!(
            b.record_ingress(id, u64::MAX),
            Err(BudgetError::Blocked(BlockReason::IngressLimit))
        );
        assert_eq!(b.snapshot(id).unwrap().usage.ingress_bytes, u64::MAX);
        let mut b = BudgetController::new(Limits {
            headroom_bytes: u64::MAX,
        });
        assert_eq!(
            b.start(u64::MAX),
            Err(BudgetError::Blocked(BlockReason::ArithmeticOverflow))
        );
    }

    #[test]
    fn ingress_charges_over_limit_observations_and_rejects_after_stop() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        assert_eq!(
            b.record_ingress(id, PER_REPOSITORY_INGRESS_LIMIT + 5),
            Err(BudgetError::Blocked(BlockReason::IngressLimit))
        );
        assert_eq!(
            b.snapshot(id).unwrap().usage.ingress_bytes,
            PER_REPOSITORY_INGRESS_LIMIT + 5
        );
        assert_eq!(
            b.record_ingress(id, 7),
            Err(BudgetError::Blocked(BlockReason::IngressLimit))
        );
        assert_eq!(
            b.snapshot(id).unwrap().usage.ingress_bytes,
            PER_REPOSITORY_INGRESS_LIMIT + 12
        );
        assert_eq!(
            b.snapshot(id).unwrap().state,
            JobState::Failed {
                reason: BlockReason::IngressLimit,
                worker_stopped: false,
            }
        );
        b.acknowledge_worker_stopped(id).unwrap();
        assert_eq!(
            b.record_ingress(id, 1),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(
            b.snapshot(id).unwrap().usage.ingress_bytes,
            PER_REPOSITORY_INGRESS_LIMIT + 12
        );
    }

    #[test]
    fn late_ingress_after_cancel_is_charged_without_resurrecting_job() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        b.cancel(id).unwrap();
        b.record_ingress(id, 42).unwrap();
        assert_eq!(b.snapshot(id).unwrap().usage.ingress_bytes, 42);
        assert_eq!(
            b.snapshot(id).unwrap().state,
            JobState::Canceled {
                cleanup_confirmed: false,
                worker_stopped: false,
            }
        );
        b.acknowledge_worker_stopped(id).unwrap();
        assert_eq!(
            b.record_ingress(id, 9),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(b.snapshot(id).unwrap().usage.ingress_bytes, 42);
        assert_eq!(
            b.snapshot(id).unwrap().state,
            JobState::Canceled {
                cleanup_confirmed: false,
                worker_stopped: true,
            }
        );
    }

    #[test]
    fn late_over_limit_ingress_after_failure_keeps_original_failure() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        b.fail(id, BlockReason::AttemptTimedOut).unwrap();
        assert_eq!(
            b.record_ingress(id, PER_REPOSITORY_INGRESS_LIMIT + 1),
            Err(BudgetError::Blocked(BlockReason::IngressLimit))
        );
        assert_eq!(
            b.snapshot(id).unwrap().usage.ingress_bytes,
            PER_REPOSITORY_INGRESS_LIMIT + 1
        );
        assert_eq!(
            b.snapshot(id).unwrap().state,
            JobState::Failed {
                reason: BlockReason::AttemptTimedOut,
                worker_stopped: false,
            }
        );
        b.acknowledge_worker_stopped(id).unwrap();
        assert_eq!(
            b.record_ingress(id, 1),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
    }

    #[test]
    fn ingress_overflow_saturates_and_completion_closes_observations() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        b.cancel(id).unwrap();
        let canceled = b.snapshot(id).unwrap().state;
        assert_eq!(
            b.record_ingress(id, u64::MAX),
            Err(BudgetError::Blocked(BlockReason::IngressLimit))
        );
        assert_eq!(
            b.record_ingress(id, 1),
            Err(BudgetError::Blocked(BlockReason::ArithmeticOverflow))
        );
        assert_eq!(b.snapshot(id).unwrap().usage.ingress_bytes, u64::MAX);
        assert_eq!(b.snapshot(id).unwrap().state, canceled);
        assert_eq!(
            b.complete(id),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(
            b.confirm_cleanup(id),
            Err(BudgetError::Blocked(BlockReason::CleanupNotPending))
        );
        b.acknowledge_worker_stopped(id).unwrap();
        let next = b.start(free()).unwrap();
        b.record_ingress(next, 12).unwrap();
        b.complete(next).unwrap();
        assert_eq!(
            b.record_ingress(next, 1),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(b.snapshot(next).unwrap().usage.ingress_bytes, 12);
    }

    #[test]
    fn partial_store_stays_accounted_after_cancel_until_cleanup() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        b.record_store(id, 1234, free()).unwrap();
        b.cancel(id).unwrap();
        assert_eq!(b.aggregate_stored().unwrap(), 1234);
        assert_eq!(
            b.start(free()),
            Err(BudgetError::Blocked(BlockReason::AlreadyAtConcurrencyLimit))
        );
        b.acknowledge_worker_stopped(id).unwrap();
        assert_eq!(
            b.snapshot(id).unwrap().state,
            JobState::Canceled {
                cleanup_confirmed: false,
                worker_stopped: true
            }
        );
        b.confirm_cleanup(id).unwrap();
        assert_eq!(b.aggregate_stored().unwrap(), 0);
        assert_eq!(
            b.snapshot(id).unwrap().state,
            JobState::Canceled {
                cleanup_confirmed: true,
                worker_stopped: true
            }
        );
    }

    #[test]
    fn cleanup_releases_failed_store_and_aggregate_limit_applies() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        b.record_store(id, PER_REPOSITORY_TEMP_LIMIT, free())
            .unwrap();
        b.fail(id, BlockReason::AttemptTimedOut).unwrap();
        assert_eq!(b.aggregate_stored().unwrap(), PER_REPOSITORY_TEMP_LIMIT);
        assert_eq!(
            b.start(free()),
            Err(BudgetError::Blocked(BlockReason::AlreadyAtConcurrencyLimit))
        );
        b.acknowledge_worker_stopped(id).unwrap();
        let second = b.start(free()).unwrap();
        assert_eq!(
            b.record_store(second, PER_REPOSITORY_TEMP_LIMIT, free()),
            Ok(())
        );
        b.complete(second).unwrap();
        let third = b.start(free()).unwrap();
        assert_eq!(
            b.record_store(third, 1, free()),
            Err(BudgetError::Blocked(BlockReason::AggregateStorageLimit))
        );
        assert_eq!(b.snapshot(third).unwrap().usage.stored_bytes, 1);
        b.confirm_cleanup(id).unwrap();
        assert_eq!(b.aggregate_stored().unwrap(), PER_REPOSITORY_TEMP_LIMIT + 1);
    }

    #[test]
    fn cumulative_ingress_repeats_increases_and_enforces_exact_cap() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        assert_eq!(b.record_ingress_total(id, 100), Ok(()));
        assert_eq!(b.record_ingress_total(id, 100), Ok(()));
        assert_eq!(b.snapshot(id).unwrap().usage.ingress_bytes, 100);

        assert_eq!(
            b.record_ingress_total(id, PER_REPOSITORY_INGRESS_LIMIT),
            Ok(())
        );
        assert_eq!(
            b.snapshot(id).unwrap().usage.ingress_bytes,
            PER_REPOSITORY_INGRESS_LIMIT
        );
        assert_eq!(
            b.record_ingress_total(id, PER_REPOSITORY_INGRESS_LIMIT + 1),
            Err(BudgetError::Blocked(BlockReason::IngressLimit))
        );
        assert_eq!(
            b.snapshot(id).unwrap().usage.ingress_bytes,
            PER_REPOSITORY_INGRESS_LIMIT + 1
        );
        assert_eq!(
            b.record_ingress_total(id, PER_REPOSITORY_INGRESS_LIMIT + 1),
            Err(BudgetError::Blocked(BlockReason::IngressLimit))
        );
        assert_eq!(
            b.snapshot(id).unwrap().usage.ingress_bytes,
            PER_REPOSITORY_INGRESS_LIMIT + 1
        );
    }

    #[test]
    fn cumulative_ingress_late_totals_are_charged_but_preserve_terminal_reason() {
        let mut b = controller();
        let canceled = b.start(free()).unwrap();
        b.record_ingress_total(canceled, 20).unwrap();
        b.cancel(canceled).unwrap();
        assert_eq!(b.record_ingress_total(canceled, 45), Ok(()));
        assert_eq!(b.snapshot(canceled).unwrap().usage.ingress_bytes, 45);
        assert_eq!(
            b.snapshot(canceled).unwrap().state,
            JobState::Canceled {
                cleanup_confirmed: false,
                worker_stopped: false
            }
        );

        let mut failed_controller = controller();
        let failed = failed_controller.start(free()).unwrap();
        failed_controller.record_ingress_total(failed, 20).unwrap();
        failed_controller
            .fail(failed, BlockReason::AttemptTimedOut)
            .unwrap();
        assert_eq!(failed_controller.record_ingress_total(failed, 45), Ok(()));
        assert_eq!(
            failed_controller
                .snapshot(failed)
                .unwrap()
                .usage
                .ingress_bytes,
            45
        );
        assert_eq!(
            failed_controller.snapshot(failed).unwrap().state,
            JobState::Failed {
                reason: BlockReason::AttemptTimedOut,
                worker_stopped: false
            }
        );
    }

    #[test]
    fn regressed_ingress_fails_running_without_changing_usage() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        b.record_ingress_total(id, 200).unwrap();
        assert_eq!(
            b.record_ingress_total(id, 199),
            Err(BudgetError::Blocked(BlockReason::IngressCounterRegressed))
        );
        assert_eq!(b.snapshot(id).unwrap().usage.ingress_bytes, 200);
        assert_eq!(
            b.snapshot(id).unwrap().state,
            JobState::Failed {
                reason: BlockReason::IngressCounterRegressed,
                worker_stopped: false
            }
        );
    }

    #[test]
    fn regressed_ingress_preserves_canceled_and_prior_failed_states() {
        let mut b = controller();
        let canceled = b.start(free()).unwrap();
        b.record_ingress_total(canceled, 200).unwrap();
        b.cancel(canceled).unwrap();
        assert_eq!(
            b.record_ingress_total(canceled, 199),
            Err(BudgetError::Blocked(BlockReason::IngressCounterRegressed))
        );
        assert_eq!(b.snapshot(canceled).unwrap().usage.ingress_bytes, 200);
        assert_eq!(
            b.snapshot(canceled).unwrap().state,
            JobState::Canceled {
                cleanup_confirmed: false,
                worker_stopped: false
            }
        );

        let mut b = controller();
        let failed = b.start(free()).unwrap();
        b.record_ingress_total(failed, 200).unwrap();
        b.fail(failed, BlockReason::AttemptTimedOut).unwrap();
        assert_eq!(
            b.record_ingress_total(failed, 199),
            Err(BudgetError::Blocked(BlockReason::IngressCounterRegressed))
        );
        assert_eq!(b.snapshot(failed).unwrap().usage.ingress_bytes, 200);
        assert_eq!(
            b.snapshot(failed).unwrap().state,
            JobState::Failed {
                reason: BlockReason::AttemptTimedOut,
                worker_stopped: false
            }
        );
    }

    #[test]
    fn cumulative_ingress_rejects_unknown_stopped_and_completed_jobs() {
        let mut b = controller();
        assert_eq!(
            b.record_ingress_total(JobId(999), 1),
            Err(BudgetError::UnknownJob(JobId(999)))
        );

        let stopped = b.start(free()).unwrap();
        b.fail(stopped, BlockReason::AttemptTimedOut).unwrap();
        b.acknowledge_worker_stopped(stopped).unwrap();
        let stopped_snapshot = b.snapshot(stopped).unwrap().clone();
        assert_eq!(
            b.record_ingress_total(stopped, 0),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(b.snapshot(stopped).unwrap(), &stopped_snapshot);

        let completed = b.start(free()).unwrap();
        b.record_ingress_total(completed, 10).unwrap();
        b.complete(completed).unwrap();
        let completed_snapshot = b.snapshot(completed).unwrap().clone();
        assert_eq!(
            b.record_ingress_total(completed, 10),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(b.snapshot(completed).unwrap(), &completed_snapshot);
    }

    #[test]
    fn cumulative_ingress_max_total_and_delta_overflow_saturate_without_wrapping() {
        let mut b = controller();
        let id = b.start(free()).unwrap();
        assert_eq!(
            b.record_ingress_total(id, u64::MAX),
            Err(BudgetError::Blocked(BlockReason::IngressLimit))
        );
        assert_eq!(b.snapshot(id).unwrap().usage.ingress_bytes, u64::MAX);
        assert_eq!(
            b.record_ingress_total(id, u64::MAX),
            Err(BudgetError::Blocked(BlockReason::IngressLimit))
        );
        assert_eq!(b.snapshot(id).unwrap().usage.ingress_bytes, u64::MAX);
        b.acknowledge_worker_stopped(id).unwrap();

        let overflow = b.start(free()).unwrap();
        b.jobs.get_mut(&overflow).unwrap().usage.ingress_bytes = u64::MAX - 4;
        assert_eq!(
            b.record_ingress(overflow, 10),
            Err(BudgetError::Blocked(BlockReason::ArithmeticOverflow))
        );
        assert_eq!(b.snapshot(overflow).unwrap().usage.ingress_bytes, u64::MAX);
        assert_eq!(
            b.record_ingress_total(overflow, u64::MAX),
            Err(BudgetError::Blocked(BlockReason::IngressLimit))
        );
        assert_eq!(b.snapshot(overflow).unwrap().usage.ingress_bytes, u64::MAX);
        assert_eq!(
            b.snapshot(overflow).unwrap().state,
            JobState::Failed {
                reason: BlockReason::ArithmeticOverflow,
                worker_stopped: false
            }
        );
    }
}
