use super::{
    identity, is_dataless, is_managed_boundary, root_metadata_matches, FolderTreeSummary, Session,
};
use crate::global_exclusions::SummaryExclusionPolicy;
use disk_steward_core::InventoryItemId;
use rustix::fs::{fstat, openat, statat, AtFlags, Dir, FileType, Mode, OFlags, Stat};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 16;
const SUMMARY_BOUNDARY_NAMES: [&[u8]; 7] = [
    b"Library", b"System", b"Volumes", b"Network", b"dev", b"private", b"proc",
];
const SUMMARY_BOUNDARY_NAMES_EXTRA: [&[u8]; 1] = [b"sys"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileVersion {
    logical_bytes: u64,
    ctime: i64,
    ctime_nsec: i64,
}

struct WalkState<'a> {
    observation_id: InventoryItemId,
    root_label: String,
    root_device: u64,
    started: Instant,
    duration: Duration,
    max_entries: usize,
    max_depth: usize,
    entries_observed: u32,
    files_observed: u32,
    duplicate_hardlink_names: u32,
    excluded_directories: u32,
    directories_visited: u32,
    skipped_entries: u32,
    unreadable_entries: u32,
    changed_directories: u32,
    unique_file_logical_bytes: u64,
    files: BTreeMap<(u64, u64), FileVersion>,
    coverage_complete: bool,
    stop_reason: Option<&'static str>,
    halted: bool,
    exclusion_policy: &'a SummaryExclusionPolicy,
}

impl WalkState<'_> {
    fn expired(&self) -> bool {
        self.started.elapsed() >= self.duration
    }

    fn set_reason(&mut self, reason: &'static str) {
        self.coverage_complete = false;
        if self.stop_reason.is_none() {
            self.stop_reason = Some(reason);
        }
    }

    fn time_check(&mut self) -> bool {
        if self.expired() {
            self.set_reason("time_limit");
            self.halted = true;
            true
        } else {
            false
        }
    }

    fn skip(&mut self) {
        self.skipped_entries = self.skipped_entries.saturating_add(1);
        self.coverage_complete = false;
    }

    fn unreadable(&mut self, reason: &'static str) {
        self.unreadable_entries = self.unreadable_entries.saturating_add(1);
        self.set_reason(reason);
    }

    fn changed_directory(&mut self) {
        self.changed_directories = self.changed_directories.saturating_add(1);
        self.set_reason("directory_changed");
        self.halted = true;
    }

    fn summary(self) -> FolderTreeSummary {
        FolderTreeSummary {
            observation_id: self.observation_id,
            root_label: self.root_label,
            entries_observed: self.entries_observed,
            files_observed: self.files_observed,
            unique_file_objects: self.files.len().min(u32::MAX as usize) as u32,
            duplicate_hardlink_names: self.duplicate_hardlink_names,
            excluded_directories: self.excluded_directories,
            directories_visited: self.directories_visited,
            skipped_entries: self.skipped_entries,
            unreadable_entries: self.unreadable_entries,
            changed_directories: self.changed_directories,
            unique_file_logical_bytes: self.unique_file_logical_bytes.to_string(),
            coverage_complete: self.coverage_complete,
            stop_reason: self.stop_reason,
        }
    }
}

pub(super) fn summarize(
    session: &Session,
    exclusion_policy: &SummaryExclusionPolicy,
    expected_observation_id: InventoryItemId,
    started: Instant,
    duration: Duration,
    max_entries: usize,
    max_depth: usize,
) -> Result<FolderTreeSummary, &'static str> {
    if session.observation_id != expected_observation_id
        || session.snapshot.observation_id != expected_observation_id
    {
        return Err("The folder observation is stale. Inspect the current folder again.");
    }
    let frame = session
        .frames
        .last()
        .ok_or("The current folder observation is unavailable.")?;
    let mut state = WalkState {
        observation_id: expected_observation_id,
        root_label: frame.label.clone(),
        root_device: session.root_device,
        started,
        duration: duration.min(Duration::from_secs(2)),
        max_entries: max_entries.min(MAX_ENTRIES),
        max_depth: max_depth.min(MAX_DEPTH),
        entries_observed: 0,
        files_observed: 0,
        duplicate_hardlink_names: 0,
        excluded_directories: 0,
        directories_visited: 0,
        skipped_entries: 0,
        unreadable_entries: 0,
        changed_directories: 0,
        unique_file_logical_bytes: 0,
        files: BTreeMap::new(),
        coverage_complete: true,
        stop_reason: None,
        halted: false,
        exclusion_policy,
    };
    if state.time_check() {
        return Ok(state.summary());
    }
    let root_stat = match fstat(&frame.fd) {
        Ok(stat) => stat,
        Err(_) => {
            state.unreadable("root_metadata_unavailable");
            return Ok(state.summary());
        }
    };
    if state.time_check() {
        return Ok(state.summary());
    }
    if identity(&root_stat) != frame.identity
        || frame.identity.device != session.root_device
        || FileType::from_raw_mode(root_stat.st_mode) != FileType::Directory
        || is_dataless(&root_stat)
    {
        state.changed_directory();
        return Ok(state.summary());
    }
    exclusion_policy.ensure_ancestor_scope_allowed(&frame.fd, started, state.duration)?;
    walk_directory(&frame.fd, root_stat, 1, &mut state);
    exclusion_policy.ensure_ancestor_scope_allowed(&frame.fd, started, state.duration)?;
    Ok(state.summary())
}

fn walk_directory(fd: &OwnedFd, baseline: Stat, depth: usize, state: &mut WalkState<'_>) {
    if state.halted || state.time_check() {
        return;
    }
    let held_before = fstat(fd);
    if state.time_check() {
        return;
    }
    let Ok(held_before) = held_before else {
        state.unreadable("directory_metadata_unavailable");
        state.halted = true;
        return;
    };
    if !root_metadata_matches(&baseline, &held_before)
        || identity(&held_before) != identity(&baseline)
        || FileType::from_raw_mode(held_before.st_mode) != FileType::Directory
        || is_dataless(&held_before)
    {
        state.changed_directory();
        return;
    }

    let directory = Dir::read_from(fd);
    if state.time_check() {
        return;
    }
    let mut directory = match directory {
        Ok(directory) => directory,
        Err(_) => {
            state.unreadable("directory_read_error");
            state.halted = true;
            return;
        }
    };
    state.directories_visited = state.directories_visited.saturating_add(1);
    loop {
        if state.halted || state.time_check() {
            break;
        }
        let next = directory.read();
        if state.time_check() {
            break;
        }
        let Some(next) = next else {
            break;
        };
        let entry = match next {
            Ok(entry) => entry,
            Err(_) => {
                state.unreadable("directory_read_error");
                state.halted = true;
                break;
            }
        };
        let raw_name = entry.file_name().to_bytes();
        if raw_name == b"." || raw_name == b".." {
            continue;
        }
        if state.entries_observed as usize >= state.max_entries {
            state.set_reason("entry_limit");
            state.halted = true;
            break;
        }
        state.entries_observed = state.entries_observed.saturating_add(1);
        if state.time_check() {
            break;
        }
        let metadata = statat(fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW);
        if state.time_check() {
            break;
        }
        let metadata = match metadata {
            Ok(metadata) => metadata,
            Err(_) => {
                state.unreadable("entry_metadata_unavailable");
                continue;
            }
        };
        let file_type = FileType::from_raw_mode(metadata.st_mode);
        if file_type == FileType::Directory && state.exclusion_policy.excludes(&metadata) {
            state.skip();
            state.excluded_directories = state.excluded_directories.saturating_add(1);
            continue;
        }
        if raw_name.first() == Some(&b'.') {
            state.skip();
            continue;
        }
        if is_dataless(&metadata) {
            state.skip();
            continue;
        }
        if file_type == FileType::Symlink {
            state.skip();
            continue;
        }
        if metadata.st_dev as u64 != state.root_device {
            state.skip();
            continue;
        }
        let name = raw_name;
        let is_directory = file_type == FileType::Directory;
        if is_directory && (is_managed_boundary(name) || is_summary_system_boundary(name)) {
            state.skip();
            continue;
        }
        if file_type == FileType::RegularFile {
            observe_file(&metadata, state);
            continue;
        }
        if !is_directory {
            state.skip();
            continue;
        }
        if depth >= state.max_depth {
            state.skip();
            state.set_reason("depth_limit");
            continue;
        }

        let child_name = match CString::new(name) {
            Ok(name) => name,
            Err(_) => {
                state.unreadable("entry_metadata_unavailable");
                continue;
            }
        };
        if state.time_check() {
            break;
        }
        let named_before = statat(fd, child_name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW);
        if state.time_check() {
            break;
        }
        let named_before = match named_before {
            Ok(stat) => stat,
            Err(_) => {
                state.unreadable("entry_metadata_unavailable");
                continue;
            }
        };
        if identity(&named_before) != identity(&metadata)
            || named_before.st_dev as u64 != state.root_device
            || FileType::from_raw_mode(named_before.st_mode) != FileType::Directory
            || is_dataless(&named_before)
        {
            state.changed_directory();
            break;
        }
        if state.time_check() {
            break;
        }
        let opened = openat(
            fd,
            child_name.as_c_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        );
        if state.time_check() {
            break;
        }
        let child_fd = match opened {
            Ok(fd) => fd,
            Err(_) => {
                state.unreadable("directory_open_failed");
                continue;
            }
        };
        let child_before = fstat(&child_fd);
        if state.time_check() {
            break;
        }
        let child_before = match child_before {
            Ok(stat) => stat,
            Err(_) => {
                state.unreadable("directory_metadata_unavailable");
                continue;
            }
        };
        if identity(&child_before) != identity(&named_before)
            || child_before.st_dev as u64 != state.root_device
            || FileType::from_raw_mode(child_before.st_mode) != FileType::Directory
            || is_dataless(&child_before)
        {
            state.changed_directory();
            break;
        }
        walk_directory(&child_fd, child_before, depth + 1, state);
        if state.time_check() {
            break;
        }
        let named_after = statat(fd, child_name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW);
        if state.time_check() {
            break;
        }
        let named_after = match named_after {
            Ok(stat) => stat,
            Err(_) => {
                state.changed_directory();
                break;
            }
        };
        let child_after = fstat(&child_fd);
        if state.time_check() {
            break;
        }
        let child_after = match child_after {
            Ok(stat) => stat,
            Err(_) => {
                state.changed_directory();
                break;
            }
        };
        if identity(&named_after) != identity(&child_before)
            || !root_metadata_matches(&child_before, &child_after)
            || is_dataless(&named_after)
            || is_dataless(&child_after)
        {
            state.changed_directory();
            break;
        }
    }

    if state.time_check() {
        return;
    }
    let held_after = fstat(fd);
    if state.time_check() {
        return;
    }
    match held_after {
        Ok(after) if root_metadata_matches(&baseline, &after) && !is_dataless(&after) => {}
        _ => state.changed_directory(),
    }
}

fn observe_file(metadata: &Stat, state: &mut WalkState<'_>) {
    state.files_observed = state.files_observed.saturating_add(1);
    let logical_bytes = match u64::try_from(metadata.st_size) {
        Ok(bytes) => bytes,
        Err(_) => {
            state.unreadable("file_size_unavailable");
            return;
        }
    };
    let key = (metadata.st_dev as u64, metadata.st_ino);
    let version = FileVersion {
        logical_bytes,
        ctime: metadata.st_ctime,
        ctime_nsec: metadata.st_ctime_nsec,
    };
    if let Some(existing) = state.files.get(&key) {
        if existing == &version {
            state.duplicate_hardlink_names = state.duplicate_hardlink_names.saturating_add(1);
        } else {
            state.unreadable_entries = state.unreadable_entries.saturating_add(1);
            state.set_reason("file_metadata_changed");
            state.halted = true;
        }
        return;
    }
    let Some(total) = state.unique_file_logical_bytes.checked_add(logical_bytes) else {
        state.set_reason("logical_size_overflow");
        state.halted = true;
        return;
    };
    state.unique_file_logical_bytes = total;
    state.files.insert(key, version);
}

fn is_summary_system_boundary(name: &[u8]) -> bool {
    SUMMARY_BOUNDARY_NAMES
        .iter()
        .chain(SUMMARY_BOUNDARY_NAMES_EXTRA.iter())
        .any(|boundary| name.eq_ignore_ascii_case(boundary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder_inventory::selected_root_session;
    use std::fs;
    use std::os::unix::fs as unix_fs;
    use tempfile::tempdir;

    fn summarize_fixture(
        path: &std::path::Path,
        entry_limit: usize,
        depth_limit: usize,
        duration: Duration,
    ) -> (FolderTreeSummary, Session) {
        let (session, _) = selected_root_session(path, 1, Duration::from_secs(2)).unwrap();
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let summary = summarize(
            &session,
            &policy,
            session.observation_id,
            Instant::now(),
            duration,
            entry_limit,
            depth_limit,
        )
        .unwrap();
        (summary, session)
    }

    #[test]
    fn tree_summary_deduplicates_hardlinks_and_never_consumes_explorer_cursor() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested/file"), b"hello").unwrap();
        fs::hard_link(root.path().join("nested/file"), root.path().join("alias")).unwrap();
        let (summary, session) =
            summarize_fixture(root.path(), MAX_ENTRIES, MAX_DEPTH, Duration::from_secs(2));
        assert_eq!(summary.entries_observed, 3);
        assert_eq!(summary.files_observed, 2);
        assert_eq!(summary.unique_file_objects, 1);
        assert_eq!(summary.duplicate_hardlink_names, 1);
        assert_eq!(summary.unique_file_logical_bytes, "5");
        assert_eq!(summary.directories_visited, 2);
        assert!(summary.coverage_complete);
        assert!(!serde_json::to_string(&summary)
            .unwrap()
            .contains(root.path().to_str().unwrap()));
        assert!(session.cursor.is_some());

        let paged_root = tempdir().unwrap();
        for index in 0..4 {
            fs::write(paged_root.path().join(format!("item-{index}")), b"x").unwrap();
        }
        let (mut paged_session, _) =
            selected_root_session(paged_root.path(), 1, Duration::from_secs(2)).unwrap();
        assert!(paged_session.cursor.is_some());
        let cursor_address = paged_session.cursor.as_ref().unwrap() as *const _;
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let _ = summarize(
            &paged_session,
            &policy,
            paged_session.observation_id,
            Instant::now(),
            Duration::from_secs(2),
            MAX_ENTRIES,
            MAX_DEPTH,
        )
        .unwrap();
        assert_eq!(
            paged_session.cursor.as_ref().unwrap() as *const _,
            cursor_address
        );
        assert!(summarize(
            &paged_session,
            &policy,
            InventoryItemId::new(),
            Instant::now(),
            Duration::from_secs(2),
            MAX_ENTRIES,
            MAX_DEPTH,
        )
        .is_err());
        assert_eq!(
            paged_session.cursor.as_ref().unwrap() as *const _,
            cursor_address
        );
        let mut observed_names = paged_session
            .snapshot
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>();
        while paged_session.snapshot.can_continue {
            let expected_observation_id = paged_session.observation_id;
            let page = super::super::continue_session(
                &mut paged_session,
                super::super::ContinueInventoryRequest {
                    expected_observation_id,
                },
                1,
                Duration::from_secs(2),
            )
            .unwrap();
            observed_names.extend(page.entries.into_iter().map(|entry| entry.name));
        }
        assert_eq!(observed_names.len(), 4);
        observed_names.sort();
        observed_names.dedup();
        assert_eq!(observed_names.len(), 4);
        assert!(observed_names.iter().all(|name| name.starts_with("item-")));
    }

    #[test]
    fn stale_observation_and_expired_limits_are_reported_without_mutating_session() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("file"), b"x").unwrap();
        let (session, _) = selected_root_session(root.path(), 1, Duration::from_secs(2)).unwrap();
        let policy = SummaryExclusionPolicy::empty_for_tests();
        assert!(summarize(
            &session,
            &policy,
            InventoryItemId::new(),
            Instant::now(),
            Duration::from_secs(2),
            MAX_ENTRIES,
            MAX_DEPTH,
        )
        .is_err());
        let expired = summarize(
            &session,
            &policy,
            session.observation_id,
            Instant::now(),
            Duration::ZERO,
            MAX_ENTRIES,
            MAX_DEPTH,
        )
        .unwrap();
        assert!(!expired.coverage_complete);
        assert_eq!(expired.stop_reason, Some("time_limit"));
    }

    #[test]
    fn summary_request_accepts_only_the_typed_observation_id() {
        let valid = serde_json::json!({
            "expectedObservationId": serde_json::to_value(InventoryItemId::new()).unwrap()
        });
        assert!(serde_json::from_value::<super::super::TreeSummaryRequest>(valid).is_ok());
        let with_path = serde_json::json!({
            "expectedObservationId": serde_json::to_value(InventoryItemId::new()).unwrap(),
            "path": "/tmp"
        });
        assert!(serde_json::from_value::<super::super::TreeSummaryRequest>(with_path).is_err());
    }

    #[test]
    fn boundaries_depth_and_entry_caps_make_coverage_incomplete() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        fs::create_dir(root.path().join("Library")).unwrap();
        fs::create_dir(root.path().join(".hidden")).unwrap();
        fs::create_dir(root.path().join("package.app")).unwrap();
        fs::write(root.path().join(".hidden-file"), b"x").unwrap();
        fs::write(root.path().join("nested/file"), b"x").unwrap();
        unix_fs::symlink("nested/file", root.path().join("alias")).unwrap();

        let (summary, _) = summarize_fixture(root.path(), MAX_ENTRIES, 1, Duration::from_secs(2));
        assert!(!summary.coverage_complete);
        assert_eq!(summary.stop_reason, Some("depth_limit"));
        assert!(summary.skipped_entries >= 5);

        let (limited, _) = summarize_fixture(root.path(), 2, MAX_DEPTH, Duration::from_secs(2));
        assert_eq!(limited.entries_observed, 2);
        assert_eq!(limited.stop_reason, Some("entry_limit"));
        assert!(!limited.coverage_complete);
    }

    #[test]
    fn hidden_regular_files_are_skipped_without_affecting_logical_totals() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("visible"), b"abc").unwrap();
        fs::write(root.path().join(".secret"), b"not observed").unwrap();
        let (summary, _) =
            summarize_fixture(root.path(), MAX_ENTRIES, MAX_DEPTH, Duration::from_secs(2));
        assert_eq!(summary.entries_observed, 2);
        assert_eq!(summary.files_observed, 1);
        assert_eq!(summary.skipped_entries, 1);
        assert_eq!(summary.unique_file_logical_bytes, "3");
        assert!(!summary.coverage_complete);
    }

    #[test]
    fn excluded_child_is_skipped_before_open_and_not_counted() {
        use rustix::fs::{statat, AtFlags, CWD};

        let root = tempdir().unwrap();
        let excluded = root.path().join("remembered");
        fs::create_dir(&excluded).unwrap();
        fs::write(excluded.join("private-child"), b"must not be counted").unwrap();
        fs::write(root.path().join("visible"), b"ok").unwrap();
        let stat = statat(CWD, &excluded, AtFlags::SYMLINK_NOFOLLOW).unwrap();
        let policy =
            SummaryExclusionPolicy::for_tests(vec![(stat.st_dev as u64, stat.st_ino as u64)]);
        let (session, _) = selected_root_session(root.path(), 1, Duration::from_secs(2)).unwrap();
        let summary = summarize(
            &session,
            &policy,
            session.observation_id,
            Instant::now(),
            Duration::from_secs(2),
            MAX_ENTRIES,
            MAX_DEPTH,
        )
        .unwrap();
        assert_eq!(summary.excluded_directories, 1);
        assert_eq!(summary.skipped_entries, 1);
        assert_eq!(summary.files_observed, 1);
        assert_eq!(summary.unique_file_logical_bytes, "2");
        assert_eq!(summary.directories_visited, 1);
        assert!(!summary.coverage_complete);
    }

    #[test]
    fn excluded_root_and_descendant_selected_through_alias_are_refused() {
        use rustix::fs::{statat, AtFlags, CWD};

        let base = tempdir().unwrap();
        let excluded = base.path().join("remembered");
        fs::create_dir_all(excluded.join("child")).unwrap();
        fs::write(excluded.join("inside"), b"x").unwrap();
        let stat = statat(CWD, &excluded, AtFlags::SYMLINK_NOFOLLOW).unwrap();
        let policy =
            SummaryExclusionPolicy::for_tests(vec![(stat.st_dev as u64, stat.st_ino as u64)]);

        let (root_session, _) =
            selected_root_session(&excluded, 1, Duration::from_secs(2)).unwrap();
        assert!(summarize(
            &root_session,
            &policy,
            root_session.observation_id,
            Instant::now(),
            Duration::from_secs(2),
            MAX_ENTRIES,
            MAX_DEPTH,
        )
        .is_err());

        let alias = base.path().join("alias");
        unix_fs::symlink(&excluded, &alias).unwrap();
        let selected_descendant = alias.join("child");
        let (descendant_session, _) =
            selected_root_session(&selected_descendant, 1, Duration::from_secs(2)).unwrap();
        assert!(summarize(
            &descendant_session,
            &policy,
            descendant_session.observation_id,
            Instant::now(),
            Duration::from_secs(2),
            MAX_ENTRIES,
            MAX_DEPTH,
        )
        .is_err());
    }

    #[test]
    fn changed_directory_metadata_is_flagged_against_captured_baseline() {
        let root = tempdir().unwrap();
        let fd = rustix::fs::open(
            root.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let baseline = fstat(&fd).unwrap();
        fs::write(root.path().join("created-after-baseline"), b"x").unwrap();
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let mut state = WalkState {
            observation_id: InventoryItemId::new(),
            root_label: "fixture".to_owned(),
            root_device: identity(&baseline).device,
            started: Instant::now(),
            duration: Duration::from_secs(2),
            max_entries: MAX_ENTRIES,
            max_depth: MAX_DEPTH,
            entries_observed: 0,
            files_observed: 0,
            duplicate_hardlink_names: 0,
            excluded_directories: 0,
            directories_visited: 0,
            skipped_entries: 0,
            unreadable_entries: 0,
            changed_directories: 0,
            unique_file_logical_bytes: 0,
            files: BTreeMap::new(),
            coverage_complete: true,
            stop_reason: None,
            halted: false,
            exclusion_policy: &policy,
        };
        walk_directory(&fd, baseline, 1, &mut state);
        let summary = state.summary();
        assert_eq!(summary.changed_directories, 1);
        assert_eq!(summary.stop_reason, Some("directory_changed"));
        assert!(!summary.coverage_complete);
    }
}
