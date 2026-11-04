//! One-shot, fixed-size payload write used to probe verification-store bounds.
//! The file is intentionally retained; it grants no cleanup authority.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::{identity, open_parent, require_entries, validate_private_directory};
use super::{OwnedStore, Phase, StoreError};
use std::fs::File;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{io::Write, os::fd::AsFd};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use verification_budget::{BlockReason, BudgetController, Limits};

pub(super) const NAME: &str = "payload.bin";
pub(super) const LIMIT: u64 = 32 * 1024;
pub(super) const PARTIAL_FIXTURE_BYTES: u64 = 2048;
const ATTEMPT_LIMIT: u64 = 64 * 1024;
const CHUNK: usize = 4096;
const RESERVE_HEADROOM: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PayloadReport {
    pub(crate) written_bytes: u64,
    pub(crate) limit_bytes: u64,
    pub(crate) cap_reached: bool,
}

impl OwnedStore {
    /// Write one fixed synthetic payload in this fresh owned store. The store
    /// is consumed so the caller cannot follow the write with another action.
    /// Errors retain any file or partial bytes already created.
    pub(crate) fn write_payload_fixture(self) -> Result<PayloadReport, StoreError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            write_payload(self)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = self;
            Err(StoreError::UnsupportedPlatform)
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_payload(store: OwnedStore) -> Result<PayloadReport, StoreError> {
    write_payload_with(
        store,
        sampled_free_bytes,
        |file, bytes| file.write_all(bytes),
        File::sync_all,
    )
}

/// Write the same fixed synthetic payload through a validated worker-owned
/// directory descriptor. The caller supplies only identity checks for this
/// fixed handoff; filenames, data and byte limits remain closed here.
pub(super) fn write_worker_payload(
    root: &File,
    verify: impl FnMut(bool, Option<(u64, u64)>) -> Result<(), StoreError>,
) -> Result<PayloadReport, StoreError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut budget = BudgetController::new(Limits {
            headroom_bytes: RESERVE_HEADROOM,
        });
        write_payload_accounted_at_root(
            root,
            verify,
            sampled_free_bytes,
            |file, bytes| file.write_all(bytes),
            File::sync_all,
            &mut budget,
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (root, verify);
        Err(StoreError::UnsupportedPlatform)
    }
}

/// Fixed failure probe: write exactly one 2 KiB prefix through the same
/// accounting core, then report an interrupted write. This is not a tunable
/// writer or a production disk-full simulation.
pub(super) fn write_worker_payload_partial(
    root: &File,
    verify: impl FnMut(bool, Option<(u64, u64)>) -> Result<(), StoreError>,
) -> Result<PayloadReport, StoreError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut budget = BudgetController::new(Limits {
            headroom_bytes: RESERVE_HEADROOM,
        });
        write_payload_accounted_at_root(
            root,
            verify,
            sampled_free_bytes,
            |file, bytes| {
                file.write_all(&bytes[..PARTIAL_FIXTURE_BYTES as usize])?;
                Err(std::io::Error::other("synthetic interrupted payload write"))
            },
            File::sync_all,
            &mut budget,
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (root, verify);
        Err(StoreError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_payload_with(
    store: OwnedStore,
    sample_free: impl FnMut(&File) -> Result<u64, StoreError>,
    write_bytes: impl FnMut(&mut File, &[u8]) -> std::io::Result<()>,
    sync_file: impl FnOnce(&File) -> std::io::Result<()>,
) -> Result<PayloadReport, StoreError> {
    let mut budget = BudgetController::new(Limits {
        headroom_bytes: RESERVE_HEADROOM,
    });
    write_payload_accounted(store, sample_free, write_bytes, sync_file, &mut budget)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_payload_accounted(
    store: OwnedStore,
    sample_free: impl FnMut(&File) -> Result<u64, StoreError>,
    write_bytes: impl FnMut(&mut File, &[u8]) -> std::io::Result<()>,
    sync_file: impl FnOnce(&File) -> std::io::Result<()>,
    budget: &mut BudgetController,
) -> Result<PayloadReport, StoreError> {
    if store.phase != Phase::Created || store.poisoned || store.cleanup_started {
        return Err(StoreError::InvalidJournal);
    }
    write_payload_accounted_at_root(
        &store.root,
        |payload_expected, payload_id| validate_store(&store, payload_expected, payload_id),
        sample_free,
        write_bytes,
        sync_file,
        budget,
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_payload_accounted_at_root(
    root: &File,
    mut verify: impl FnMut(bool, Option<(u64, u64)>) -> Result<(), StoreError>,
    mut sample_free: impl FnMut(&File) -> Result<u64, StoreError>,
    mut write_bytes: impl FnMut(&mut File, &[u8]) -> std::io::Result<()>,
    sync_file: impl FnOnce(&File) -> std::io::Result<()>,
    budget: &mut BudgetController,
) -> Result<PayloadReport, StoreError> {
    use rustix::fs::{openat, OFlags};

    verify(false, None)?;
    let free_before = sample_free(root)?;
    let job = budget
        .start(free_before)
        .map_err(|_| StoreError::InvalidJournal)?;
    let outcome = (|| {
        budget
            .check_store_admission(job, LIMIT, free_before)
            .map_err(|_| StoreError::InvalidJournal)?;
        let fd = openat(
            root,
            NAME,
            OFlags::RDWR
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            rustix::fs::Mode::from_bits(0o600).unwrap(),
        )?;
        let mut payload = File::from(fd);
        let metadata = payload.metadata()?;
        validate_payload_metadata(&metadata, None, 0)?;
        let payload_id = identity(&payload)?;
        verify(true, Some(payload_id))?;
        let mut accounted_high_water = 0;
        account_payload_observation(
            job,
            budget,
            payload_high_water_from_metadata(&metadata)?,
            free_before,
            &mut accounted_high_water,
        )?;

        let (written, cap_reached) = write_chunks(&mut payload, |file, offset, next| {
            verify(true, Some(payload_id))?;
            let free_before = sample_free(root)?;
            account_payload_observation(
                job,
                budget,
                payload_high_water(file)?,
                free_before,
                &mut accounted_high_water,
            )?;
            budget
                .check_store_admission(job, next as u64, free_before)
                .map_err(|_| StoreError::InvalidJournal)?;
            verify(true, Some(payload_id))?;
            validate_named_payload_at_root(root, file, payload_id, offset)?;
            let mut chunk = [0u8; CHUNK];
            for (index, byte) in chunk[..next].iter_mut().enumerate() {
                *byte = (((offset as usize + index) * 31 + 17) % 251 + 1) as u8;
            }
            let write_result = write_bytes(file, &chunk[..next]);
            // Observe and charge actual high-water growth even when write_all
            // left only part of this chunk. Never release a prior allocation
            // charge if the filesystem later reports fewer blocks.
            let actual_high_water = payload_high_water(file)?;
            let free_after = sample_free(root);
            let charge_result = account_payload_observation(
                job,
                budget,
                actual_high_water,
                free_after.as_ref().copied().unwrap_or(0),
                &mut accounted_high_water,
            );
            write_result?;
            free_after?;
            charge_result?;
            verify(true, Some(payload_id))?;
            validate_named_payload_at_root(root, file, payload_id, offset + next as u64)?;
            Ok(())
        })?;
        let sync_result = sync_payload(&payload, sync_file);
        let sync_high_water = payload_high_water(&payload);
        let free_after_sync = sample_free(root);
        let sync_charge = match sync_high_water {
            Ok(high_water) => account_payload_observation(
                job,
                budget,
                high_water,
                free_after_sync.as_ref().copied().unwrap_or(0),
                &mut accounted_high_water,
            ),
            Err(error) => Err(error),
        };
        sync_result?;
        free_after_sync?;
        sync_charge?;
        root.sync_all()?;
        verify(true, Some(payload_id))?;
        validate_named_payload_at_root(root, &payload, payload_id, written)?;
        Ok(PayloadReport {
            written_bytes: written,
            limit_bytes: LIMIT,
            cap_reached,
        })
    })();
    if outcome.is_err() {
        // fail() only accepts Running; an earlier budget failure keeps its reason.
        let _ = budget.fail(job, BlockReason::StoreIncomplete);
    } else {
        budget
            .complete(job)
            .map_err(|_| StoreError::InvalidJournal)?;
    }
    outcome
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_chunks(
    file: &mut File,
    mut write_one: impl FnMut(&mut File, u64, usize) -> Result<(), StoreError>,
) -> Result<(u64, bool), StoreError> {
    let mut written = 0u64;
    let mut attempted = 0u64;
    let mut cap_reached = false;
    while attempted < ATTEMPT_LIMIT {
        let next = usize::try_from((ATTEMPT_LIMIT - attempted).min(CHUNK as u64))
            .map_err(|_| StoreError::InvalidJournal)?;
        if written
            .checked_add(next as u64)
            .is_none_or(|end| end > LIMIT)
        {
            cap_reached = true;
            break;
        }
        write_one(file, written, next)?;
        written = written
            .checked_add(next as u64)
            .ok_or(StoreError::InvalidJournal)?;
        attempted = attempted
            .checked_add(next as u64)
            .ok_or(StoreError::InvalidJournal)?;
    }
    Ok((written, cap_reached))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sync_payload(
    file: &File,
    sync: impl FnOnce(&File) -> std::io::Result<()>,
) -> Result<(), StoreError> {
    sync(file).map_err(StoreError::Io)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn payload_high_water(file: &File) -> Result<u64, StoreError> {
    payload_high_water_from_metadata(&file.metadata()?)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn payload_high_water_from_metadata(
    metadata: &std::fs::Metadata,
) -> Result<u64, StoreError> {
    use std::os::unix::fs::MetadataExt;
    payload_high_water_bytes(metadata.len(), metadata.blocks())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn payload_high_water_bytes(logical_bytes: u64, allocated_blocks: u64) -> Result<u64, StoreError> {
    let allocated_bytes = allocated_blocks
        .checked_mul(512)
        .ok_or(StoreError::InvalidJournal)?;
    Ok(logical_bytes.max(allocated_bytes))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn account_payload_observation(
    job: verification_budget::JobId,
    budget: &mut BudgetController,
    observed_high_water: u64,
    free_bytes: u64,
    accounted_high_water: &mut u64,
) -> Result<(), StoreError> {
    let high_water = (*accounted_high_water).max(observed_high_water);
    let growth = high_water
        .checked_sub(*accounted_high_water)
        .ok_or(StoreError::InvalidJournal)?;
    *accounted_high_water = high_water;
    budget
        .record_store(job, growth, free_bytes)
        .map_err(|_| StoreError::InvalidJournal)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_store(
    store: &OwnedStore,
    payload_expected: bool,
    payload_id: Option<(u64, u64)>,
) -> Result<(), StoreError> {
    use rustix::fs::{openat, OFlags};
    if identity(&store.parent)? != store.parent_id || identity(&store.root)? != store.root_id {
        return Err(StoreError::InvalidIdentity);
    }
    if store.phase != Phase::Created || store.poisoned || store.cleanup_started {
        return Err(StoreError::InvalidJournal);
    }
    store.inspect_state()?;
    let named_parent = open_parent(&store.parent_path)?;
    if identity(&named_parent)? != store.parent_id {
        return Err(StoreError::InvalidIdentity);
    }
    validate_private_directory(&named_parent)?;
    let named_root = openat(
        &named_parent,
        &store.name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    if identity(&named_root)? != store.root_id {
        return Err(StoreError::InvalidIdentity);
    }
    validate_private_directory(&named_root)?;
    validate_private_directory(&store.root)?;
    let journal = openat(
        &store.root,
        "owner.journal",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    let journal = File::from(journal);
    if identity(&journal)? != store.journal_id.ok_or(StoreError::InvalidJournal)? {
        return Err(StoreError::InvalidJournal);
    }
    let mut bytes = Vec::new();
    journal.try_clone()?.take(512).read_to_end(&mut bytes)?;
    if bytes != store.journal_contents().as_bytes() {
        return Err(StoreError::InvalidJournal);
    }
    if payload_expected {
        require_entries(&store.root, &["owner.journal", NAME])?;
        let expected = payload_id.ok_or(StoreError::InvalidJournal)?;
        validate_named_identity(&store.root, expected)?;
    } else {
        require_entries(&store.root, &["owner.journal"])?;
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_named_identity(root: &impl AsFd, expected: (u64, u64)) -> Result<File, StoreError> {
    use rustix::fs::{openat, OFlags};
    let fd = openat(
        root,
        NAME,
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    let file = File::from(fd);
    let metadata = file.metadata()?;
    validate_payload_metadata(&metadata, Some(expected), metadata.len())?;
    Ok(file)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn validate_named_payload_at_root(
    root: &File,
    payload: &File,
    payload_id: (u64, u64),
    expected_size: u64,
) -> Result<(), StoreError> {
    let fd_metadata = payload.metadata()?;
    validate_payload_metadata(&fd_metadata, Some(payload_id), expected_size)?;
    if identity(payload)? != payload_id {
        return Err(StoreError::InvalidIdentity);
    }
    let named = validate_named_identity(root, payload_id)?;
    if named.metadata()?.len() != expected_size {
        return Err(StoreError::InvalidIdentity);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn validate_payload_metadata(
    metadata: &std::fs::Metadata,
    expected: Option<(u64, u64)>,
    expected_size: u64,
) -> Result<(), StoreError> {
    use std::os::unix::fs::MetadataExt;
    let id = (metadata.dev(), metadata.ino());
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() != expected_size
        || expected.is_some_and(|expected| expected != id)
    {
        return Err(StoreError::InvalidIdentity);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sampled_free_bytes(root: &impl AsFd) -> Result<u64, StoreError> {
    use rustix::fs::fstatvfs;
    let stat = fstatvfs(root)?;
    free_bytes(stat.f_bavail, stat.f_frsize)
}

fn free_bytes(available_blocks: u64, fragment_size: u64) -> Result<u64, StoreError> {
    available_blocks
        .checked_mul(fragment_size)
        .ok_or(StoreError::InvalidJournal)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io::Read;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn payload_length(root: &impl AsFd) -> u64 {
    payload_length_named(root, NAME)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn payload_length_named(root: &impl AsFd, name: &str) -> u64 {
    use rustix::fs::{openat, OFlags};
    let fd = openat(
        root,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .unwrap();
    File::from(fd).metadata().unwrap().len()
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn payload_high_water_named(root: &impl AsFd, name: &str) -> u64 {
    use rustix::fs::{openat, OFlags};
    let fd = openat(
        root,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .unwrap();
    payload_high_water(&File::from(fd)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn writes_only_the_fixed_bounded_payload_and_retains_it() {
        use rustix::fs::{openat, OFlags};
        use std::fs::File;
        use std::io::Read;
        let store = OwnedStore::create().unwrap();
        let root = store.root.try_clone().unwrap();
        let report = store.write_payload_fixture().unwrap();
        assert_eq!(report.written_bytes, LIMIT);
        assert_eq!(report.limit_bytes, LIMIT);
        assert!(report.cap_reached);
        let fd = openat(
            &root,
            NAME,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let mut bytes = Vec::new();
        File::from(fd).read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, LIMIT);
        assert!(bytes.iter().all(|byte| *byte != 0));
        require_entries(&root, &["owner.journal", NAME]).unwrap();
    }

    #[test]
    fn partial_writes_remain_charged_and_do_not_authorize_cleanup() {
        use verification_budget::{JobId, JobState};
        let store = OwnedStore::create().unwrap();
        let root = store.root.try_clone().unwrap();
        let mut budget = BudgetController::new(Limits {
            headroom_bytes: RESERVE_HEADROOM,
        });
        let result = write_payload_accounted(
            store,
            |_| Ok(u64::MAX),
            |file, bytes| {
                file.write_all(&bytes[..2048])?;
                Err(std::io::Error::other("injected partial write"))
            },
            File::sync_all,
            &mut budget,
        );
        assert!(result.is_err());
        assert_eq!(payload_length(&root), 2048);
        let actual_high_water = payload_high_water_named(&root, NAME);
        let job = budget.snapshot(JobId(1)).unwrap();
        assert_eq!(job.usage.stored_bytes, actual_high_water);
        assert_eq!(
            job.state,
            JobState::Failed {
                reason: BlockReason::StoreIncomplete,
                worker_stopped: false
            }
        );
        assert!(budget.confirm_cleanup(JobId(1)).is_err());
        assert_eq!(budget.aggregate_stored().unwrap(), actual_high_water);
    }

    #[test]
    fn postwrite_reserve_failure_charges_growth_and_keeps_first_reason() {
        use verification_budget::{JobId, JobState};
        let store = OwnedStore::create().unwrap();
        let root = store.root.try_clone().unwrap();
        let mut samples = 0;
        let mut budget = BudgetController::new(Limits {
            headroom_bytes: RESERVE_HEADROOM,
        });
        let result = write_payload_accounted(
            store,
            |_| {
                samples += 1;
                Ok(if samples <= 2 { u64::MAX } else { 0 })
            },
            |file, bytes| file.write_all(bytes),
            File::sync_all,
            &mut budget,
        );
        assert!(result.is_err());
        assert_eq!(payload_length(&root), CHUNK as u64);
        let actual_high_water = payload_high_water_named(&root, NAME);
        let job = budget.snapshot(JobId(1)).unwrap();
        assert_eq!(job.usage.stored_bytes, actual_high_water);
        assert_eq!(
            job.state,
            JobState::Failed {
                reason: BlockReason::FreeSpaceReserve,
                worker_stopped: false
            }
        );
    }

    #[test]
    fn aggregate_admission_refuses_creation_with_simulated_retained_totals() {
        use verification_budget::{JobId, PER_REPOSITORY_TEMP_LIMIT};
        let mut budget = BudgetController::new(Limits {
            headroom_bytes: RESERVE_HEADROOM,
        });
        for _ in 0..2 {
            let id = budget.start(u64::MAX).unwrap();
            // Model retained stores; this test never writes GiB-sized fixtures.
            budget
                .record_store(id, PER_REPOSITORY_TEMP_LIMIT, u64::MAX)
                .unwrap();
            budget.complete(id).unwrap();
        }
        let store = OwnedStore::create().unwrap();
        let root = store.root.try_clone().unwrap();
        let result = write_payload_accounted(
            store,
            |_| Ok(u64::MAX),
            |_, _| panic!("denied admission must not write"),
            File::sync_all,
            &mut budget,
        );
        assert!(result.is_err());
        require_entries(&root, &["owner.journal"]).unwrap();
        assert_eq!(budget.snapshot(JobId(3)).unwrap().usage.stored_bytes, 0);
        assert_eq!(
            budget.aggregate_stored().unwrap(),
            2 * PER_REPOSITORY_TEMP_LIMIT
        );
    }

    #[test]
    fn free_space_multiplication_refuses_overflow() {
        assert!(free_bytes(u64::MAX, 2).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn low_space_preflight_refuses_before_payload_creation() {
        use std::fs::File;
        let store = OwnedStore::create().unwrap();
        let root = store.root.try_clone().unwrap();
        let low = verification_budget::MINIMUM_FREE_RESERVE + RESERVE_HEADROOM + LIMIT - 1;
        let result = write_payload_with(
            store,
            |_| Ok(low),
            |_, _| panic!("writer must not run after failed preflight"),
            File::sync_all,
        );
        assert!(result.is_err());
        require_entries(&root, &["owner.journal"]).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn midwrite_reserve_failure_retains_partial_payload() {
        use std::cell::Cell;
        use std::fs::File;
        let store = OwnedStore::create().unwrap();
        let root = store.root.try_clone().unwrap();
        let samples = Cell::new(0);
        let enough = u64::MAX;
        let low = verification_budget::MINIMUM_FREE_RESERVE + RESERVE_HEADROOM + CHUNK as u64 - 1;
        let result = write_payload_with(
            store,
            |_| {
                let count = samples.get() + 1;
                samples.set(count);
                Ok(if count <= 2 { enough } else { low })
            },
            |file, bytes| file.write_all(bytes),
            File::sync_all,
        );
        assert!(result.is_err());
        assert_eq!(payload_length(&root), CHUNK as u64);
        require_entries(&root, &["owner.journal", NAME]).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn named_payload_replacement_between_chunks_stops_further_writes() {
        use rustix::fs::{openat, renameat, OFlags};
        use std::{cell::Cell, fs::File};
        let store = OwnedStore::create().unwrap();
        let root = store.root.try_clone().unwrap();
        let samples = Cell::new(0);
        let result = write_payload_with(
            store,
            |directory| {
                let count = samples.get() + 1;
                samples.set(count);
                if count == 3 {
                    renameat(directory, NAME, directory, "payload.previous")?;
                    let replacement = openat(
                        directory,
                        NAME,
                        OFlags::RDWR
                            | OFlags::CREATE
                            | OFlags::EXCL
                            | OFlags::NOFOLLOW
                            | OFlags::CLOEXEC,
                        rustix::fs::Mode::from_bits(0o600).unwrap(),
                    )?;
                    drop(File::from(replacement));
                }
                Ok(u64::MAX)
            },
            |file, bytes| file.write_all(bytes),
            File::sync_all,
        );
        assert!(result.is_err());
        assert_eq!(
            payload_length_named(&root, "payload.previous"),
            CHUNK as u64
        );
        assert_eq!(payload_length(&root), 0);
        require_entries(&root, &["owner.journal", "payload.previous", NAME]).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn existing_cleanup_refuses_payload_before_intent_or_unlink() {
        use rustix::fs::{openat, OFlags};
        use std::fs::File;
        let mut store = OwnedStore::create().unwrap();
        let pid = 42;
        store.bind_worker(pid).unwrap();
        let receipt = crate::StopReceipt::confirmed(store.token(), pid);
        store.record_stopped(&receipt).unwrap();
        let fd = openat(
            &store.root,
            NAME,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::from_bits(0o600).unwrap(),
        )
        .unwrap();
        drop(File::from(fd));
        assert!(matches!(
            store.cleanup_after_stopped(&receipt),
            Err(StoreError::InvalidJournal)
        ));
        require_entries(&store.root, &["owner.journal", NAME]).unwrap();
        let named_parent = open_parent(&store.parent_path).unwrap();
        assert!(rustix::fs::openat(
            &named_parent,
            "cleanup.journal",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn mid_write_and_sync_failures_leave_partial_payload_in_place() {
        use std::fs::File;
        let store = OwnedStore::create().unwrap();
        let root = store.root.try_clone().unwrap();
        let fail_after_partial = write_payload_with(
            store,
            |_| Ok(u64::MAX),
            |file, bytes| {
                file.write_all(&bytes[..2048])?;
                Err(std::io::Error::other("injected write failure"))
            },
            File::sync_all,
        );
        assert!(fail_after_partial.is_err());
        assert_eq!(payload_length(&root), 2048);
        require_entries(&root, &["owner.journal", NAME]).unwrap();

        let store = OwnedStore::create().unwrap();
        let root = store.root.try_clone().unwrap();
        let mut budget = BudgetController::new(Limits {
            headroom_bytes: RESERVE_HEADROOM,
        });
        let failure = write_payload_accounted(
            store,
            |_| Ok(u64::MAX),
            |file, bytes| file.write_all(bytes),
            |file| {
                file.sync_all()?;
                Err(std::io::Error::other("injected sync failure"))
            },
            &mut budget,
        );
        assert!(failure.is_err());
        assert_eq!(payload_length(&root), LIMIT);
        assert_eq!(
            budget
                .snapshot(verification_budget::JobId(1))
                .unwrap()
                .usage
                .stored_bytes,
            payload_high_water_named(&root, NAME)
        );
        require_entries(&root, &["owner.journal", NAME]).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn high_water_uses_the_larger_logical_or_allocated_observation() {
        assert_eq!(payload_high_water_bytes(2048, 1).unwrap(), 2048);
        assert_eq!(payload_high_water_bytes(1000, 4).unwrap(), 2048);
        assert_eq!(payload_high_water_bytes(0, 0).unwrap(), 0);
        assert!(payload_high_water_bytes(1, u64::MAX).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn lower_later_allocation_observation_does_not_release_budget_charge() {
        let mut budget = BudgetController::new(Limits { headroom_bytes: 0 });
        let job = budget.start(u64::MAX).unwrap();
        let mut accounted = 0;
        account_payload_observation(job, &mut budget, 8192, u64::MAX, &mut accounted).unwrap();
        account_payload_observation(job, &mut budget, 4096, u64::MAX, &mut accounted).unwrap();
        assert_eq!(accounted, 8192);
        assert_eq!(budget.snapshot(job).unwrap().usage.stored_bytes, 8192);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn payload_method_rejects_nonfresh_store_before_create() {
        let mut store = OwnedStore::create().unwrap();
        store.bind_worker(99).unwrap();
        let root = store.root.try_clone().unwrap();
        assert!(store.write_payload_fixture().is_err());
        require_entries(&root, &["owner.journal"]).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn payload_method_refuses_unexpected_root_entry_before_create() {
        use rustix::fs::{openat, OFlags};
        use std::fs::File;
        let store = OwnedStore::create().unwrap();
        let root = store.root.try_clone().unwrap();
        let fd = openat(
            &root,
            "unexpected",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::from_bits(0o600).unwrap(),
        )
        .unwrap();
        drop(File::from(fd));
        assert!(store.write_payload_fixture().is_err());
        require_entries(&root, &["owner.journal", "unexpected"]).unwrap();
    }
}
