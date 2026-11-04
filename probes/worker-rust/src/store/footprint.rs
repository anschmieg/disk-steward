//! Fixed, post-stop measurement for the app-created synthetic worker fixture.

use super::{FixtureFootprintObservation, OwnedStore, Phase, StoreError};
use crate::StopReceipt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    fs::File,
    time::{Duration, Instant},
};
use verification_store_registry::VerificationStoreRegistry;

const REGISTRY_NAME: &str = "verification-registry.sqlite3";
const REGISTRY_LEASE_NAME: &str = "verification-registry.sqlite3.lease";
const JOURNAL_NAME: &str = "owner.journal";
const OBJECT_COUNT: u8 = 6;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObjectStamp {
    identity: (u64, u64),
    len: u64,
    blocks: u64,
    mode: u32,
    links: u64,
    uid: u32,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl From<&std::fs::Metadata> for ObjectStamp {
    fn from(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            identity: (metadata.dev(), metadata.ino()),
            len: metadata.len(),
            blocks: metadata.blocks(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            uid: metadata.uid(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

pub(super) fn observe_stopped_fixture(
    store: &OwnedStore,
    registry: &VerificationStoreRegistry,
    receipt: &StopReceipt,
) -> Result<FixtureFootprintObservation, StoreError> {
    observe_with_checkpoint(store, registry, receipt, |_| Ok(()))
}

// Private checkpoint supports deterministic mutation coverage without races.
fn observe_with_checkpoint(
    store: &OwnedStore,
    registry: &VerificationStoreRegistry,
    receipt: &StopReceipt,
    after_snapshot: impl FnOnce(&OwnedStore) -> Result<(), StoreError>,
) -> Result<FixtureFootprintObservation, StoreError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use rustix::fs::{openat, OFlags};
        use std::os::unix::fs::MetadataExt;

        let deadline = Instant::now() + Duration::from_secs(1);
        if !store.worker_stdin_issued
            || store.poisoned
            || store.cleanup_started
            || store.worker_payload_reservation != Some(super::WORKER_PAYLOAD_RESERVATION)
            || receipt.pid() == 0
            || receipt.token() != store.token
            || store.phase != Phase::Stopped(receipt.pid())
        {
            return Err(StoreError::InvalidJournal);
        }
        store.inspect_stopped_worker_payload(receipt)?;
        check_deadline(deadline)?;
        store.inspect_state()?;
        check_deadline(deadline)?;

        let control_before = registry
            .observe_control_footprint()
            .map_err(|_| StoreError::InvalidIdentity)?;
        check_deadline(deadline)?;
        if control_before.parent_identity() != store.parent_id {
            return Err(StoreError::InvalidIdentity);
        }

        let named_parent = File::from(super::open_parent(&store.parent_path)?);
        if super::identity(&named_parent)? != store.parent_id {
            return Err(StoreError::InvalidIdentity);
        }
        super::validate_private_directory(&store.parent)?;
        super::validate_private_directory(&named_parent)?;
        let named_root = openat(
            &named_parent,
            &store.name,
            OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        let named_root = File::from(named_root);
        if super::identity(&store.root)? != store.root_id
            || super::identity(&named_root)? != store.root_id
        {
            return Err(StoreError::InvalidIdentity);
        }
        super::validate_private_directory(&store.root)?;
        super::validate_private_directory(&named_root)?;
        super::require_entries(
            &named_parent,
            &[store.name.as_str(), REGISTRY_NAME, REGISTRY_LEASE_NAME],
        )?;
        super::require_entries(&named_root, &[JOURNAL_NAME, super::payload::NAME])?;
        check_deadline(deadline)?;

        let db_lease_ids = control_before.file_identities();
        let registry_db = openat(
            &named_parent,
            REGISTRY_NAME,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        let registry_lease = openat(
            &named_parent,
            REGISTRY_LEASE_NAME,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        let registry_db = File::from(registry_db);
        let registry_lease = File::from(registry_lease);
        let marker = File::from(openat(
            &named_root,
            JOURNAL_NAME,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?);
        let payload = File::from(openat(
            &named_root,
            super::payload::NAME,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?);

        let parent_metadata = store.parent.metadata()?;
        let root_metadata = store.root.metadata()?;
        let parent_stamp = ObjectStamp::from(&parent_metadata);
        let root_stamp = ObjectStamp::from(&root_metadata);
        let named_parent_stamp = ObjectStamp::from(&named_parent.metadata()?);
        let named_root_stamp = ObjectStamp::from(&named_root.metadata()?);
        if parent_stamp != named_parent_stamp
            || root_stamp != named_root_stamp
            || parent_stamp.identity != store.parent_id
            || root_stamp.identity != store.root_id
            || parent_stamp.mode & 0o7777 != 0o700
            || root_stamp.mode & 0o7777 != 0o700
            || parent_stamp.uid != rustix::process::geteuid().as_raw()
            || root_stamp.uid != rustix::process::geteuid().as_raw()
            || !parent_metadata.is_dir()
            || !root_metadata.is_dir()
        {
            return Err(StoreError::InvalidIdentity);
        }

        let database_stamp = validate_regular(&registry_db, db_lease_ids[0])?;
        let lease_stamp = validate_regular(&registry_lease, db_lease_ids[1])?;
        let journal_stamp =
            validate_regular(&marker, store.journal_id.ok_or(StoreError::InvalidJournal)?)?;
        let payload_meta = payload.metadata()?;
        let payload_stamp = validate_regular(&payload, (payload_meta.dev(), payload_meta.ino()))?;
        if payload_stamp.len != super::payload::LIMIT
            || payload_stamp.uid != rustix::process::geteuid().as_raw()
        {
            return Err(StoreError::InvalidIdentity);
        }
        super::handoff::read_and_verify_payload(&payload, deadline, super::payload::LIMIT)?;
        let mut marker_copy = marker.try_clone()?;
        let journal_bytes = super::handoff::read_bounded_marker(&mut marker_copy)?;
        if journal_bytes.len() > 512 || journal_bytes != store.journal_contents().as_bytes() {
            return Err(StoreError::InvalidJournal);
        }

        let mut identities = [
            parent_stamp.identity,
            root_stamp.identity,
            journal_stamp.identity,
            payload_stamp.identity,
            database_stamp.identity,
            lease_stamp.identity,
        ];
        identities.sort_unstable();
        if identities.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(StoreError::InvalidIdentity);
        }
        let observed_bytes = [
            object_bytes(parent_stamp)?,
            object_bytes(root_stamp)?,
            object_bytes(journal_stamp)?,
            object_bytes(payload_stamp)?,
            control_before.observed_bytes(),
        ]
        .into_iter()
        .try_fold(0u64, u64::checked_add)
        .ok_or(StoreError::InvalidJournal)?;

        check_deadline(deadline)?;
        after_snapshot(store)?;
        check_deadline(deadline)?;
        let control_after = registry
            .observe_control_footprint()
            .map_err(|_| StoreError::InvalidIdentity)?;
        check_deadline(deadline)?;
        if control_after != control_before {
            return Err(StoreError::InvalidIdentity);
        }
        let final_parent = File::from(super::open_parent(&store.parent_path)?);
        if super::identity(&final_parent)? != store.parent_id {
            return Err(StoreError::InvalidIdentity);
        }
        let final_root = File::from(openat(
            &final_parent,
            &store.name,
            OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?);
        if super::identity(&final_root)? != store.root_id {
            return Err(StoreError::InvalidIdentity);
        }
        super::validate_private_directory(&final_parent)?;
        super::validate_private_directory(&final_root)?;
        super::require_entries(
            &final_parent,
            &[store.name.as_str(), REGISTRY_NAME, REGISTRY_LEASE_NAME],
        )?;
        super::require_entries(&final_root, &[JOURNAL_NAME, super::payload::NAME])?;
        let named_marker = File::from(openat(
            &final_root,
            JOURNAL_NAME,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?);
        let named_payload = File::from(openat(
            &final_root,
            super::payload::NAME,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?);
        let parent_after = ObjectStamp::from(&store.parent.metadata()?);
        let root_after = ObjectStamp::from(&store.root.metadata()?);
        let named_parent_after = ObjectStamp::from(&final_parent.metadata()?);
        let named_root_after = ObjectStamp::from(&final_root.metadata()?);
        if parent_after != parent_stamp
            || named_parent_after != parent_stamp
            || root_after != root_stamp
            || named_root_after != root_stamp
            || ObjectStamp::from(&registry_db.metadata()?) != database_stamp
            || ObjectStamp::from(&registry_lease.metadata()?) != lease_stamp
            || ObjectStamp::from(&marker.metadata()?) != journal_stamp
            || ObjectStamp::from(&payload.metadata()?) != payload_stamp
            || ObjectStamp::from(&named_marker.metadata()?) != journal_stamp
            || ObjectStamp::from(&named_payload.metadata()?) != payload_stamp
        {
            return Err(StoreError::InvalidIdentity);
        }
        let final_state = store.inspect_state()?;
        check_deadline(deadline)?;
        if final_state != super::StoreState::NeedsReconciliation {
            return Err(StoreError::InvalidIdentity);
        }
        Ok(FixtureFootprintObservation {
            object_count: OBJECT_COUNT,
            observed_bytes,
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (store, registry, receipt, after_snapshot);
        Err(StoreError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_regular(file: &File, expected_id: (u64, u64)) -> Result<ObjectStamp, StoreError> {
    let metadata = file.metadata()?;
    let stamp = ObjectStamp::from(&metadata);
    if !metadata.is_file()
        || stamp.identity != expected_id
        || stamp.links != 1
        || stamp.mode & 0o7777 != 0o600
        || stamp.uid != rustix::process::geteuid().as_raw()
    {
        return Err(StoreError::InvalidIdentity);
    }
    Ok(stamp)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn object_bytes(stamp: ObjectStamp) -> Result<u64, StoreError> {
    Ok(stamp.len.max(
        stamp
            .blocks
            .checked_mul(512)
            .ok_or(StoreError::InvalidJournal)?,
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn check_deadline(deadline: Instant) -> Result<(), StoreError> {
    if Instant::now() >= deadline {
        Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "fixed fixture observation deadline",
        )))
    } else {
        Ok(())
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    const FIXTURE_ID: &str = "0123456789abcdef0123456789abcdef";

    fn completed_fixture() -> (OwnedStore, VerificationStoreRegistry, StopReceipt) {
        let mut store = OwnedStore::create().unwrap();
        let registry_path = store.control_registry_path().unwrap();
        let (parent, root) = store.control_identity().unwrap();
        let mut registry = VerificationStoreRegistry::open(registry_path).unwrap();
        registry
            .register(FIXTURE_ID, parent.0, parent.1, root.0, root.1)
            .unwrap();
        registry
            .reserve(FIXTURE_ID, 0, super::super::WORKER_PAYLOAD_RESERVATION)
            .unwrap();
        registry
            .transition(
                FIXTURE_ID,
                verification_store_registry::StoreState::Registered,
                verification_store_registry::StoreState::Active,
            )
            .unwrap();
        store.bind_worker_payload_reservation().unwrap();
        let descriptor = store.worker_stdin_once().unwrap();
        let pid = std::process::id();
        store.bind_worker(pid).unwrap();
        super::super::handoff::write_payload_from_root(&descriptor).unwrap();
        let receipt = StopReceipt::confirmed(store.token(), pid);
        store.record_stopped(&receipt).unwrap();
        registry
            .transition(
                FIXTURE_ID,
                verification_store_registry::StoreState::Active,
                verification_store_registry::StoreState::Stopped,
            )
            .unwrap();
        (store, registry, receipt)
    }

    #[test]
    fn stopped_fixture_footprint_is_a_stable_six_object_observation() {
        let (store, registry, receipt) = completed_fixture();
        let observation = store
            .observe_stopped_fixture_footprint(&registry, &receipt)
            .unwrap();
        assert_eq!(observation.object_count, OBJECT_COUNT);
        assert!(observation.observed_bytes >= super::super::payload::LIMIT);
        assert!(observation.observed_bytes <= super::super::WORKER_PAYLOAD_RESERVATION);
    }

    #[test]
    fn payload_change_after_initial_snapshot_refuses_the_observation() {
        let (store, registry, receipt) = completed_fixture();
        let invoked = std::cell::Cell::new(false);
        let result = observe_with_checkpoint(&store, &registry, &receipt, |owned| {
            invoked.set(true);
            let payload = fs::OpenOptions::new()
                .write(true)
                .open(owned.parent_path.join("store/payload.bin"))?;
            payload.set_len(super::super::payload::LIMIT - 1)?;
            Ok(())
        });
        assert!(
            invoked.get(),
            "mutation must happen after the initial snapshot"
        );
        assert!(matches!(result, Err(StoreError::InvalidIdentity)));
        assert_eq!(
            fs::metadata(store.parent_path.join("store/payload.bin"))
                .unwrap()
                .len(),
            super::super::payload::LIMIT - 1
        );
    }

    #[test]
    fn stopped_fixture_footprint_requires_exact_stopped_receipt() {
        let store = OwnedStore::create().unwrap();
        let registry_path = store.control_registry_path().unwrap();
        let registry = VerificationStoreRegistry::open(registry_path).unwrap();
        let receipt = StopReceipt::confirmed(store.token(), std::process::id());
        assert!(store
            .observe_stopped_fixture_footprint(&registry, &receipt)
            .is_err());

        let (store, registry, receipt) = completed_fixture();
        let wrong = StopReceipt::confirmed("different-token", receipt.pid());
        assert!(store
            .observe_stopped_fixture_footprint(&registry, &wrong)
            .is_err());
        let wrong_pid =
            StopReceipt::confirmed(receipt.token(), receipt.pid().wrapping_add(1).max(1));
        assert!(store
            .observe_stopped_fixture_footprint(&registry, &wrong_pid)
            .is_err());
    }

    #[test]
    fn stopped_fixture_footprint_refuses_unknown_entries_and_sqlite_sidecars() {
        let (store, registry, receipt) = completed_fixture();
        fs::write(
            store.parent_path.join("verification-registry.sqlite3-wal"),
            b"x",
        )
        .unwrap();
        assert!(store
            .observe_stopped_fixture_footprint(&registry, &receipt)
            .is_err());

        let (store, registry, receipt) = completed_fixture();
        fs::write(store.parent_path.join("unexpected"), b"retained").unwrap();
        assert!(store
            .observe_stopped_fixture_footprint(&registry, &receipt)
            .is_err());
    }

    #[test]
    fn stopped_fixture_footprint_refuses_unsafe_modes_and_links() {
        let (store, registry, receipt) = completed_fixture();
        let marker = store.parent_path.join("store/owner.journal");
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store
            .observe_stopped_fixture_footprint(&registry, &receipt)
            .is_err());

        let (store, registry, receipt) = completed_fixture();
        let payload = store.parent_path.join("store/payload.bin");
        let hardlink = store.parent_path.join("store/payload.link");
        fs::hard_link(&payload, &hardlink).unwrap();
        assert!(store
            .observe_stopped_fixture_footprint(&registry, &receipt)
            .is_err());

        let (store, registry, receipt) = completed_fixture();
        let marker = store.parent_path.join("store/owner.journal");
        let saved = store.parent_path.join("store/owner.saved");
        fs::rename(&marker, &saved).unwrap();
        std::os::unix::fs::symlink("owner.saved", &marker).unwrap();
        assert!(store
            .observe_stopped_fixture_footprint(&registry, &receipt)
            .is_err());
    }
}
