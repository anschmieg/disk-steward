//! Retained, bounded, iterative metadata traversal over an explicitly selected folder.
//! The job owns independent directory streams; it never changes inventory paging state.

use super::{
    identity, is_dataless, is_managed_boundary, root_metadata_matches, FolderTreeSummary, Session,
};
use crate::global_exclusions::SummaryExclusionPolicy;
use disk_steward_core::{InventoryItemId, RevisionId};
use rustix::fs::{fstat, openat, statat, AtFlags, Dir, FileType, Mode, OFlags, Stat};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_DEPTH: usize = 16;
const MAX_ENTRIES: u32 = 65_536;
const MAX_BATCH_ENTRIES: u32 = 512;
const MAX_JOB_LIFETIME: Duration = Duration::from_secs(15 * 60);
const SYSTEM_BOUNDARY_NAMES: [&[u8]; 8] = [
    b"Library", b"System", b"Volumes", b"Network", b"dev", b"private", b"proc", b"sys",
];

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Progress {
    #[serde(flatten)]
    pub summary: FolderTreeSummary,
    pub scan_id: InventoryItemId,
    pub can_continue: bool,
    pub scan_finished: bool,
    pub entries_this_batch: u32,
    pub coverage_breakdown: CoverageBreakdown,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoverageBreakdown {
    pub hidden_entries: u32,
    pub symbolic_links: u32,
    pub nonregular_entries: u32,
    pub cross_volume_entries: u32,
    pub dataless_entries: u32,
    pub managed_boundaries: u32,
    pub system_boundaries: u32,
    pub depth_limited_directories: u32,
    pub pending_directories: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileVersion {
    logical_bytes: u64,
    ctime: i64,
    ctime_nsec: i64,
}

struct Frame {
    fd: OwnedFd,
    dir: Dir,
    baseline: Stat,
    name_from_parent: Option<CString>,
}

pub(super) struct SubtreeJob {
    scan_id: InventoryItemId,
    observation_id: InventoryItemId,
    revision: RevisionId,
    policy: SummaryExclusionPolicy,
    root_fd: OwnedFd,
    root_device: u64,
    root_label: String,
    frames: Vec<Frame>,
    entries_observed: u32,
    files_observed: u32,
    duplicate_hardlink_names: u32,
    directories_visited: u32,
    skipped_entries: u32,
    excluded_directories: u32,
    coverage_breakdown: CoverageBreakdown,
    unreadable_entries: u32,
    changed_directories: u32,
    unique_file_logical_bytes: u64,
    files: BTreeMap<(u64, u64), FileVersion>,
    coverage_complete: bool,
    stop_reason: Option<&'static str>,
    finished: bool,
    resource_halted: bool,
    created_at: Instant,
}

impl SubtreeJob {
    pub(super) fn start(
        session: &Session,
        policy: SummaryExclusionPolicy,
        revision: RevisionId,
    ) -> Result<Self, &'static str> {
        let started = Instant::now();
        super::validate_session_bindings(session, started, Duration::from_secs(2))?;
        if session.observation_id != session.snapshot.observation_id {
            return Err("The folder observation is stale. Inspect the current folder again.");
        }
        let source = session
            .frames
            .last()
            .ok_or("The current folder observation is unavailable.")?;
        let root_fd = openat(
            &source.fd,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| "The selected folder could not be opened safely.")?;
        let baseline =
            fstat(&root_fd).map_err(|_| "The selected folder metadata is unavailable.")?;
        if identity(&baseline) != source.identity
            || source.identity.device != session.root_device
            || FileType::from_raw_mode(baseline.st_mode) != FileType::Directory
            || is_dataless(&baseline)
        {
            return Err("The selected folder changed. Inspect it again.");
        }
        policy.ensure_ancestor_scope_allowed(&root_fd, Instant::now(), Duration::from_secs(2))?;
        let dir = Dir::read_from(&root_fd)
            .map_err(|_| "The selected folder could not be read safely.")?;
        super::validate_session_bindings(session, started, Duration::from_secs(2))?;
        Ok(Self {
            scan_id: InventoryItemId::new(),
            observation_id: session.observation_id,
            revision,
            policy,
            root_fd: openat(
                &root_fd,
                ".",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| "The selected folder could not be opened safely.")?,
            root_device: session.root_device,
            root_label: source.label.clone(),
            frames: vec![Frame {
                fd: root_fd,
                dir,
                baseline,
                name_from_parent: None,
            }],
            entries_observed: 0,
            files_observed: 0,
            duplicate_hardlink_names: 0,
            directories_visited: 1,
            skipped_entries: 0,
            excluded_directories: 0,
            coverage_breakdown: CoverageBreakdown::default(),
            unreadable_entries: 0,
            changed_directories: 0,
            unique_file_logical_bytes: 0,
            files: BTreeMap::new(),
            coverage_complete: true,
            stop_reason: None,
            finished: false,
            resource_halted: false,
            created_at: Instant::now(),
        })
    }

    pub(super) fn id(&self) -> InventoryItemId {
        self.scan_id
    }
    pub(super) fn observation_id(&self) -> InventoryItemId {
        self.observation_id
    }
    pub(super) fn revision(&self) -> RevisionId {
        self.revision
    }
    pub(super) fn policy(&self) -> &SummaryExclusionPolicy {
        &self.policy
    }

    pub(super) fn advance(
        &mut self,
        cancel: &AtomicBool,
        started: Instant,
        duration: Duration,
    ) -> Result<Progress, &'static str> {
        let duration = duration.min(Duration::from_secs(2));
        let created_at = self.created_at;
        let guard = || -> Result<(), &'static str> {
            if cancel.load(Ordering::Acquire) {
                return Err("Subtree summary was canceled.");
            }
            if started.elapsed() >= duration || created_at.elapsed() >= MAX_JOB_LIFETIME {
                return Err("Subtree summary observation expired.");
            }
            Ok(())
        };
        if self.finished {
            return Ok(self.progress(0));
        }
        guard()?;
        self.validate_stack(&guard)?;
        self.policy
            .ensure_ancestor_scope_allowed(&self.root_fd, started, duration)?;
        if self.resource_halted {
            return Ok(self.progress(0));
        }
        let mut processed = 0u32;
        while !self.frames.is_empty() && processed < MAX_BATCH_ENTRIES {
            guard()?;
            if self.entries_observed >= MAX_ENTRIES {
                self.coverage_complete = false;
                self.stop_reason.get_or_insert("entry_limit");
                self.resource_halted = true;
                break;
            }
            let top = self.frames.len() - 1;
            let next = self.frames[top].dir.read();
            guard()?;
            let Some(entry) = next else {
                if top == 0 {
                    let after = fstat(&self.frames[0].fd)
                        .map_err(|_| "The selected folder changed during inspection.")?;
                    guard()?;
                    if !root_metadata_matches(&self.frames[0].baseline, &after)
                        || is_dataless(&after)
                    {
                        return Err("The selected folder changed during inspection.");
                    }
                    self.frames.clear();
                    self.finished = true;
                    break;
                }
                self.pop_validated_child(top, &guard)?;
                continue;
            };
            let entry = entry.map_err(|_| "A folder could not be read safely.")?;
            let raw = entry.file_name().to_bytes();
            if raw == b"." || raw == b".." {
                continue;
            }
            self.entries_observed += 1;
            processed += 1;
            guard()?;
            let metadata = statat(
                &self.frames[top].fd,
                entry.file_name(),
                AtFlags::SYMLINK_NOFOLLOW,
            );
            guard()?;
            let metadata = match metadata {
                Ok(stat) => stat,
                Err(_) => {
                    self.unreadable_entries = self.unreadable_entries.saturating_add(1);
                    self.coverage_complete = false;
                    self.stop_reason.get_or_insert("entry_metadata_unavailable");
                    continue;
                }
            };
            let kind = FileType::from_raw_mode(metadata.st_mode);
            if kind == FileType::Directory && self.policy.excludes(&metadata) {
                self.mark_skipped();
                self.excluded_directories = self.excluded_directories.saturating_add(1);
                continue;
            }
            // One deterministic primary reason is recorded for each skipped entry.
            if raw.first() == Some(&b'.') {
                self.skip(|coverage| coverage.hidden_entries += 1);
                continue;
            }
            if is_dataless(&metadata) {
                self.skip(|coverage| coverage.dataless_entries += 1);
                continue;
            }
            if kind == FileType::Symlink {
                self.skip(|coverage| coverage.symbolic_links += 1);
                continue;
            }
            if metadata.st_dev as u64 != self.root_device {
                self.skip(|coverage| coverage.cross_volume_entries += 1);
                continue;
            }
            if kind == FileType::RegularFile {
                self.observe_file(&metadata)?;
                continue;
            }
            if kind != FileType::Directory {
                self.skip(|coverage| coverage.nonregular_entries += 1);
                continue;
            }
            if is_managed_boundary(raw) {
                self.skip(|coverage| coverage.managed_boundaries += 1);
                continue;
            }
            if SYSTEM_BOUNDARY_NAMES
                .iter()
                .any(|boundary| raw.eq_ignore_ascii_case(boundary))
            {
                self.skip(|coverage| coverage.system_boundaries += 1);
                continue;
            }
            if self.frames.len() >= MAX_DEPTH {
                self.skip(|coverage| coverage.depth_limited_directories += 1);
                self.stop_reason.get_or_insert("depth_limit");
                continue;
            }
            let name = CString::new(raw).map_err(|_| "A folder entry could not be validated.")?;
            guard()?;
            let named_before = statat(
                &self.frames[top].fd,
                name.as_c_str(),
                AtFlags::SYMLINK_NOFOLLOW,
            );
            guard()?;
            let named_before = named_before.map_err(|_| "A folder changed during inspection.")?;
            if identity(&named_before) != identity(&metadata)
                || named_before.st_dev as u64 != self.root_device
                || FileType::from_raw_mode(named_before.st_mode) != FileType::Directory
                || is_dataless(&named_before)
            {
                return Err("A folder changed during inspection.");
            }
            guard()?;
            let child_fd = openat(
                &self.frames[top].fd,
                name.as_c_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| "A folder could not be opened safely.")?;
            guard()?;
            let child_stat = fstat(&child_fd).map_err(|_| "A folder changed during inspection.")?;
            guard()?;
            if identity(&child_stat) != identity(&named_before)
                || FileType::from_raw_mode(child_stat.st_mode) != FileType::Directory
                || child_stat.st_dev as u64 != self.root_device
                || is_dataless(&child_stat)
            {
                return Err("A folder changed during inspection.");
            }
            let child_dir =
                Dir::read_from(&child_fd).map_err(|_| "A folder could not be read safely.")?;
            guard()?;
            self.directories_visited = self.directories_visited.saturating_add(1);
            self.frames.push(Frame {
                fd: child_fd,
                dir: child_dir,
                baseline: child_stat,
                name_from_parent: Some(name),
            });
        }
        guard()?;
        self.validate_stack(&guard)?;
        self.policy
            .ensure_ancestor_scope_allowed(&self.root_fd, started, duration)?;
        Ok(self.progress(processed))
    }

    fn progress(&self, entries_this_batch: u32) -> Progress {
        Progress {
            summary: FolderTreeSummary {
                observation_id: self.observation_id,
                root_label: self.root_label.clone(),
                entries_observed: self.entries_observed,
                files_observed: self.files_observed,
                unique_file_objects: self.files.len().min(u32::MAX as usize) as u32,
                duplicate_hardlink_names: self.duplicate_hardlink_names,
                directories_visited: self.directories_visited,
                skipped_entries: self.skipped_entries,
                excluded_directories: self.excluded_directories,
                unreadable_entries: self.unreadable_entries,
                changed_directories: self.changed_directories,
                unique_file_logical_bytes: self.unique_file_logical_bytes.to_string(),
                coverage_complete: self.finished && self.coverage_complete,
                stop_reason: self.stop_reason,
            },
            scan_id: self.scan_id,
            can_continue: !self.finished && !self.resource_halted,
            scan_finished: self.finished,
            entries_this_batch,
            coverage_breakdown: CoverageBreakdown {
                pending_directories: if self.finished {
                    0
                } else {
                    self.frames.len() as u32
                },
                ..self.coverage_breakdown.clone()
            },
        }
    }

    fn mark_skipped(&mut self) {
        self.skipped_entries = self.skipped_entries.saturating_add(1);
        self.coverage_complete = false;
    }

    fn skip(&mut self, count_reason: impl FnOnce(&mut CoverageBreakdown)) {
        self.mark_skipped();
        count_reason(&mut self.coverage_breakdown);
    }

    fn observe_file(&mut self, metadata: &Stat) -> Result<(), &'static str> {
        self.files_observed = self.files_observed.saturating_add(1);
        let logical_bytes =
            u64::try_from(metadata.st_size).map_err(|_| "File size is unavailable.")?;
        let key = (metadata.st_dev as u64, metadata.st_ino);
        let version = FileVersion {
            logical_bytes,
            ctime: metadata.st_ctime,
            ctime_nsec: metadata.st_ctime_nsec,
        };
        if let Some(existing) = self.files.get(&key) {
            if existing != &version {
                return Err("File metadata changed during inspection.");
            }
            self.duplicate_hardlink_names = self.duplicate_hardlink_names.saturating_add(1);
            return Ok(());
        }
        self.unique_file_logical_bytes = self
            .unique_file_logical_bytes
            .checked_add(logical_bytes)
            .ok_or("File sizes exceeded the supported range.")?;
        self.files.insert(key, version);
        Ok(())
    }

    fn validate_stack<F>(&self, guard: &F) -> Result<(), &'static str>
    where
        F: Fn() -> Result<(), &'static str>,
    {
        for (index, frame) in self.frames.iter().enumerate() {
            guard()?;
            let held = fstat(&frame.fd).map_err(|_| "A folder changed during inspection.")?;
            guard()?;
            if !root_metadata_matches(&frame.baseline, &held)
                || identity(&held) != identity(&frame.baseline)
                || FileType::from_raw_mode(held.st_mode) != FileType::Directory
                || is_dataless(&held)
            {
                return Err("A folder changed during inspection.");
            }
            if index > 0 {
                let parent = &self.frames[index - 1];
                let name = frame
                    .name_from_parent
                    .as_ref()
                    .ok_or("A folder changed during inspection.")?;
                guard()?;
                let named = statat(&parent.fd, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|_| "A folder changed during inspection.")?;
                guard()?;
                if identity(&named) != identity(&frame.baseline)
                    || !root_metadata_matches(&frame.baseline, &named)
                    || is_dataless(&named)
                {
                    return Err("A folder changed during inspection.");
                }
            }
        }
        Ok(())
    }

    fn pop_validated_child<F>(&mut self, child_index: usize, guard: &F) -> Result<(), &'static str>
    where
        F: Fn() -> Result<(), &'static str>,
    {
        self.validate_stack(guard)?;
        let frame = self
            .frames
            .get(child_index)
            .ok_or("A folder changed during inspection.")?;
        guard()?;
        let held = fstat(&frame.fd).map_err(|_| "A folder changed during inspection.")?;
        guard()?;
        if !root_metadata_matches(&frame.baseline, &held) || is_dataless(&held) {
            return Err("A folder changed during inspection.");
        }
        self.frames.pop();
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    fn session(
        path: &std::path::Path,
    ) -> (super::super::Session, super::super::FolderInventorySnapshot) {
        super::super::selected_root_session(path, 512, Duration::from_secs(2)).unwrap()
    }

    fn drain(job: &mut SubtreeJob) -> Vec<Progress> {
        let cancel = AtomicBool::new(false);
        let mut pages = Vec::new();
        loop {
            let page = job
                .advance(&cancel, Instant::now(), Duration::from_secs(2))
                .unwrap();
            let finished = page.scan_finished;
            pages.push(page);
            if finished {
                break;
            }
        }
        pages
    }

    #[test]
    fn total_entry_limit_and_expiry_refuse_completion_claims() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("one"), b"x").unwrap();
        std::fs::write(root.path().join("two"), b"x").unwrap();
        let (session, _) = session(root.path());
        let mut job = SubtreeJob::start(
            &session,
            SummaryExclusionPolicy::empty_for_tests(),
            RevisionId::new(),
        )
        .unwrap();
        // Reduced-state boundary test; this is not a 65,536-entry performance probe.
        job.entries_observed = MAX_ENTRIES - 1;
        let result = job
            .advance(
                &AtomicBool::new(false),
                Instant::now(),
                Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(result.summary.stop_reason, Some("entry_limit"));
        assert!(!result.can_continue);
        assert!(!result.scan_finished);
        assert!(!result.summary.coverage_complete);
        let mut expired = SubtreeJob::start(
            &session,
            SummaryExclusionPolicy::empty_for_tests(),
            RevisionId::new(),
        )
        .unwrap();
        expired.created_at = Instant::now()
            .checked_sub(MAX_JOB_LIFETIME + Duration::from_secs(1))
            .unwrap();
        assert!(expired
            .advance(
                &AtomicBool::new(false),
                Instant::now(),
                Duration::from_secs(2)
            )
            .is_err());
    }

    #[test]
    fn case_variant_system_boundaries_are_skipped() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("lIbRaRy")).unwrap();
        std::fs::write(root.path().join("lIbRaRy/private-fixture"), b"not observed").unwrap();
        let (session, _) = session(root.path());
        let mut job = SubtreeJob::start(
            &session,
            SummaryExclusionPolicy::empty_for_tests(),
            RevisionId::new(),
        )
        .unwrap();
        let pages = drain(&mut job);
        let last = pages.last().unwrap();
        assert_eq!(last.summary.files_observed, 0);
        assert_eq!(last.summary.skipped_entries, 1);
        assert!(!last.summary.coverage_complete);
    }

    #[test]
    fn continues_across_many_batches_without_changing_inventory_paging() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..4_200 {
            std::fs::write(root.path().join(format!("file-{index:05}")), b"abc").unwrap();
        }
        let (session, first_page) = session(root.path());
        let old_page = (
            session.snapshot.page_number,
            session.snapshot.entries_seen.clone(),
            session.snapshot.can_continue,
        );
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let mut job = SubtreeJob::start(&session, policy, RevisionId::new()).unwrap();
        let pages = drain(&mut job);
        let final_page = pages.last().unwrap();
        assert!(pages.len() > 8);
        assert!(pages
            .iter()
            .take(pages.len() - 1)
            .all(|page| page.coverage_breakdown.pending_directories > 0));
        assert_eq!(final_page.coverage_breakdown.pending_directories, 0);
        let serialized = serde_json::to_value(final_page).unwrap();
        assert_eq!(serialized["coverageBreakdown"]["pendingDirectories"], 0);
        assert_eq!(serialized["coverageBreakdown"]["datalessEntries"], 0);
        assert!(pages
            .iter()
            .take(pages.len() - 1)
            .all(|page| page.can_continue && !page.scan_finished));
        assert_eq!(final_page.summary.entries_observed, 4_200);
        assert_eq!(final_page.summary.files_observed, 4_200);
        assert_eq!(final_page.summary.unique_file_objects, 4_200);
        assert_eq!(
            final_page.summary.unique_file_logical_bytes,
            (4_200 * 3).to_string()
        );
        assert!(final_page.summary.coverage_complete);
        assert_eq!(session.snapshot.page_number, old_page.0);
        assert_eq!(session.snapshot.entries_seen, old_page.1);
        assert_eq!(session.snapshot.can_continue, old_page.2);
        assert_eq!(first_page.page_number, old_page.0);
    }

    #[test]
    fn nested_hardlinks_hidden_symlink_and_excluded_folder_follow_summary_policy() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        let excluded = root.path().join("excluded");
        std::fs::create_dir(&nested).unwrap();
        std::fs::create_dir(&excluded).unwrap();
        std::fs::write(root.path().join("visible"), b"1234").unwrap();
        std::fs::hard_link(root.path().join("visible"), nested.join("alias")).unwrap();
        std::fs::write(excluded.join("secret"), b"ignored").unwrap();
        std::fs::write(root.path().join(".hidden"), b"ignored").unwrap();
        symlink(root.path().join("visible"), root.path().join("link")).unwrap();
        symlink(
            root.path().join("visible"),
            root.path().join(".hidden-link"),
        )
        .unwrap();
        let _socket = UnixListener::bind(root.path().join("local.sock")).unwrap();
        let (session, _) = session(root.path());
        let excluded_stat =
            rustix::fs::statat(rustix::fs::CWD, &excluded, AtFlags::SYMLINK_NOFOLLOW).unwrap();
        let policy = SummaryExclusionPolicy::for_tests(vec![(
            excluded_stat.st_dev as u64,
            excluded_stat.st_ino,
        )]);
        let mut job = SubtreeJob::start(&session, policy, RevisionId::new()).unwrap();
        let final_page = drain(&mut job).pop().unwrap();
        assert_eq!(final_page.summary.files_observed, 2);
        assert_eq!(final_page.summary.unique_file_objects, 1);
        assert_eq!(final_page.summary.duplicate_hardlink_names, 1);
        assert_eq!(final_page.summary.unique_file_logical_bytes, "4");
        assert_eq!(final_page.summary.excluded_directories, 1);
        assert_eq!(final_page.coverage_breakdown.hidden_entries, 2);
        assert_eq!(final_page.coverage_breakdown.symbolic_links, 1);
        assert_eq!(final_page.coverage_breakdown.nonregular_entries, 1);
        assert_eq!(final_page.coverage_breakdown.pending_directories, 0);
        assert_eq!(final_page.summary.skipped_entries, 5);
        assert!(!final_page.summary.coverage_complete);
    }

    #[test]
    fn managed_and_system_boundary_counts_are_distinct() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("bundle.app")).unwrap();
        std::fs::create_dir(root.path().join("lIbRaRy")).unwrap();
        let (session, _) = session(root.path());
        let mut job = SubtreeJob::start(
            &session,
            SummaryExclusionPolicy::empty_for_tests(),
            RevisionId::new(),
        )
        .unwrap();
        let final_page = drain(&mut job).pop().unwrap();
        assert_eq!(final_page.coverage_breakdown.managed_boundaries, 1);
        assert_eq!(final_page.coverage_breakdown.system_boundaries, 1);
        assert_eq!(final_page.summary.skipped_entries, 2);
    }

    #[test]
    fn cancellation_and_root_change_refuse_the_in_progress_evidence() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..600 {
            std::fs::write(root.path().join(format!("file-{index:04}")), b"x").unwrap();
        }
        let (session, _) = session(root.path());
        let mut canceled = SubtreeJob::start(
            &session,
            SummaryExclusionPolicy::empty_for_tests(),
            RevisionId::new(),
        )
        .unwrap();
        let cancel = AtomicBool::new(true);
        assert!(canceled
            .advance(&cancel, Instant::now(), Duration::from_secs(2))
            .is_err());

        let mut changed = SubtreeJob::start(
            &session,
            SummaryExclusionPolicy::empty_for_tests(),
            RevisionId::new(),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        changed
            .advance(&cancel, Instant::now(), Duration::from_secs(2))
            .unwrap();
        std::fs::write(root.path().join("new-entry"), b"x").unwrap();
        assert!(changed
            .advance(&cancel, Instant::now(), Duration::from_secs(2))
            .is_err());
    }

    #[test]
    fn changed_held_child_between_batches_refuses_the_job() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("a-nested");
        std::fs::create_dir(&nested).unwrap();
        for index in 0..600 {
            std::fs::write(nested.join(format!("file-{index:04}")), b"x").unwrap();
        }
        let (session, _) = session(root.path());
        let mut job = SubtreeJob::start(
            &session,
            SummaryExclusionPolicy::empty_for_tests(),
            RevisionId::new(),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        job.advance(&cancel, Instant::now(), Duration::from_secs(2))
            .unwrap();
        std::fs::write(nested.join("new-entry"), b"x").unwrap();
        assert!(job
            .advance(&cancel, Instant::now(), Duration::from_secs(2))
            .is_err());
    }

    #[test]
    fn depth_cap_is_reported_as_a_gap_while_siblings_finish() {
        let root = tempfile::tempdir().unwrap();
        let mut current = root.path().to_path_buf();
        for depth in 0..MAX_DEPTH {
            current.push(format!("level-{depth}"));
            std::fs::create_dir(&current).unwrap();
        }
        std::fs::write(current.join("too-deep"), b"x").unwrap();
        std::fs::write(root.path().join("sibling"), b"ok").unwrap();
        let (session, _) = session(root.path());
        let mut job = SubtreeJob::start(
            &session,
            SummaryExclusionPolicy::empty_for_tests(),
            RevisionId::new(),
        )
        .unwrap();
        let final_page = drain(&mut job).pop().unwrap();
        assert!(final_page.scan_finished);
        assert_eq!(final_page.summary.stop_reason, Some("depth_limit"));
        assert_eq!(final_page.summary.files_observed, 1);
        assert_eq!(final_page.summary.unique_file_logical_bytes, "2");
        assert_eq!(final_page.coverage_breakdown.depth_limited_directories, 1);
        assert!(!final_page.summary.coverage_complete);
    }
}
