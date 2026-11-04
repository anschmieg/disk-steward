//! Private, app-created Git-worker store fixture with bounded live cleanup.

#[path = "store/footprint.rs"]
mod footprint;
#[path = "store/handoff.rs"]
mod handoff;
#[path = "store/payload.rs"]
mod payload;

use std::{
    fs::File,
    io::{Read, Seek, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub enum StoreError {
    #[allow(dead_code)]
    UnsupportedPlatform,
    Io(std::io::Error),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Rustix(rustix::io::Errno),
    InvalidIdentity,
    InvalidJournal,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPlatform => f.write_str("unsupported platform"),
            Self::Io(error) => write!(f, "store I/O: {error}"),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Rustix(error) => write!(f, "store filesystem operation: {error}"),
            Self::InvalidIdentity => f.write_str("store identity mismatch"),
            Self::InvalidJournal => f.write_str("store journal mismatch"),
        }
    }
}

impl std::error::Error for StoreError {}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl From<rustix::io::Errno> for StoreError {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Rustix(error)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreState {
    NeedsReconciliation,
}

/// Read-only identities captured from a completed, dead creator process.
#[derive(Clone, Debug)]
pub struct AbandonedSnapshot {
    pub token: String,
    pub parent_id: (u64, u64),
    pub root_id: (u64, u64),
    pub journal_id: (u64, u64),
    pub bound_pid: u32,
    pub version: u32,
}

/// Process-local handle for one newly created private store.
pub struct OwnedStore {
    parent_path: PathBuf,
    parent: File,
    root: File,
    name: String,
    parent_id: (u64, u64),
    root_id: (u64, u64),
    token: String,
    journal_id: Option<(u64, u64)>,
    worker_payload_reservation: Option<u64>,
    worker_payload_reservation_attempted: bool,
    phase: Phase,
    poisoned: bool,
    cleanup_started: bool,
    worker_stdin_issued: bool,
    #[cfg(test)]
    fail_after_marker_unlink: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Created,
    Bound(u32),
    Stopped(u32),
}

pub(crate) type ControlIdentity = ((u64, u64), (u64, u64));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WorkerPayloadObservation {
    pub(crate) written_bytes: u64,
    pub(crate) high_water_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureFootprintObservation {
    pub(crate) object_count: u8,
    pub(crate) observed_bytes: u64,
}

pub(crate) const PARTIAL_PAYLOAD_FIXTURE_BYTES: u64 = payload::PARTIAL_FIXTURE_BYTES;
pub(crate) const WORKER_PAYLOAD_RESERVATION: u64 = 128 * 1024;

/// Run the fixed synthetic payload writer against the validated worker stdin
/// directory capability. No paths or payload parameters are accepted.
pub(crate) fn write_worker_stdin_payload() -> Result<payload::PayloadReport, StoreError> {
    handoff::write_stdin_payload()
}

pub(crate) fn write_worker_stdin_partial_payload() -> Result<payload::PayloadReport, StoreError> {
    handoff::write_stdin_payload_partial()
}

impl OwnedStore {
    /// Return the verified store-root descriptor for the single fixed worker
    /// stdin handoff. The child receives only this directory capability.
    pub(crate) fn worker_stdin_once(&mut self) -> Result<File, StoreError> {
        handoff::worker_stdin_once(self)
    }

    pub(crate) fn validate_worker_stdin_handoff() -> Result<(), StoreError> {
        handoff::validate_stdin()
    }

    pub(crate) fn inspect_stopped_worker_payload(
        &self,
        receipt: &crate::StopReceipt,
    ) -> Result<WorkerPayloadObservation, StoreError> {
        handoff::inspect_stopped_worker_payload(self, receipt)
    }

    pub(crate) fn inspect_stopped_worker_partial_payload(
        &self,
        receipt: &crate::StopReceipt,
    ) -> Result<WorkerPayloadObservation, StoreError> {
        handoff::inspect_stopped_worker_partial_payload(self, receipt)
    }

    pub(crate) fn observe_stopped_fixture_footprint(
        &self,
        registry: &verification_store_registry::VerificationStoreRegistry,
        receipt: &crate::StopReceipt,
    ) -> Result<FixtureFootprintObservation, StoreError> {
        footprint::observe_stopped_fixture(self, registry, receipt)
    }

    pub(crate) fn control_registry_path(&self) -> Result<PathBuf, StoreError> {
        self.inspect_state()?;
        Ok(self.parent_path.join("verification-registry.sqlite3"))
    }

    pub(crate) fn control_identity(&self) -> Result<ControlIdentity, StoreError> {
        self.inspect_state()?;
        Ok((self.parent_id, self.root_id))
    }

    /// Creates a fresh private parent and store. There is intentionally no
    /// caller-supplied path and no TempDir/drop cleanup behavior.
    pub fn create() -> Result<Self, StoreError> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            return Err(StoreError::UnsupportedPlatform);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        create_supported()
    }

    pub fn inspect_state(&self) -> Result<StoreState, StoreError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use rustix::fs::{openat, OFlags};
            if identity(&self.parent)? != self.parent_id || identity(&self.root)? != self.root_id {
                return Err(StoreError::InvalidIdentity);
            }
            let parent = open_parent(&self.parent_path)?;
            if identity(&parent)? != self.parent_id {
                return Err(StoreError::InvalidIdentity);
            }
            validate_private_directory(&parent)?;
            let root = openat(
                &parent,
                &self.name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )?;
            if identity(&root)? != self.root_id {
                return Err(StoreError::InvalidIdentity);
            }
            validate_private_directory(&root)?;
            let marker = openat(
                &root,
                "owner.journal",
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )?;
            let mut bytes = Vec::new();
            let marker = File::from(marker);
            let metadata = marker.metadata()?;
            use std::os::unix::fs::MetadataExt;
            if !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.mode() & 0o777 != 0o600
                || metadata.uid() != rustix::process::geteuid().as_raw()
            {
                return Err(StoreError::InvalidJournal);
            }
            marker.take(512).read_to_end(&mut bytes)?;
            let expected = self.journal_contents();
            if bytes != expected.as_bytes() {
                return Err(StoreError::InvalidJournal);
            }
            Ok(StoreState::NeedsReconciliation)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(StoreError::UnsupportedPlatform)
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn abandon_record(&self, pid: u32) -> Result<String, StoreError> {
        if self.phase != Phase::Bound(pid) || pid == 0 {
            return Err(StoreError::InvalidJournal);
        }
        let (journal_dev, journal_ino) = self.journal_id.ok_or(StoreError::InvalidJournal)?;
        Ok(format!(
            "v1\n{}\n{}:{}\n{}:{}\n{}:{}\n{}\nend\n",
            self.token,
            self.parent_id.0,
            self.parent_id.1,
            self.root_id.0,
            self.root_id.1,
            journal_dev,
            journal_ino,
            pid
        ))
    }

    /// Reopen and inspect only a canonical fixture identified by a closed token.
    pub fn inspect_abandoned(snapshot: &AbandonedSnapshot) -> Result<StoreState, StoreError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            inspect_abandoned_supported(snapshot)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = snapshot;
            Err(StoreError::UnsupportedPlatform)
        }
    }

    fn journal_contents(&self) -> String {
        let phase = match self.phase {
            Phase::Created => "created".to_string(),
            Phase::Bound(pid) => format!("bound:{pid}"),
            Phase::Stopped(pid) => format!("stopped:{pid}"),
        };
        let reservation = self
            .worker_payload_reservation
            .map(|bytes| format!("reservation:{bytes}\n"))
            .unwrap_or_default();
        format!(
            "disk-steward-worker-store-v1\n{}\n{}:{}\n{}:{}\n{}{}\n",
            self.token,
            self.parent_id.0,
            self.parent_id.1,
            self.root_id.0,
            self.root_id.1,
            reservation,
            phase
        )
    }

    pub fn bind_worker(&mut self, pid: u32) -> Result<(), StoreError> {
        if self.phase != Phase::Created || pid == 0 {
            return Err(StoreError::InvalidJournal);
        }
        self.update_journal(Phase::Bound(pid))
    }

    /// Bind the one closed worker payload admission after the registry has
    /// committed its matching reservation and before descriptor handoff.
    pub(crate) fn bind_worker_payload_reservation(&mut self) -> Result<(), StoreError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            if self.worker_payload_reservation_attempted {
                return Err(StoreError::InvalidJournal);
            }
            self.worker_payload_reservation_attempted = true;
            if self.phase != Phase::Created
                || self.poisoned
                || self.cleanup_started
                || self.worker_stdin_issued
                || self.worker_payload_reservation.is_some()
            {
                self.poisoned = true;
                return Err(StoreError::InvalidJournal);
            }
            self.update_journal_fields(self.phase, Some(WORKER_PAYLOAD_RESERVATION))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(StoreError::UnsupportedPlatform)
        }
    }

    pub fn record_stopped(&mut self, receipt: &crate::StopReceipt) -> Result<(), StoreError> {
        let pid = receipt.pid();
        if receipt.token() != self.token || self.phase != Phase::Bound(pid) {
            return Err(StoreError::InvalidJournal);
        }
        self.update_journal(Phase::Stopped(pid))
    }

    /// Remove only this fixture's recorded marker and fixed store directory.
    /// The retained parent journal makes the operation explicitly one-way.
    pub fn cleanup_after_stopped(
        &mut self,
        receipt: &crate::StopReceipt,
    ) -> Result<(), StoreError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            if self.poisoned
                || self.cleanup_started
                || self.phase != Phase::Stopped(receipt.pid())
                || receipt.token() != self.token
                || receipt.pid() == 0
            {
                return Err(StoreError::InvalidJournal);
            }
            self.cleanup_started = true;
            let result = self.cleanup_supported(receipt.pid());
            if result.is_err() {
                self.poisoned = true;
            }
            result
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = receipt;
            Err(StoreError::UnsupportedPlatform)
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn cleanup_supported(&mut self, pid: u32) -> Result<(), StoreError> {
        use rustix::fs::{openat, unlinkat, AtFlags, OFlags};
        let named_parent = open_parent(&self.parent_path)?;
        if identity(&named_parent)? != self.parent_id {
            return Err(StoreError::InvalidIdentity);
        }
        let parent = &self.parent;
        if identity(parent)? != self.parent_id || identity(&self.root)? != self.root_id {
            return Err(StoreError::InvalidIdentity);
        }
        validate_private_directory(parent)?;
        validate_private_directory(&self.root)?;
        let named_root = openat(
            parent,
            &self.name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        if identity(&named_root)? != self.root_id {
            return Err(StoreError::InvalidIdentity);
        }
        let marker = openat(
            &self.root,
            "owner.journal",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )?;
        let marker = File::from(marker);
        let metadata = marker.metadata()?;
        use std::os::unix::fs::MetadataExt;
        let marker_id = (metadata.dev(), metadata.ino());
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || Some(marker_id) != self.journal_id
        {
            return Err(StoreError::InvalidJournal);
        }
        let mut contents = Vec::new();
        marker.take(512).read_to_end(&mut contents)?;
        if contents != self.journal_contents().as_bytes() {
            return Err(StoreError::InvalidJournal);
        }
        require_entries(&self.root, &["owner.journal"])?;
        require_entries(parent, &["store"])?;

        let intent = openat(
            parent,
            "cleanup.journal",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::from_bits(0o600).unwrap(),
        )?;
        let mut intent = File::from(intent);
        let pending = format!(
            "disk-steward-cleanup-v1\n{}\n{}:{}\n{}:{}\n{}\npending\n",
            self.token, self.parent_id.0, self.parent_id.1, self.root_id.0, self.root_id.1, pid
        );
        intent.write_all(pending.as_bytes())?;
        intent.sync_all()?;
        parent.sync_all()?;

        // Recheck named identities immediately before each unlink.
        let marker_check = openat(
            &self.root,
            "owner.journal",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )?;
        let mut marker_check = File::from(marker_check);
        let mut rechecked = Vec::new();
        Read::by_ref(&mut marker_check)
            .take(512)
            .read_to_end(&mut rechecked)?;
        if rechecked != self.journal_contents().as_bytes() {
            return Err(StoreError::InvalidJournal);
        }
        require_entries(&self.root, &["owner.journal"])?;
        require_entries(parent, &["store", "cleanup.journal"])?;
        if identity(&marker_check)? != marker_id
            || identity(parent)? != self.parent_id
            || identity(&self.root)? != self.root_id
        {
            return Err(StoreError::InvalidIdentity);
        }
        unlinkat(&self.root, "owner.journal", AtFlags::empty())?;
        self.root.sync_all()?;
        #[cfg(test)]
        if self.fail_after_marker_unlink {
            return Err(StoreError::InvalidJournal);
        }
        let root_check = openat(
            parent,
            &self.name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        if identity(&root_check)? != self.root_id {
            return Err(StoreError::InvalidIdentity);
        }
        require_entries(&self.root, &[])?;
        unlinkat(parent, &self.name, AtFlags::REMOVEDIR)?;
        parent.sync_all()?;
        let completed = pending.replace("pending\n", "completed\n");
        intent.set_len(0)?;
        use std::io::Seek;
        intent.rewind()?;
        intent.write_all(completed.as_bytes())?;
        intent.sync_all()?;
        parent.sync_all()?;
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn update_journal(&mut self, next: Phase) -> Result<(), StoreError> {
        self.update_journal_fields(next, self.worker_payload_reservation)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn update_journal_fields(
        &mut self,
        next: Phase,
        next_reservation: Option<u64>,
    ) -> Result<(), StoreError> {
        use rustix::fs::{openat, OFlags};
        if self.poisoned {
            return Err(StoreError::InvalidJournal);
        }
        let result = (|| {
            if identity(&self.parent)? != self.parent_id || identity(&self.root)? != self.root_id {
                return Err(StoreError::InvalidIdentity);
            }
            let parent = open_parent(&self.parent_path)?;
            if identity(&parent)? != self.parent_id {
                return Err(StoreError::InvalidIdentity);
            }
            validate_private_directory(&parent)?;
            let root = openat(
                &parent,
                &self.name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )?;
            if identity(&root)? != self.root_id {
                return Err(StoreError::InvalidIdentity);
            }
            validate_private_directory(&root)?;
            let fd = openat(
                &root,
                "owner.journal",
                OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )?;
            let mut marker = File::from(fd);
            let metadata = marker.metadata()?;
            use std::os::unix::fs::MetadataExt;
            let marker_id = (metadata.dev(), metadata.ino());
            if !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.mode() & 0o777 != 0o600
                || metadata.uid() != rustix::process::geteuid().as_raw()
                || self.journal_id != Some(marker_id)
            {
                return Err(StoreError::InvalidJournal);
            }
            let mut current = Vec::new();
            marker.try_clone()?.take(512).read_to_end(&mut current)?;
            if current != self.journal_contents().as_bytes() {
                return Err(StoreError::InvalidJournal);
            }
            marker.set_len(0)?;
            marker.rewind()?;
            let old_phase = self.phase;
            let old_reservation = self.worker_payload_reservation;
            self.phase = next;
            self.worker_payload_reservation = next_reservation;
            if let Err(error) = marker
                .write_all(self.journal_contents().as_bytes())
                .and_then(|_| marker.sync_all())
            {
                self.phase = old_phase;
                self.worker_payload_reservation = old_reservation;
                return Err(StoreError::Io(error));
            }
            self.journal_id = Some(identity(&marker)?);
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn update_journal(&mut self, _next: Phase) -> Result<(), StoreError> {
        Err(StoreError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn create_supported() -> Result<OwnedStore, StoreError> {
    use rustix::fs::{mkdirat, openat, Mode, OFlags};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let temp_path = std::env::temp_dir().canonicalize()?;
    let temp_anchor = open_parent(&temp_path)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::InvalidIdentity)?
        .as_nanos();
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let token = format!("{:x}-{:x}-{:x}", std::process::id(), nonce, serial);
    let parent_name = format!("disk-steward-worker-{token}");
    mkdirat(
        &temp_anchor,
        &parent_name,
        Mode::RUSR | Mode::WUSR | Mode::XUSR,
    )?;
    let parent = openat(
        &temp_anchor,
        &parent_name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    validate_private_directory(&parent)?;
    let parent_id = identity(&parent)?;
    let parent_path = temp_path.join(&parent_name);
    File::from(temp_anchor.try_clone()?).sync_all()?;
    let name = "store".to_string();
    mkdirat(&parent, &name, Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
    let root = openat(
        &parent,
        &name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    validate_private_directory(&root)?;
    File::from(parent.try_clone()?).sync_all()?;
    let root_id = identity(&root)?;
    let marker = openat(
        &root,
        "owner.journal",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?;
    let mut marker = File::from(marker);
    writeln!(marker, "disk-steward-worker-store-v1")?;
    writeln!(marker, "{token}")?;
    writeln!(marker, "{}:{}", parent_id.0, parent_id.1)?;
    writeln!(marker, "{}:{}", root_id.0, root_id.1)?;
    writeln!(marker, "created")?;
    marker.sync_all()?;
    use std::os::unix::fs::MetadataExt;
    let journal_id = Some((marker.metadata()?.dev(), marker.metadata()?.ino()));
    drop(marker);
    // Keep open descriptors as the authority; the path is retained only to
    // reacquire and compare the internally generated parent on reopen.
    File::from(root.try_clone()?).sync_all()?;
    File::from(parent.try_clone()?).sync_all()?;
    Ok(OwnedStore {
        parent_path,
        parent: File::from(parent),
        root: File::from(root),
        name,
        parent_id,
        root_id,
        token,
        journal_id,
        worker_payload_reservation: None,
        worker_payload_reservation_attempted: false,
        phase: Phase::Created,
        poisoned: false,
        cleanup_started: false,
        worker_stdin_issued: false,
        #[cfg(test)]
        fail_after_marker_unlink: false,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_private_directory(fd: &impl std::os::fd::AsFd) -> Result<(), StoreError> {
    let stat = rustix::fs::fstat(fd)?;
    if stat.st_mode & 0o777 != 0o700 || stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(StoreError::InvalidIdentity);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn require_entries(fd: &impl std::os::fd::AsFd, expected: &[&str]) -> Result<(), StoreError> {
    let mut seen = vec![false; expected.len()];
    for entry in rustix::fs::Dir::read_from(fd)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let Some(index) = expected
            .iter()
            .position(|expected| expected.as_bytes() == name)
        else {
            return Err(StoreError::InvalidJournal);
        };
        if seen[index] {
            return Err(StoreError::InvalidJournal);
        }
        seen[index] = true;
    }
    if seen.iter().all(|seen| *seen) {
        Ok(())
    } else {
        Err(StoreError::InvalidJournal)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_parent(path: &std::path::Path) -> Result<std::os::fd::OwnedFd, StoreError> {
    use rustix::fs::{open, Mode, OFlags};
    Ok(open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn identity(fd: &impl std::os::fd::AsFd) -> Result<(u64, u64), StoreError> {
    let stat = rustix::fs::fstat(fd)?;
    Ok((stat.st_dev as u64, stat.st_ino as u64))
}

pub(crate) fn valid_token(token: &str) -> bool {
    if token.len() > 128 {
        return false;
    }
    let parts: Vec<_> = token.split('-').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn inspect_abandoned_supported(snapshot: &AbandonedSnapshot) -> Result<StoreState, StoreError> {
    use rustix::fs::{openat, OFlags};
    use std::os::unix::fs::MetadataExt;
    if snapshot.version != 1 || snapshot.bound_pid == 0 || !valid_token(&snapshot.token) {
        return Err(StoreError::InvalidIdentity);
    }
    let temp = std::env::temp_dir().canonicalize()?;
    let temp_fd = open_parent(&temp)?;
    let dirname = format!("disk-steward-worker-{}", snapshot.token);
    let parent = openat(
        &temp_fd,
        &dirname,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    validate_private_directory(&parent)?;
    if identity(&parent)? != snapshot.parent_id {
        return Err(StoreError::InvalidIdentity);
    }
    let root = openat(
        &parent,
        "store",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    validate_private_directory(&root)?;
    if identity(&root)? != snapshot.root_id {
        return Err(StoreError::InvalidIdentity);
    }
    let marker = openat(
        &root,
        "owner.journal",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?;
    let marker = File::from(marker);
    let metadata = marker.metadata()?;
    let marker_id = (metadata.dev(), metadata.ino());
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || marker_id != snapshot.journal_id
    {
        return Err(StoreError::InvalidJournal);
    }
    let expected = format!(
        "disk-steward-worker-store-v1\n{}\n{}:{}\n{}:{}\nbound:{}\n",
        snapshot.token,
        snapshot.parent_id.0,
        snapshot.parent_id.1,
        snapshot.root_id.0,
        snapshot.root_id.1,
        snapshot.bound_pid
    );
    let mut bytes = Vec::new();
    marker.take(512).read_to_end(&mut bytes)?;
    if bytes != expected.as_bytes() {
        return Err(StoreError::InvalidJournal);
    }
    Ok(StoreState::NeedsReconciliation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StopReceipt;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn fresh_store_inspects_only_as_reconciliation() {
        let store = OwnedStore::create().unwrap();
        assert!(!store.token().is_empty());
        assert_eq!(
            store.inspect_state().unwrap(),
            StoreState::NeedsReconciliation
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn payload_reservation_binding_is_once_only_and_immutable() {
        let mut store = OwnedStore::create().unwrap();
        let path = store.parent_path.join("store/owner.journal");
        let before = std::fs::read(&path).unwrap();
        store.bind_worker_payload_reservation().unwrap();
        let bound = std::fs::read(&path).unwrap();
        assert_ne!(bound, before);
        let text = std::str::from_utf8(&bound).unwrap();
        assert!(text.contains("reservation:131072\ncreated\n"));
        assert!(store.bind_worker_payload_reservation().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bound);
        let _descriptor = store.worker_stdin_once().unwrap();
        assert!(store.bind_worker_payload_reservation().is_err());
        assert_eq!(std::fs::read(path).unwrap(), bound);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn failed_payload_reservation_binding_poisoned_without_rewriting_journal() {
        let mut store = OwnedStore::create().unwrap();
        store.bind_worker(41).unwrap();
        let path = store.parent_path.join("store/owner.journal");
        let before = std::fs::read(&path).unwrap();
        assert!(store.bind_worker_payload_reservation().is_err());
        assert!(store.poisoned);
        assert!(store.worker_payload_reservation_attempted);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(store.worker_stdin_once().is_err());

        use std::os::unix::fs::PermissionsExt;
        let mut fresh = OwnedStore::create().unwrap();
        let marker = fresh.parent_path.join("store/owner.journal");
        let original = std::fs::read(&marker).unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(fresh.bind_worker_payload_reservation().is_err());
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(fresh.poisoned);
        assert!(fresh.worker_payload_reservation.is_none());
        assert!(fresh.bind_worker_payload_reservation().is_err());
        assert!(fresh.worker_stdin_once().is_err());
        assert_eq!(std::fs::read(marker).unwrap(), original);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn changed_owner_journal_is_rejected() {
        use rustix::fs::{openat, OFlags};
        use std::io::Write;
        let store = OwnedStore::create().unwrap();
        let marker = openat(
            &store.root,
            "owner.journal",
            OFlags::WRONLY | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let mut marker = File::from(marker);
        marker.write_all(b"forged\n").unwrap();
        marker.sync_all().unwrap();
        assert!(matches!(
            store.inspect_state(),
            Err(StoreError::InvalidJournal)
        ));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn another_created_root_cannot_satisfy_recorded_identity() {
        let mut first = OwnedStore::create().unwrap();
        let mut second = OwnedStore::create().unwrap();
        std::mem::swap(&mut first.parent_path, &mut second.parent_path);
        assert!(matches!(
            first.inspect_state(),
            Err(StoreError::InvalidIdentity)
        ));
        assert!(matches!(
            second.inspect_state(),
            Err(StoreError::InvalidIdentity)
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn public_owner_journal_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let store = OwnedStore::create().unwrap();
        let path = store.parent_path.join(&store.name).join("owner.journal");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            store.inspect_state(),
            Err(StoreError::InvalidJournal)
        ));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn changed_private_parent_mode_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let mut store = OwnedStore::create().unwrap();
        let journal = store.parent_path.join(&store.name).join("owner.journal");
        let original = std::fs::read(&journal).unwrap();
        assert_eq!(
            store.parent.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            store.root.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::set_permissions(&store.parent_path, std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(matches!(
            store.inspect_state(),
            Err(StoreError::InvalidIdentity)
        ));
        assert!(store.bind_worker(41).is_err());
        assert_eq!(std::fs::read(&journal).unwrap(), original);
        std::fs::set_permissions(&store.parent_path, std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn symlinked_owner_journal_is_not_followed() {
        let store = OwnedStore::create().unwrap();
        let root = store.parent_path.join(&store.name);
        std::fs::rename(root.join("owner.journal"), root.join("saved.journal")).unwrap();
        std::os::unix::fs::symlink("saved.journal", root.join("owner.journal")).unwrap();
        assert!(store.inspect_state().is_err());
        assert!(std::fs::read(root.join("saved.journal"))
            .unwrap()
            .starts_with(b"disk-steward-worker-store-v1\n"));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn failed_update_poisoning_prevents_retry_after_permissions_restored() {
        use std::os::unix::fs::PermissionsExt;
        let mut store = OwnedStore::create().unwrap();
        let path = store.parent_path.join(&store.name).join("owner.journal");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.bind_worker(41).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(store.bind_worker(41).is_err());
        assert_eq!(store.phase, Phase::Created);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn same_contents_in_replaced_marker_are_not_mutated() {
        let mut store = OwnedStore::create().unwrap();
        let root = store.parent_path.join(&store.name);
        let path = root.join("owner.journal");
        let expected = std::fs::read(&path).unwrap();
        std::fs::rename(&path, root.join("saved.journal")).unwrap();
        use std::os::unix::fs::OpenOptionsExt;
        let mut replacement = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        replacement.write_all(&expected).unwrap();
        replacement.sync_all().unwrap();
        assert!(store.bind_worker(41).is_err());
        assert_eq!(std::fs::read(path).unwrap(), expected);
        assert_eq!(std::fs::read(root.join("saved.journal")).unwrap(), expected);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn stopped_owned_store_cleanup_removes_only_root_and_retains_completed_parent_journal() {
        let mut store = OwnedStore::create().unwrap();
        store.bind_worker(41).unwrap();
        let receipt = StopReceipt::confirmed(store.token(), 41);
        store.record_stopped(&receipt).unwrap();
        let parent = store.parent_path.clone();
        let root = parent.join("store");
        store.cleanup_after_stopped(&receipt).unwrap();
        assert!(!root.exists());
        let record = std::fs::read_to_string(parent.join("cleanup.journal")).unwrap();
        assert!(record.ends_with("completed\n"));
        assert!(record.contains(store.token()));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn cleanup_refuses_unknown_root_entry_without_mutation() {
        let mut store = OwnedStore::create().unwrap();
        store.bind_worker(41).unwrap();
        let receipt = StopReceipt::confirmed(store.token(), 41);
        store.record_stopped(&receipt).unwrap();
        let unknown = store.parent_path.join("store/unknown");
        std::fs::write(&unknown, b"keep").unwrap();
        assert!(store.cleanup_after_stopped(&receipt).is_err());
        assert!(unknown.exists());
        assert!(!store.parent_path.join("cleanup.journal").exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn cleanup_refuses_unknown_parent_entry_without_mutation() {
        let mut store = OwnedStore::create().unwrap();
        store.bind_worker(41).unwrap();
        let receipt = StopReceipt::confirmed(store.token(), 41);
        store.record_stopped(&receipt).unwrap();
        let unknown = store.parent_path.join("keep");
        std::fs::write(&unknown, b"keep").unwrap();
        assert!(store.cleanup_after_stopped(&receipt).is_err());
        assert!(unknown.exists());
        assert!(store.parent_path.join("store/owner.journal").exists());
        assert!(!store.parent_path.join("cleanup.journal").exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn cleanup_refuses_before_stopped_phase_and_with_replaced_or_symlink_marker() {
        let mut prebound = OwnedStore::create().unwrap();
        prebound.bind_worker(41).unwrap();
        let receipt = StopReceipt::confirmed(prebound.token(), 41);
        assert!(prebound.cleanup_after_stopped(&receipt).is_err());
        assert!(prebound.parent_path.join("store/owner.journal").exists());
        assert!(!prebound.parent_path.join("cleanup.journal").exists());

        for symlink in [false, true] {
            let mut store = OwnedStore::create().unwrap();
            store.bind_worker(41).unwrap();
            let receipt = StopReceipt::confirmed(store.token(), 41);
            store.record_stopped(&receipt).unwrap();
            let root = store.parent_path.join("store");
            let marker = root.join("owner.journal");
            let saved = root.join("saved.journal");
            std::fs::rename(&marker, &saved).unwrap();
            if symlink {
                std::os::unix::fs::symlink("saved.journal", &marker).unwrap();
            } else {
                use std::os::unix::fs::OpenOptionsExt;
                let mut replacement = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&marker)
                    .unwrap();
                replacement
                    .write_all(&std::fs::read(&saved).unwrap())
                    .unwrap();
            }
            assert!(store.cleanup_after_stopped(&receipt).is_err());
            assert!(saved.exists());
            assert!(marker.exists());
            assert!(!store.parent_path.join("cleanup.journal").exists());
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn cleanup_refuses_wrong_receipt_and_replay() {
        let mut store = OwnedStore::create().unwrap();
        let wrong = StopReceipt::confirmed("wrong-token", 41);
        assert!(store.cleanup_after_stopped(&wrong).is_err());
        store.bind_worker(41).unwrap();
        let receipt = StopReceipt::confirmed(store.token(), 41);
        store.record_stopped(&receipt).unwrap();
        assert!(store.cleanup_after_stopped(&wrong).is_err());
        let wrong_pid = StopReceipt::confirmed(store.token(), 42);
        assert!(store.cleanup_after_stopped(&wrong_pid).is_err());
        assert!(!store.parent_path.join("cleanup.journal").exists());
        store.cleanup_after_stopped(&receipt).unwrap();
        assert!(store.cleanup_after_stopped(&receipt).is_err());
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn partial_cleanup_retains_pending_intent_and_refuses_retry() {
        let mut store = OwnedStore::create().unwrap();
        store.bind_worker(41).unwrap();
        let receipt = StopReceipt::confirmed(store.token(), 41);
        store.record_stopped(&receipt).unwrap();
        store.fail_after_marker_unlink = true;
        assert!(store.cleanup_after_stopped(&receipt).is_err());
        assert!(store.parent_path.join("store").is_dir());
        assert!(!store.parent_path.join("store/owner.journal").exists());
        let path = store.parent_path.join("cleanup.journal");
        let pending = std::fs::read(&path).unwrap();
        assert!(pending.ends_with(b"pending\n"));
        store.fail_after_marker_unlink = false;
        assert!(store.cleanup_after_stopped(&receipt).is_err());
        assert_eq!(std::fs::read(path).unwrap(), pending);
        assert!(store.parent_path.join("store").is_dir());
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn fresh_descriptors_inspect_without_mutation_and_refuse_changed_marker() {
        let mut store = OwnedStore::create().unwrap();
        store.bind_worker(41).unwrap();
        let snapshot =
            crate::parse_abandoned_record(store.abandon_record(41).unwrap().as_bytes()).unwrap();
        let marker = store.parent_path.join("store/owner.journal");
        let before = std::fs::read(&marker).unwrap();
        drop(store);
        assert_eq!(
            OwnedStore::inspect_abandoned(&snapshot).unwrap(),
            StoreState::NeedsReconciliation
        );
        assert_eq!(std::fs::read(&marker).unwrap(), before);
        std::fs::write(&marker, b"altered fixture journal\n").unwrap();
        assert!(OwnedStore::inspect_abandoned(&snapshot).is_err());
        assert_eq!(std::fs::read(marker).unwrap(), b"altered fixture journal\n");
    }
}
