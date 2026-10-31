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

    /// Account a received network chunk. Oversized/impossible accounting blocks
    /// the job and preserves any already-accounted store bytes.
    pub fn record_ingress(&mut self, id: JobId, bytes: u64) -> Result<(), BudgetError> {
        let job = self.running_mut(id)?;
        let next = match job.usage.ingress_bytes.checked_add(bytes) {
            Some(v) if v <= PER_REPOSITORY_INGRESS_LIMIT => v,
            Some(_) => return Self::block_job(job, BlockReason::IngressLimit),
            None => return Self::block_job(job, BlockReason::ArithmeticOverflow),
        };
        job.usage.ingress_bytes = next;
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
        let mut b = BudgetController::new(Limits {
            headroom_bytes: u64::MAX,
        });
        assert_eq!(
            b.start(u64::MAX),
            Err(BudgetError::Blocked(BlockReason::ArithmeticOverflow))
        );
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
}
