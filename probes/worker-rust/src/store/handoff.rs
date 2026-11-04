//! One-shot, read-only transfer of the fixed worker store root through stdin.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::Phase;
use super::{OwnedStore, StoreError};
use std::fs::File;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    io::{self, Read},
    os::fd::AsFd,
    thread,
    time::{Duration, Instant},
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::fs::MetadataExt;

#[cfg(any(target_os = "linux", target_os = "macos"))]
const HANDOFF_WAIT: Duration = Duration::from_millis(500);
#[cfg(any(target_os = "linux", target_os = "macos"))]
const POLL_INTERVAL: Duration = Duration::from_millis(2);
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_POLLS: usize = 256;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const JOURNAL_LIMIT: usize = 512;

/// Clone the checked root descriptor once for the closed worker handoff.
pub(super) fn worker_stdin_once(store: &mut OwnedStore) -> Result<File, StoreError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use rustix::fs::{openat, OFlags};

        if store.phase != Phase::Created
            || store.poisoned
            || store.cleanup_started
            || store.worker_stdin_issued
        {
            return Err(StoreError::InvalidJournal);
        }
        // Consume this attempt before fallible validation. Restoring a changed
        // marker must not renew a previously refused handoff.
        store.worker_stdin_issued = true;
        store.inspect_state()?;
        super::require_entries(&store.root, &["owner.journal"])?;
        super::validate_private_directory(&store.root)?;
        if super::identity(&store.root)? != store.root_id {
            return Err(StoreError::InvalidIdentity);
        }
        let marker = openat(
            &store.root,
            "owner.journal",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )?;
        let mut marker = File::from(marker);
        let metadata = marker.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || store.journal_id != Some((metadata.dev(), metadata.ino()))
        {
            return Err(StoreError::InvalidJournal);
        }
        let contents = read_bounded_marker(&mut marker)?;
        if contents.len() > JOURNAL_LIMIT || contents != store.journal_contents().as_bytes() {
            return Err(StoreError::InvalidJournal);
        }
        if super::require_entries(&store.root, &["owner.journal"]).is_err() {
            return Err(StoreError::InvalidJournal);
        }
        validate_named_marker_identity(&store.root, (metadata.dev(), metadata.ino()))?;
        let descriptor = store.root.try_clone()?;
        Ok(descriptor)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = store;
        Err(StoreError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn validate_stdin() -> Result<(), StoreError> {
    let stdin = std::io::stdin();
    validate_root_descriptor(&stdin)
}

/// Run the single fixed payload operation against the validated stdin
/// directory capability. The child's path is never reconstructed.
pub(super) fn write_stdin_payload() -> Result<super::payload::PayloadReport, StoreError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::fd::AsFd;
        let stdin = std::io::stdin();
        let descriptor = rustix::io::dup(stdin.as_fd())?;
        let root = File::from(descriptor);
        write_payload_from_root(&root)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(StoreError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn write_payload_from_root(
    root: &File,
) -> Result<super::payload::PayloadReport, StoreError> {
    validate_root_descriptor(root)?;
    let binding = capture_bound_journal(root)?;
    super::payload::write_worker_payload(root, binding.reservation, |has_payload, payload_id| {
        verify_bound_root(root, &binding, has_payload, payload_id)
    })
}

/// Fixed injected partial-write path for the supervised incomplete-store probe.
pub(super) fn write_stdin_payload_partial() -> Result<super::payload::PayloadReport, StoreError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::fd::AsFd;
        let stdin = std::io::stdin();
        let descriptor = rustix::io::dup(stdin.as_fd())?;
        let root = File::from(descriptor);
        write_partial_payload_from_root(&root)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(StoreError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_partial_payload_from_root(
    root: &File,
) -> Result<super::payload::PayloadReport, StoreError> {
    validate_root_descriptor(root)?;
    let binding = capture_bound_journal(root)?;
    super::payload::write_worker_payload_partial(
        root,
        binding.reservation,
        |has_payload, payload_id| verify_bound_root(root, &binding, has_payload, payload_id),
    )
}

/// Inspect the fixed payload only after this exact worker has been stopped.
pub(super) fn inspect_stopped_worker_payload(
    store: &OwnedStore,
    receipt: &crate::StopReceipt,
) -> Result<super::WorkerPayloadObservation, StoreError> {
    inspect_stopped_payload(store, receipt, super::payload::LIMIT)
}

pub(super) fn inspect_stopped_worker_partial_payload(
    store: &OwnedStore,
    receipt: &crate::StopReceipt,
) -> Result<super::WorkerPayloadObservation, StoreError> {
    inspect_stopped_payload(store, receipt, super::payload::PARTIAL_FIXTURE_BYTES)
}

fn inspect_stopped_payload(
    store: &OwnedStore,
    receipt: &crate::StopReceipt,
    expected_bytes: u64,
) -> Result<super::WorkerPayloadObservation, StoreError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt;
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
        let deadline = Instant::now() + Duration::from_secs(1);
        store.inspect_state()?;
        verify_owned_stopped_root(store, expected_bytes)?;
        check_readback_deadline(deadline)?;
        let fd = open_payload(&store.root)?;
        let before = fd.metadata()?;
        super::payload::validate_payload_metadata(&before, None, expected_bytes)?;
        let payload_id = (before.dev(), before.ino());
        let high_water = super::payload::payload_high_water_from_metadata(&before)?;
        let before_stamp = PayloadStamp::from(&before);
        verify_owned_stopped_root(store, expected_bytes)?;
        read_and_verify_payload(&fd, deadline, expected_bytes)?;
        let after = fd.metadata()?;
        super::payload::validate_payload_metadata(&after, Some(payload_id), expected_bytes)?;
        if PayloadStamp::from(&after) != before_stamp {
            return Err(StoreError::InvalidIdentity);
        }
        let named = open_payload(&store.root)?;
        let named_metadata = named.metadata()?;
        super::payload::validate_payload_metadata(
            &named_metadata,
            Some(payload_id),
            expected_bytes,
        )?;
        if PayloadStamp::from(&named_metadata) != before_stamp {
            return Err(StoreError::InvalidIdentity);
        }
        verify_owned_stopped_root(store, expected_bytes)?;
        store.inspect_state()?;
        if PayloadStamp::from(&fd.metadata()?) != before_stamp {
            return Err(StoreError::InvalidIdentity);
        }
        check_readback_deadline(deadline)?;
        Ok(super::WorkerPayloadObservation {
            written_bytes: expected_bytes,
            high_water_bytes: high_water,
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (store, receipt, expected_bytes);
        Err(StoreError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct BoundJournal {
    root_id: (u64, u64),
    journal_id: (u64, u64),
    bytes: Vec<u8>,
    reservation: u64,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn capture_bound_journal(root: &File) -> Result<BoundJournal, StoreError> {
    let root_id = validate_worker_root(root)?;
    super::require_entries(root, &["owner.journal"])?;
    let (journal_id, bytes) = read_named_journal(root)?;
    let reservation = match parse_journal_prefix(&bytes, root_id, std::process::id())? {
        JournalState::BoundWithReservation(value) if value == super::WORKER_PAYLOAD_RESERVATION => {
            value
        }
        _ => return Err(StoreError::InvalidJournal),
    };
    super::require_entries(root, &["owner.journal"])?;
    Ok(BoundJournal {
        root_id,
        journal_id,
        bytes,
        reservation,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn verify_bound_root(
    root: &File,
    binding: &BoundJournal,
    has_payload: bool,
    payload_id: Option<(u64, u64)>,
) -> Result<(), StoreError> {
    if validate_worker_root(root)? != binding.root_id {
        return Err(StoreError::InvalidIdentity);
    }
    let expected = if has_payload {
        &["owner.journal", super::payload::NAME][..]
    } else {
        &["owner.journal"][..]
    };
    super::require_entries(root, expected)?;
    let (journal_id, bytes) = read_named_journal(root)?;
    if journal_id != binding.journal_id || bytes != binding.bytes {
        return Err(StoreError::InvalidJournal);
    }
    if parse_journal_prefix(&bytes, binding.root_id, std::process::id())?
        != JournalState::BoundWithReservation(binding.reservation)
    {
        return Err(StoreError::InvalidJournal);
    }
    match (has_payload, payload_id) {
        (false, None) => Ok(()),
        (true, Some(expected_id)) => {
            let payload = open_payload(root)?;
            let metadata = payload.metadata()?;
            super::payload::validate_payload_metadata(
                &metadata,
                Some(expected_id),
                metadata.len(),
            )?;
            if super::identity(&payload)? != expected_id {
                return Err(StoreError::InvalidIdentity);
            }
            Ok(())
        }
        _ => Err(StoreError::InvalidIdentity),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_worker_root(root: &File) -> Result<(u64, u64), StoreError> {
    super::validate_private_directory(root)?;
    let stat = rustix::fs::fstat(root)?;
    if !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_uid != rustix::process::geteuid().as_raw()
    {
        return Err(StoreError::InvalidIdentity);
    }
    super::identity(root)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_named_journal(root: &File) -> Result<((u64, u64), Vec<u8>), StoreError> {
    use rustix::fs::{openat, OFlags};
    let descriptor = openat(
        root,
        "owner.journal",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    let mut journal = File::from(descriptor);
    let before = journal.metadata()?;
    validate_journal_metadata(&before)?;
    let journal_id = (before.dev(), before.ino());
    let bytes = read_bounded_marker(&mut journal)?;
    let after = journal.metadata()?;
    validate_journal_metadata(&after)?;
    if (after.dev(), after.ino()) != journal_id || bytes.len() > JOURNAL_LIMIT {
        return Err(StoreError::InvalidJournal);
    }
    let named_bytes = read_named_marker_contents(root, journal_id)?;
    if named_bytes != bytes {
        return Err(StoreError::InvalidJournal);
    }
    Ok((journal_id, bytes))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_named_marker_contents(root: &File, expected_id: (u64, u64)) -> Result<Vec<u8>, StoreError> {
    use rustix::fs::{openat, OFlags};
    let descriptor = openat(
        root,
        "owner.journal",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    let mut named = File::from(descriptor);
    let before = named.metadata()?;
    validate_journal_metadata(&before)?;
    if (before.dev(), before.ino()) != expected_id {
        return Err(StoreError::InvalidJournal);
    }
    let bytes = read_bounded_marker(&mut named)?;
    let after = named.metadata()?;
    validate_journal_metadata(&after)?;
    if (after.dev(), after.ino()) != expected_id
        || bytes.len() > JOURNAL_LIMIT
        || super::identity(&named)? != expected_id
    {
        return Err(StoreError::InvalidJournal);
    }
    Ok(bytes)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_payload(root: &File) -> Result<File, StoreError> {
    use rustix::fs::{openat, OFlags};
    Ok(File::from(openat(
        root,
        super::payload::NAME,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn verify_owned_stopped_root(store: &OwnedStore, expected_bytes: u64) -> Result<(), StoreError> {
    use std::os::unix::fs::MetadataExt;
    if validate_worker_root(&store.root)? != store.root_id {
        return Err(StoreError::InvalidIdentity);
    }
    super::require_entries(&store.root, &["owner.journal", super::payload::NAME])?;
    let (journal_id, bytes) = read_named_journal(&store.root)?;
    if Some(journal_id) != store.journal_id || bytes != store.journal_contents().as_bytes() {
        return Err(StoreError::InvalidJournal);
    }
    let metadata = open_payload(&store.root)?.metadata()?;
    super::payload::validate_payload_metadata(&metadata, None, expected_bytes)?;
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(StoreError::InvalidIdentity);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PayloadStamp {
    id: (u64, u64),
    len: u64,
    mode: u32,
    links: u64,
    uid: u32,
    blocks: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl From<&std::fs::Metadata> for PayloadStamp {
    fn from(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            id: (metadata.dev(), metadata.ino()),
            len: metadata.len(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            uid: metadata.uid(),
            blocks: metadata.blocks(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn check_readback_deadline(deadline: Instant) -> Result<(), StoreError> {
    if Instant::now() >= deadline {
        return Err(StoreError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "worker payload readback deadline",
        )));
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn read_and_verify_payload(
    file: &File,
    deadline: Instant,
    expected_bytes: u64,
) -> Result<(), StoreError> {
    use std::os::unix::fs::FileExt;
    let mut buffer = [0u8; 4096];
    let mut offset = 0u64;
    let mut interruptions = 0;
    while offset < expected_bytes {
        let want = (expected_bytes - offset).min(buffer.len() as u64) as usize;
        let mut got = 0usize;
        while got < want {
            check_readback_deadline(deadline)?;
            let read = file.read_at(&mut buffer[got..want], offset + got as u64);
            check_readback_deadline(deadline)?;
            match read {
                Ok(0) => return Err(StoreError::InvalidIdentity),
                Ok(count) => {
                    got += count;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    interruptions += 1;
                    if interruptions > 16 {
                        return Err(StoreError::Io(error));
                    }
                }
                Err(error) => return Err(StoreError::Io(error)),
            }
        }
        for (index, byte) in buffer[..want].iter().enumerate() {
            let absolute = offset + index as u64;
            let expected = (((absolute as usize).wrapping_mul(31) + 17) % 251 + 1) as u8;
            if *byte != expected {
                return Err(StoreError::InvalidIdentity);
            }
        }
        offset += want as u64;
    }
    let mut eof_probe = [0u8; 1];
    loop {
        check_readback_deadline(deadline)?;
        let read = file.read_at(&mut eof_probe, expected_bytes);
        check_readback_deadline(deadline)?;
        match read {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(StoreError::InvalidIdentity),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                interruptions += 1;
                if interruptions > 16 {
                    return Err(StoreError::Io(error));
                }
            }
            Err(error) => return Err(StoreError::Io(error)),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn validate_stdin() -> Result<(), StoreError> {
    Err(StoreError::UnsupportedPlatform)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_root_descriptor(root: &impl AsFd) -> Result<(), StoreError> {
    let stat = rustix::fs::fstat(root)?;
    if !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_mode & 0o777 != 0o700
        || stat.st_uid != rustix::process::geteuid().as_raw()
    {
        return Err(StoreError::InvalidIdentity);
    }
    let expected_root = super::identity(root)?;
    let deadline = Instant::now() + HANDOFF_WAIT;
    let mut stable_journal_id = None;
    let mut polls = 0usize;

    loop {
        if polls >= MAX_POLLS || Instant::now() >= deadline {
            return Err(StoreError::InvalidJournal);
        }
        polls += 1;
        let state = read_child_journal(root, expected_root, &mut stable_journal_id)?;
        if Instant::now() >= deadline {
            return Err(StoreError::InvalidJournal);
        }
        if matches!(
            state,
            JournalState::Bound | JournalState::BoundWithReservation(_)
        ) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(StoreError::InvalidJournal);
        }
        thread::sleep(remaining.min(POLL_INTERVAL));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JournalState {
    Pending,
    Bound,
    BoundWithReservation(u64),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_child_journal(
    root: &impl AsFd,
    expected_root: (u64, u64),
    stable_journal_id: &mut Option<(u64, u64)>,
) -> Result<JournalState, StoreError> {
    use rustix::fs::{openat, OFlags};

    super::validate_private_directory(root)?;
    if super::identity(root)? != expected_root {
        return Err(StoreError::InvalidIdentity);
    }
    super::require_entries(root, &["owner.journal"])?;
    let marker_fd = openat(
        root,
        "owner.journal",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?;
    let mut marker = File::from(marker_fd);
    let before = marker.metadata()?;
    let journal_id = (before.dev(), before.ino());
    validate_journal_metadata(&before)?;
    if stable_journal_id.is_some_and(|expected| expected != journal_id) {
        return Err(StoreError::InvalidJournal);
    }

    let bytes = read_bounded_marker(&mut marker)?;
    let after = marker.metadata()?;
    validate_journal_metadata(&after)?;
    if (after.dev(), after.ino()) != journal_id || bytes.len() > JOURNAL_LIMIT {
        return Err(StoreError::InvalidJournal);
    }
    super::require_entries(root, &["owner.journal"])?;
    validate_named_marker_identity(root, journal_id)?;
    *stable_journal_id = Some(journal_id);

    let current_pid = std::process::id();
    parse_journal_prefix(&bytes, expected_root, current_pid)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_named_marker_identity(
    root: &impl AsFd,
    expected_id: (u64, u64),
) -> Result<(), StoreError> {
    use rustix::fs::{openat, OFlags};

    let named = openat(
        root,
        "owner.journal",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?;
    let named = File::from(named);
    let metadata = named.metadata()?;
    validate_journal_metadata(&metadata)?;
    if (metadata.dev(), metadata.ino()) != expected_id {
        return Err(StoreError::InvalidJournal);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn read_bounded_marker(marker: &mut File) -> Result<Vec<u8>, StoreError> {
    let mut contents = Vec::with_capacity(JOURNAL_LIMIT + 1);
    let mut buffer = [0u8; JOURNAL_LIMIT + 1];
    let mut interruptions = 0u8;
    while contents.len() < buffer.len() {
        let remaining = buffer.len() - contents.len();
        match marker.read(&mut buffer[..remaining]) {
            Ok(0) => break,
            Ok(count) => {
                contents.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                interruptions += 1;
                if interruptions > 16 {
                    return Err(StoreError::Io(error));
                }
            }
            Err(error) => return Err(StoreError::Io(error)),
        }
    }
    Ok(contents)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_journal_metadata(metadata: &std::fs::Metadata) -> Result<(), StoreError> {
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(StoreError::InvalidJournal);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn parse_journal_prefix(
    bytes: &[u8],
    root_id: (u64, u64),
    current_pid: u32,
) -> Result<JournalState, StoreError> {
    const MAGIC: &[u8] = b"disk-steward-worker-store-v1\n";
    if bytes.len() <= MAGIC.len() && MAGIC.starts_with(bytes) {
        return Ok(JournalState::Pending);
    }
    if !bytes.is_ascii() || !bytes.starts_with(MAGIC) {
        return Err(StoreError::InvalidJournal);
    }
    let rest = &bytes[MAGIC.len()..];

    let token_end = match rest.iter().position(|byte| *byte == b'\n') {
        Some(end) => end,
        None => {
            return if valid_token_prefix(rest) {
                Ok(JournalState::Pending)
            } else {
                Err(StoreError::InvalidJournal)
            }
        }
    };
    let token = std::str::from_utf8(&rest[..token_end]).map_err(|_| StoreError::InvalidJournal)?;
    if !super::valid_token(token) {
        return Err(StoreError::InvalidJournal);
    }
    let mut rest = &rest[token_end + 1..];

    let parent_end = match rest.iter().position(|byte| *byte == b'\n') {
        Some(end) => end,
        None => {
            return if valid_pair_prefix(rest) {
                Ok(JournalState::Pending)
            } else {
                Err(StoreError::InvalidJournal)
            }
        }
    };
    parse_pair(&rest[..parent_end])?;
    rest = &rest[parent_end + 1..];

    let expected_root_line = format!("{}:{}\n", root_id.0, root_id.1);
    let expected_root_bytes = expected_root_line.as_bytes();
    if rest.len() < expected_root_bytes.len() {
        return if expected_root_bytes.starts_with(rest) {
            Ok(JournalState::Pending)
        } else {
            Err(StoreError::InvalidJournal)
        };
    }
    if !rest.starts_with(expected_root_bytes) {
        return Err(StoreError::InvalidJournal);
    }
    rest = &rest[expected_root_bytes.len()..];

    let mut reservation = None;
    const RESERVATION_PREFIX: &[u8] = b"reservation:";
    if rest.starts_with(RESERVATION_PREFIX)
        || (rest.len() <= RESERVATION_PREFIX.len() && RESERVATION_PREFIX.starts_with(rest))
    {
        if rest.len() <= RESERVATION_PREFIX.len() {
            return Ok(JournalState::Pending);
        }
        let value_line = &rest[RESERVATION_PREFIX.len()..];
        let Some(end) = value_line.iter().position(|byte| *byte == b'\n') else {
            return if valid_decimal_prefix(value_line) {
                Ok(JournalState::Pending)
            } else {
                Err(StoreError::InvalidJournal)
            };
        };
        let value = parse_decimal(&value_line[..end])?;
        if value == 0 {
            return Err(StoreError::InvalidJournal);
        }
        reservation = Some(value);
        rest = &value_line[end + 1..];
    }

    let expected_bound = format!("bound:{current_pid}\n");
    if rest == b"created\n" || b"created\n".starts_with(rest) {
        return Ok(JournalState::Pending);
    }
    if rest == expected_bound.as_bytes() {
        return Ok(match reservation {
            Some(value) => JournalState::BoundWithReservation(value),
            None => JournalState::Bound,
        });
    }
    if expected_bound.as_bytes().starts_with(rest) {
        return Ok(JournalState::Pending);
    }
    Err(StoreError::InvalidJournal)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn valid_token_prefix(bytes: &[u8]) -> bool {
    if bytes.len() > 128 {
        return false;
    }
    let mut components = 1;
    let mut component_len = 0;
    for byte in bytes {
        if byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase() {
            component_len += 1;
        } else if *byte == b'-' && components < 3 && component_len > 0 {
            components += 1;
            component_len = 0;
        } else {
            return false;
        }
    }
    true
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn valid_pair_prefix(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    let mut parts = bytes.split(|byte| *byte == b':');
    let first = parts.next().unwrap_or_default();
    let second = parts.next();
    if parts.next().is_some() || !valid_decimal_prefix(first) {
        return false;
    }
    match second {
        Some(bytes) => valid_decimal_prefix(bytes),
        None => true,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn valid_decimal_prefix(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_digit)
        && !(bytes.len() > 1 && bytes.first() == Some(&b'0'))
        && (bytes.is_empty()
            || std::str::from_utf8(bytes)
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .is_some())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn parse_pair(bytes: &[u8]) -> Result<(u64, u64), StoreError> {
    let mut parts = bytes.split(|byte| *byte == b':');
    let first = parse_decimal(parts.next().unwrap_or_default())?;
    let second = parse_decimal(parts.next().ok_or(StoreError::InvalidJournal)?)?;
    if parts.next().is_some() {
        return Err(StoreError::InvalidJournal);
    }
    Ok((first, second))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn parse_decimal(bytes: &[u8]) -> Result<u64, StoreError> {
    if bytes.is_empty()
        || !bytes.iter().all(u8::is_ascii_digit)
        || (bytes.len() > 1 && bytes.first() == Some(&b'0'))
    {
        return Err(StoreError::InvalidJournal);
    }
    std::str::from_utf8(bytes)
        .map_err(|_| StoreError::InvalidJournal)?
        .parse::<u64>()
        .map_err(|_| StoreError::InvalidJournal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn root_descriptor_is_issued_once_and_only_while_created() {
        let mut store = OwnedStore::create().unwrap();
        let _descriptor = store.worker_stdin_once().unwrap();
        assert!(matches!(
            store.worker_stdin_once(),
            Err(StoreError::InvalidJournal)
        ));

        let mut bound = OwnedStore::create().unwrap();
        bound.bind_worker(41).unwrap();
        assert!(matches!(
            bound.worker_stdin_once(),
            Err(StoreError::InvalidJournal)
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn root_descriptor_refuses_unexpected_entry_and_unsafe_journal_mode() {
        use std::os::unix::fs::PermissionsExt;

        let mut with_extra = OwnedStore::create().unwrap();
        std::fs::write(with_extra.parent_path.join("store/unexpected"), b"keep").unwrap();
        assert!(matches!(
            with_extra.worker_stdin_once(),
            Err(StoreError::InvalidJournal)
        ));

        let mut with_unsafe_marker = OwnedStore::create().unwrap();
        let marker = with_unsafe_marker.parent_path.join("store/owner.journal");
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(with_unsafe_marker.worker_stdin_once().is_err());
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(with_unsafe_marker.worker_stdin_once().is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn child_descriptor_validation_accepts_only_bound_own_store_identity() {
        use std::os::unix::fs::PermissionsExt;

        let pid = std::process::id();
        let mut valid = OwnedStore::create().unwrap();
        let descriptor = valid.worker_stdin_once().unwrap();
        valid.bind_worker(pid).unwrap();
        validate_root_descriptor(&descriptor).unwrap();

        let mut wrong_pid = OwnedStore::create().unwrap();
        let descriptor = wrong_pid.root.try_clone().unwrap();
        let foreign_pid = if pid == u32::MAX { pid - 1 } else { pid + 1 };
        wrong_pid.bind_worker(foreign_pid).unwrap();
        assert!(matches!(
            validate_root_descriptor(&descriptor),
            Err(StoreError::InvalidJournal)
        ));

        let wrong_root = OwnedStore::create().unwrap();
        let descriptor = wrong_root.root.try_clone().unwrap();
        let bad_root = (wrong_root.root_id.0.wrapping_add(1), wrong_root.root_id.1);
        std::fs::write(
            wrong_root.parent_path.join("store/owner.journal"),
            format!(
                "disk-steward-worker-store-v1\n{}\n{}:{}\n{}:{}\nbound:{pid}\n",
                wrong_root.token,
                wrong_root.parent_id.0,
                wrong_root.parent_id.1,
                bad_root.0,
                bad_root.1
            ),
        )
        .unwrap();
        assert!(matches!(
            validate_root_descriptor(&descriptor),
            Err(StoreError::InvalidJournal)
        ));

        let mut unsafe_marker = OwnedStore::create().unwrap();
        let descriptor = unsafe_marker.root.try_clone().unwrap();
        unsafe_marker.bind_worker(pid).unwrap();
        std::fs::set_permissions(
            unsafe_marker.parent_path.join("store/owner.journal"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(validate_root_descriptor(&descriptor).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn named_marker_replacement_during_handoff_is_refused() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let store = OwnedStore::create().unwrap();
        let expected_id = store.journal_id.unwrap();
        let root_path = store.parent_path.join("store");
        let original_path = root_path.join("owner.journal");
        let contents = std::fs::read(&original_path).unwrap();
        std::fs::rename(&original_path, root_path.join("saved.journal")).unwrap();
        let mut replacement = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&original_path)
            .unwrap();
        replacement.write_all(&contents).unwrap();
        replacement.sync_all().unwrap();

        assert!(matches!(
            validate_named_marker_identity(&store.root, expected_id),
            Err(StoreError::InvalidJournal)
        ));
        assert!(root_path.join("saved.journal").exists());
        assert!(original_path.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn complete_binding_is_required_for_every_valid_journal_prefix() {
        let journal = b"disk-steward-worker-store-v1\nabc-def-123\n2:3\n4:7\nbound:42\n";
        for end in 0..journal.len() {
            assert_eq!(
                parse_journal_prefix(&journal[..end], (4, 7), 42).unwrap(),
                JournalState::Pending,
                "prefix length {end} must not prove binding"
            );
        }
        assert_eq!(
            parse_journal_prefix(journal, (4, 7), 42).unwrap(),
            JournalState::Bound
        );
        assert!(parse_journal_prefix(journal, (4, 7), 43).is_err());
        assert!(parse_journal_prefix(journal, (4, 8), 42).is_err());
        let mut extra = journal.to_vec();
        extra.push(b'x');
        assert!(parse_journal_prefix(&extra, (4, 7), 42).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn reserved_binding_prefixes_wait_and_exact_value_is_required_by_payload() {
        let journal =
            b"disk-steward-worker-store-v1\nabc-def-123\n2:3\n4:7\nreservation:131072\nbound:42\n";
        for end in 0..journal.len() {
            assert_eq!(
                parse_journal_prefix(&journal[..end], (4, 7), 42).unwrap(),
                JournalState::Pending,
                "reserved journal prefix {end} must not prove binding"
            );
        }
        assert_eq!(
            parse_journal_prefix(journal, (4, 7), 42).unwrap(),
            JournalState::BoundWithReservation(super::super::WORKER_PAYLOAD_RESERVATION)
        );
        assert_eq!(
            parse_journal_prefix(
                b"disk-steward-worker-store-v1\nabc-def-123\n2:3\n4:7\nreservation:131071\nbound:42\n",
                (4, 7),
                42
            )
            .unwrap(),
            JournalState::BoundWithReservation(131071)
        );
        assert!(parse_journal_prefix(
            b"disk-steward-worker-store-v1\nabc-def-123\n2:3\n4:7\nreservation:0131072\nbound:42\n",
            (4, 7),
            42
        )
        .is_err());
        let extra_blank = [&journal[..], &b"\n"[..]].concat();
        assert!(parse_journal_prefix(&extra_blank, (4, 7), 42).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn created_descriptor_times_out_without_readiness_or_journal_mutation() {
        let mut store = OwnedStore::create().unwrap();
        let descriptor = store.worker_stdin_once().unwrap();
        let marker = store.parent_path.join("store/owner.journal");
        let before = std::fs::read(&marker).unwrap();
        assert_eq!(
            parse_journal_prefix(&before, store.root_id, std::process::id()).unwrap(),
            JournalState::Pending
        );
        assert!(matches!(
            validate_root_descriptor(&descriptor),
            Err(StoreError::InvalidJournal)
        ));
        assert_eq!(std::fs::read(&marker).unwrap(), before);
        assert_eq!(store.phase, Phase::Created);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn fixed_payload_writer_refuses_wrong_binding_before_creation() {
        let pid = std::process::id();
        let foreign_pid = if pid == u32::MAX { pid - 1 } else { pid + 1 };
        let mut wrong_pid = OwnedStore::create().unwrap();
        wrong_pid.bind_worker(foreign_pid).unwrap();
        assert!(write_payload_from_root(&wrong_pid.root).is_err());
        super::super::require_entries(&wrong_pid.root, &["owner.journal"]).unwrap();

        let wrong_root = OwnedStore::create().unwrap();
        let foreign_root = (wrong_root.root_id.0.wrapping_add(1), wrong_root.root_id.1);
        std::fs::write(
            wrong_root.parent_path.join("store/owner.journal"),
            format!(
                "disk-steward-worker-store-v1\n{}\n{}:{}\n{}:{}\nbound:{pid}\n",
                wrong_root.token,
                wrong_root.parent_id.0,
                wrong_root.parent_id.1,
                foreign_root.0,
                foreign_root.1
            ),
        )
        .unwrap();
        assert!(write_payload_from_root(&wrong_root.root).is_err());
        super::super::require_entries(&wrong_root.root, &["owner.journal"]).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn fixed_payload_writer_refuses_missing_or_mismatched_reservation_before_creation() {
        let pid = std::process::id();
        let missing = OwnedStore::create().unwrap();
        let descriptor = missing.root.try_clone().unwrap();
        let mut missing = missing;
        missing.bind_worker(pid).unwrap();
        assert!(write_payload_from_root(&descriptor).is_err());
        super::super::require_entries(&descriptor, &["owner.journal"]).unwrap();

        let mut mismatched = OwnedStore::create().unwrap();
        let descriptor = mismatched.root.try_clone().unwrap();
        mismatched.bind_worker_payload_reservation().unwrap();
        mismatched.bind_worker(pid).unwrap();
        std::fs::write(
            mismatched.parent_path.join("store/owner.journal"),
            format!(
                "disk-steward-worker-store-v1\n{}\n{}:{}\n{}:{}\nreservation:131071\nbound:{pid}\n",
                mismatched.token,
                mismatched.parent_id.0,
                mismatched.parent_id.1,
                mismatched.root_id.0,
                mismatched.root_id.1
            ),
        )
        .unwrap();
        assert!(write_payload_from_root(&descriptor).is_err());
        super::super::require_entries(&descriptor, &["owner.journal"]).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn fixed_payload_writer_refuses_a_second_attempt_without_changing_file() {
        let mut store = OwnedStore::create().unwrap();
        store.bind_worker_payload_reservation().unwrap();
        let descriptor = store.worker_stdin_once().unwrap();
        store.bind_worker(std::process::id()).unwrap();
        let first = write_payload_from_root(&descriptor).unwrap();
        assert_eq!(first.written_bytes, super::super::payload::LIMIT);
        let path = store
            .parent_path
            .join("store")
            .join(super::super::payload::NAME);
        let before = std::fs::read(&path).unwrap();
        assert!(write_payload_from_root(&descriptor).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn stopped_payload_inspection_requires_receipt_and_refuses_changed_file() {
        let mut store = OwnedStore::create().unwrap();
        store.bind_worker_payload_reservation().unwrap();
        let descriptor = store.worker_stdin_once().unwrap();
        let pid = std::process::id();
        store.bind_worker(pid).unwrap();
        write_payload_from_root(&descriptor).unwrap();
        let receipt = crate::StopReceipt::confirmed(store.token(), pid);
        assert!(store.inspect_stopped_worker_payload(&receipt).is_err());
        store.record_stopped(&receipt).unwrap();
        let file = open_payload(&store.root).unwrap();
        assert!(matches!(
            read_and_verify_payload(&file, Instant::now(), super::super::payload::LIMIT),
            Err(StoreError::Io(error)) if error.kind() == io::ErrorKind::TimedOut
        ));
        let wrong_token = crate::StopReceipt::confirmed("other-token", pid);
        assert!(store.inspect_stopped_worker_payload(&wrong_token).is_err());
        let observation = store.inspect_stopped_worker_payload(&receipt).unwrap();
        assert_eq!(observation.written_bytes, super::super::payload::LIMIT);
        assert!(observation.high_water_bytes >= observation.written_bytes);

        let wrong_pid = if pid == u32::MAX { pid - 1 } else { pid + 1 };
        let wrong_receipt = crate::StopReceipt::confirmed(store.token(), wrong_pid);
        assert!(store
            .inspect_stopped_worker_payload(&wrong_receipt)
            .is_err());

        let path = store
            .parent_path
            .join("store")
            .join(super::super::payload::NAME);
        let mut corrupt = std::fs::read(&path).unwrap();
        corrupt[100] ^= 0x5a;
        std::fs::write(&path, &corrupt).unwrap();
        assert!(store.inspect_stopped_worker_payload(&receipt).is_err());
        std::fs::write(&path, b"short").unwrap();
        assert!(store.inspect_stopped_worker_payload(&receipt).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn stopped_partial_payload_has_a_separate_exact_inspection_gate() {
        let mut store = OwnedStore::create().unwrap();
        store.bind_worker_payload_reservation().unwrap();
        let descriptor = store.worker_stdin_once().unwrap();
        let pid = std::process::id();
        store.bind_worker(pid).unwrap();
        assert!(write_partial_payload_from_root(&descriptor).is_err());
        let path = store
            .parent_path
            .join("store")
            .join(super::super::payload::NAME);
        let before_repeat = std::fs::read(&path).unwrap();
        assert!(write_partial_payload_from_root(&descriptor).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before_repeat);
        let receipt = crate::StopReceipt::confirmed(store.token(), pid);
        assert!(store
            .inspect_stopped_worker_partial_payload(&receipt)
            .is_err());
        store.record_stopped(&receipt).unwrap();
        let wrong_receipt = crate::StopReceipt::confirmed("other-token", pid);
        assert!(store
            .inspect_stopped_worker_partial_payload(&wrong_receipt)
            .is_err());
        let wrong_pid = if pid == u32::MAX { pid - 1 } else { pid + 1 };
        let wrong_pid_receipt = crate::StopReceipt::confirmed(store.token(), wrong_pid);
        assert!(store
            .inspect_stopped_worker_partial_payload(&wrong_pid_receipt)
            .is_err());
        let partial = store
            .inspect_stopped_worker_partial_payload(&receipt)
            .unwrap();
        assert_eq!(
            partial.written_bytes,
            super::super::payload::PARTIAL_FIXTURE_BYTES
        );
        assert!(partial.high_water_bytes >= partial.written_bytes);
        assert!(store.inspect_stopped_worker_payload(&receipt).is_err());

        let mut changed = std::fs::read(&path).unwrap();
        changed[31] ^= 0x5a;
        std::fs::write(&path, &changed).unwrap();
        assert!(store
            .inspect_stopped_worker_partial_payload(&receipt)
            .is_err());
        std::fs::write(&path, b"short").unwrap();
        assert!(store
            .inspect_stopped_worker_partial_payload(&receipt)
            .is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn stopped_inspection_refuses_unreserved_store_even_with_payload_entry() {
        use std::os::unix::fs::OpenOptionsExt;
        let mut store = OwnedStore::create().unwrap();
        let _descriptor = store.worker_stdin_once().unwrap();
        let pid = std::process::id();
        store.bind_worker(pid).unwrap();
        let path = store
            .parent_path
            .join("store")
            .join(super::super::payload::NAME);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap()
            .set_len(super::super::payload::LIMIT)
            .unwrap();
        let receipt = crate::StopReceipt::confirmed(store.token(), pid);
        store.record_stopped(&receipt).unwrap();
        assert!(store.inspect_stopped_worker_payload(&receipt).is_err());
        assert!(store
            .inspect_stopped_worker_partial_payload(&receipt)
            .is_err());
    }
}
