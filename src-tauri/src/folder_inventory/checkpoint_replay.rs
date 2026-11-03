//! Rebuilds a page cursor from a private, explicit checkpoint without storing a source path.

#[cfg(unix)]
use super::checkpoint_store::{Payload, RootStamp};
#[cfg(unix)]
use super::{
    identity, root_metadata_matches, scan_directory_fd, selected_root_session,
    validate_session_bindings, DirectoryCursor, ScanRequest, Session, MAX_CHECKPOINT_NAMES,
    MAX_CHECKPOINT_NAME_BYTES, MAX_DURATION, MAX_PAGES_PER_DIRECTORY, MAX_RAW_ENTRIES,
};
#[cfg(unix)]
use disk_steward_core::{InventoryItemId, RevisionId};
#[cfg(unix)]
use rustix::fs::{fstat, Dir, FileType};
#[cfg(unix)]
use std::collections::BTreeSet;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
const CHECKPOINT_VERSION: u32 = 1;
#[cfg(unix)]
const MAX_CHECKPOINT_BYTES: usize = 16 * 1024 * 1024;

#[cfg(unix)]
pub(super) fn capture(
    session: &Session,
    exclusion_revision: RevisionId,
) -> Result<Payload, &'static str> {
    let started = Instant::now();
    validate_session_bindings(session, started, MAX_DURATION)?;
    if session.snapshot.observation_id != session.observation_id {
        return Err("Folder checkpoint is stale. Refresh the current folder.");
    }
    let cursor = session
        .cursor
        .as_ref()
        .ok_or("This folder page has no resumable continuation.")?;
    let frame = session
        .frames
        .last()
        .ok_or("Folder observation is unavailable. Choose a folder again.")?;
    let entries_seen = cursor.entries_seen;
    if !session.snapshot.can_continue
        || session.snapshot.page_number != cursor.page_number
        || session.snapshot.entries_seen != entries_seen.to_string()
        || cursor.identity != frame.identity
        || !(1..MAX_PAGES_PER_DIRECTORY).contains(&cursor.page_number)
        || entries_seen == 0
        || entries_seen > MAX_CHECKPOINT_NAMES as u64
        || cursor.replay_names.len() != entries_seen as usize
        || cursor.replay_names.len() > MAX_CHECKPOINT_NAMES
    {
        return Err("Folder paging state cannot be checkpointed safely.");
    }
    validate_replay_names(&cursor.replay_names)?;
    let before = fstat(&frame.fd).map_err(|_| "Folder observation changed. Inspect it again.")?;
    if started.elapsed() >= MAX_DURATION
        || identity(&before) != frame.identity
        || !root_metadata_matches(&cursor.baseline, &before)
    {
        return Err("Folder observation changed. Inspect it again.");
    }
    let root_stamp = stamp(&before)?;
    validate_session_bindings(session, started, MAX_DURATION)?;
    let after = fstat(&frame.fd).map_err(|_| "Folder observation changed. Inspect it again.")?;
    if started.elapsed() >= MAX_DURATION
        || !root_metadata_matches(&before, &after)
        || stamp(&after)? != root_stamp
    {
        return Err("Folder observation changed. Inspect it again.");
    }
    Ok(Payload {
        version: CHECKPOINT_VERSION,
        checkpoint_id: InventoryItemId::new(),
        root_label: frame.label.clone(),
        page_number: cursor.page_number,
        entries_seen,
        root_stamp,
        exclusion_revision,
        replay_names: cursor.replay_names.clone(),
    })
}

#[cfg(unix)]
pub(super) fn resume(
    path: &Path,
    payload: &Payload,
    duration: Duration,
) -> Result<(Session, super::FolderInventorySnapshot), &'static str> {
    let started = Instant::now();
    if payload.version != CHECKPOINT_VERSION
        || payload.page_number == 0
        || payload.page_number >= MAX_PAGES_PER_DIRECTORY
        || payload.entries_seen == 0
        || payload.entries_seen > MAX_CHECKPOINT_NAMES as u64
        || payload.replay_names.len() != payload.entries_seen as usize
        || payload.entries_seen > u64::from(payload.page_number) * MAX_RAW_ENTRIES as u64
    {
        return Err("Folder checkpoint is invalid or cannot be resumed.");
    }
    validate_replay_names(&payload.replay_names)?;
    let duration = duration.min(MAX_DURATION);
    let (mut session, _) = selected_root_session(
        path,
        MAX_RAW_ENTRIES,
        duration.saturating_sub(started.elapsed()),
    )?;
    check_deadline(started, duration)?;
    let frame = session
        .frames
        .last()
        .ok_or("Folder observation is unavailable. Choose a folder again.")?;
    if frame.label != payload.root_label {
        return Err("The selected folder does not match this checkpoint.");
    }
    let root_before =
        fstat(&frame.fd).map_err(|_| "The selected folder no longer matches this checkpoint.")?;
    if check_deadline(started, duration).is_err()
        || identity(&root_before) != frame.identity
        || FileType::from_raw_mode(root_before.st_mode) != FileType::Directory
        || stamp(&root_before)? != payload.root_stamp
    {
        return Err("The selected folder no longer matches this checkpoint.");
    }

    let mut directory = Dir::read_from(&frame.fd)
        .map_err(|_| "The selected folder could not be replayed safely.")?;
    check_deadline(started, duration)?;
    for expected_name in &payload.replay_names {
        loop {
            check_deadline(started, duration)?;
            let next = directory.read();
            check_deadline(started, duration)?;
            let entry = next
                .ok_or("The selected folder changed since this checkpoint was saved.")?
                .map_err(|_| "The selected folder could not be replayed safely.")?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if name != expected_name.as_slice() {
                return Err("The selected folder changed since this checkpoint was saved.");
            }
            break;
        }
    }

    check_deadline(started, duration)?;
    let root_after_replay =
        fstat(&frame.fd).map_err(|_| "The selected folder no longer matches this checkpoint.")?;
    if check_deadline(started, duration).is_err()
        || !root_metadata_matches(&root_before, &root_after_replay)
        || stamp(&root_after_replay)? != payload.root_stamp
    {
        return Err("The selected folder changed since this checkpoint was saved.");
    }

    let replay_cursor = DirectoryCursor {
        directory,
        baseline: root_after_replay,
        identity: frame.identity,
        page_number: payload.page_number,
        entries_seen: payload.entries_seen,
        replay_names: payload.replay_names.clone(),
    };
    let root_device = session.root_device;
    let root_label = frame.label.clone();
    let frame_identity = frame.identity;
    let scanned = scan_directory_fd(ScanRequest {
        fd: &frame.fd,
        expected_identity: frame_identity,
        root_device,
        root_label: &root_label,
        breadcrumbs: vec![root_label.clone()],
        can_go_back: false,
        depth: 1,
        max_entries: MAX_RAW_ENTRIES,
        duration: duration.saturating_sub(started.elapsed()),
        cursor: Some(replay_cursor),
        page_number: payload.page_number + 1,
        entries_seen: payload.entries_seen,
    })?;
    check_deadline(started, duration)?;
    validate_session_bindings(&session, started, duration)?;
    let root_final =
        fstat(&frame.fd).map_err(|_| "The selected folder changed during checkpoint replay.")?;
    if check_deadline(started, duration).is_err()
        || !root_metadata_matches(&root_after_replay, &root_final)
        || stamp(&root_final)? != payload.root_stamp
    {
        return Err("The selected folder changed during checkpoint replay.");
    }

    session.observation_id = scanned.snapshot.observation_id;
    session.entries = scanned.entries;
    session.snapshot = scanned.snapshot.clone();
    session.cursor = scanned.cursor;
    Ok((session, scanned.snapshot))
}

#[cfg(unix)]
fn check_deadline(started: Instant, duration: Duration) -> Result<(), &'static str> {
    if started.elapsed() >= duration {
        Err("Folder checkpoint replay exceeded its cooperative time limit.")
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn validate_replay_names(names: &[Vec<u8>]) -> Result<(), &'static str> {
    if names.is_empty() || names.len() > MAX_CHECKPOINT_NAMES {
        return Err("Folder checkpoint replay prefix is invalid.");
    }
    let mut seen = BTreeSet::new();
    let mut total_bytes = 0usize;
    for name in names {
        if name.is_empty()
            || name.len() > MAX_CHECKPOINT_NAME_BYTES
            || name.contains(&0)
            || name.contains(&b'/')
            || name.as_slice() == b"."
            || name.as_slice() == b".."
            || !seen.insert(name.as_slice())
        {
            return Err("Folder checkpoint replay prefix is invalid.");
        }
        total_bytes = total_bytes
            .checked_add(name.len())
            .ok_or("Folder checkpoint replay prefix is too large.")?;
        if total_bytes > MAX_CHECKPOINT_BYTES {
            return Err("Folder checkpoint replay prefix is too large.");
        }
    }
    Ok(())
}

#[cfg(unix)]
// Native stat field widths differ across Unix targets; checked conversions stay explicit.
#[allow(clippy::unnecessary_fallible_conversions, clippy::useless_conversion)]
fn stamp(stat: &rustix::fs::Stat) -> Result<RootStamp, &'static str> {
    let identity = identity(stat);
    #[cfg(target_os = "macos")]
    let (birth_seconds, birth_nanos) = (stat.st_birthtime, stat.st_birthtime_nsec);
    #[cfg(not(target_os = "macos"))]
    let (birth_seconds, birth_nanos) = (0, 0);
    Ok(RootStamp {
        device: identity.device,
        inode: identity.inode,
        mode: u32::try_from(stat.st_mode).map_err(|_| "Folder metadata is unavailable.")?,
        size: i64::try_from(stat.st_size).map_err(|_| "Folder metadata is unavailable.")?,
        nlink: u64::try_from(stat.st_nlink).map_err(|_| "Folder metadata is unavailable.")?,
        mtime: stat.st_mtime,
        mtime_nsec: i64::try_from(stat.st_mtime_nsec)
            .map_err(|_| "Folder metadata is unavailable.")?,
        ctime: stat.st_ctime,
        ctime_nsec: i64::try_from(stat.st_ctime_nsec)
            .map_err(|_| "Folder metadata is unavailable.")?,
        birth_seconds: i64::try_from(birth_seconds)
            .map_err(|_| "Folder metadata is unavailable.")?,
        birth_nanos: i64::try_from(birth_nanos).map_err(|_| "Folder metadata is unavailable.")?,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::folder_inventory::{continue_session, ContinueInventoryRequest};
    use tempfile::tempdir;

    fn continue_page(
        session: &mut Session,
        max_entries: usize,
    ) -> super::super::FolderInventorySnapshot {
        let expected_observation_id = session.observation_id;
        continue_session(
            session,
            ContinueInventoryRequest {
                expected_observation_id,
            },
            max_entries,
            MAX_DURATION,
        )
        .unwrap()
    }

    fn fixture_payload(
        path: &Path,
        page_size: usize,
    ) -> (Payload, super::super::FolderInventorySnapshot) {
        let (session, first) = selected_root_session(path, page_size, MAX_DURATION).unwrap();
        let payload = capture(&session, RevisionId::new()).unwrap();
        drop(session);
        (payload, first)
    }

    #[test]
    fn resume_replays_saved_prefix_then_continues_without_losing_or_repeating_names() {
        let root = tempdir().unwrap();
        for index in 0..520 {
            std::fs::write(root.path().join(format!("item-{index}")), b"x").unwrap();
        }
        let (payload, first) = fixture_payload(root.path(), 2);
        assert_eq!(payload.page_number, 1);
        let (mut resumed, second) = resume(root.path(), &payload, MAX_DURATION).unwrap();
        assert_eq!(second.page_number, 2);
        assert_eq!(second.entries.len(), MAX_RAW_ENTRIES);
        assert_ne!(second.observation_id, first.observation_id);
        let third = continue_page(&mut resumed, MAX_RAW_ENTRIES);
        let mut names = payload
            .replay_names
            .iter()
            .map(|name| super::super::safe_label_bytes(name))
            .chain(second.entries.iter().map(|entry| entry.name.clone()))
            .chain(third.entries.iter().map(|entry| entry.name.clone()))
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 520);
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 520);
        assert!(names.iter().all(|name| name.starts_with("item-")));
        assert!(third.enumeration_complete);
    }

    #[test]
    fn saved_later_page_resumes_after_its_entire_prefix() {
        let root = tempdir().unwrap();
        for index in 0..516 {
            std::fs::write(root.path().join(format!("item-{index}")), b"x").unwrap();
        }
        let (mut session, _) = selected_root_session(root.path(), 2, MAX_DURATION).unwrap();
        let second = continue_page(&mut session, 2);
        let payload = capture(&session, RevisionId::new()).unwrap();
        assert_eq!(payload.page_number, 2);
        assert_eq!(payload.replay_names.len(), 4);
        drop(session);

        let (mut resumed, third) = resume(root.path(), &payload, MAX_DURATION).unwrap();
        assert_eq!(third.page_number, 3);
        assert_eq!(third.entries.len(), MAX_RAW_ENTRIES);
        assert_ne!(third.observation_id, second.observation_id);
        let end = continue_page(&mut resumed, MAX_RAW_ENTRIES);
        assert!(end.entries.is_empty());
        assert!(end.enumeration_complete);
        let mut names = payload
            .replay_names
            .iter()
            .map(|name| super::super::safe_label_bytes(name))
            .chain(third.entries.iter().map(|entry| entry.name.clone()))
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 516);
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 516);
    }

    #[test]
    fn wrong_root_changed_membership_and_prefix_refuse_resume() {
        let root_parent = tempdir().unwrap();
        let root = root_parent.path().join("selected");
        std::fs::create_dir(&root).unwrap();
        for index in 0..3 {
            std::fs::write(root.join(format!("item-{index}")), b"x").unwrap();
        }
        let (payload, _) = fixture_payload(&root, 1);

        let other_parent = tempdir().unwrap();
        let other = other_parent.path().join("selected");
        std::fs::create_dir(&other).unwrap();
        for index in 0..3 {
            std::fs::write(other.join(format!("item-{index}")), b"x").unwrap();
        }
        assert!(resume(&other, &payload, MAX_DURATION).is_err());

        std::fs::write(root.join("new-entry"), b"changed membership").unwrap();
        assert!(resume(&root, &payload, MAX_DURATION).is_err());

        let prefix_parent = tempdir().unwrap();
        let prefix_root = prefix_parent.path().join("selected");
        std::fs::create_dir(&prefix_root).unwrap();
        for index in 0..3 {
            std::fs::write(prefix_root.join(format!("item-{index}")), b"x").unwrap();
        }
        let (prefix_payload, _) = fixture_payload(&prefix_root, 1);
        let mut wrong_prefix = prefix_payload;
        wrong_prefix.replay_names[0] = b"not-the-first-entry".to_vec();
        assert!(resume(&prefix_root, &wrong_prefix, MAX_DURATION).is_err());
    }

    #[test]
    fn root_stamp_refuses_changed_current_directory_contents() {
        let parent = tempdir().unwrap();
        let root = parent.path().join("selected");
        std::fs::create_dir(&root).unwrap();
        for index in 0..3 {
            std::fs::write(root.join(format!("item-{index}")), b"x").unwrap();
        }
        let (payload, _) = fixture_payload(&root, 1);
        std::fs::write(root.join("new-entry"), b"changed directory membership").unwrap();
        assert!(resume(&root, &payload, MAX_DURATION).is_err());
    }

    #[test]
    fn capture_requires_an_active_page_cursor() {
        let root = tempdir().unwrap();
        std::fs::write(root.path().join("only"), b"x").unwrap();
        let (session, _) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        assert!(capture(&session, RevisionId::new()).is_err());
    }
}
