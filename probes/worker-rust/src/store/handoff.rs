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
        if state == JournalState::Bound {
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
fn read_bounded_marker(marker: &mut File) -> Result<Vec<u8>, StoreError> {
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

    let expected_bound = format!("bound:{current_pid}\n");
    if rest == b"created\n" || (rest.len() < b"created\n".len() && b"created\n".starts_with(rest)) {
        return Ok(JournalState::Pending);
    }
    let expected_bound = expected_bound.as_bytes();
    if rest == expected_bound {
        return Ok(JournalState::Bound);
    }
    if expected_bound.starts_with(rest) {
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
}
