//! Bounded replay recovery for metadata-only subtree scans.

use super::checkpoint_store::RootStamp;
use super::subtree_job::{Progress, SubtreeJob};
use super::{checkpoint_replay, Session};
use crate::global_exclusions::SummaryExclusionPolicy;
use disk_steward_core::{InventoryItemId, RevisionId};
use rustix::fs::{fstat, FileType};
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const REPLAY_VERSION: u32 = 1;
const MAX_REPLAY_BATCHES: u32 = 128;
const MAX_REPLAY_ENTRIES: u32 = 65_536;
const MAX_REPLAY_ENTRIES_PER_BATCH: u32 = 512;
const MAX_REPLAY_DURATION: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ReplayPayload {
    pub version: u32,
    pub checkpoint_id: InventoryItemId,
    pub root_label: String,
    pub root_stamp: RootStamp,
    pub exclusion_revision: RevisionId,
    pub batches: u32,
    pub entries_observed: u32,
    pub transcript: [u8; 32],
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReplayInfo {
    pub checkpoint_id: InventoryItemId,
    pub root_label: String,
    pub entries_observed: u32,
    pub batches: u32,
}

impl From<&ReplayPayload> for ReplayInfo {
    fn from(payload: &ReplayPayload) -> Self {
        Self {
            checkpoint_id: payload.checkpoint_id,
            root_label: payload.root_label.clone(),
            entries_observed: payload.entries_observed,
            batches: payload.batches,
        }
    }
}

pub(super) fn validate_payload(payload: &ReplayPayload) -> Result<(), &'static str> {
    let stamp = &payload.root_stamp;
    if payload.version != REPLAY_VERSION
        || payload.root_label.is_empty()
        || payload.root_label.len() > 480
        || payload
            .root_label
            .chars()
            .any(|ch| ch.is_control() || super::is_bidi_control(ch))
        || !(1..=MAX_REPLAY_BATCHES).contains(&payload.batches)
        || !(1..=MAX_REPLAY_ENTRIES).contains(&payload.entries_observed)
        || payload.entries_observed > payload.batches.saturating_mul(MAX_REPLAY_ENTRIES_PER_BATCH)
        || stamp.mode & 0o170000 != 0o040000
        || stamp.nlink == 0
        || stamp.size < 0
        || !(0..1_000_000_000).contains(&stamp.mtime_nsec)
        || !(0..1_000_000_000).contains(&stamp.ctime_nsec)
        || !(0..1_000_000_000).contains(&stamp.birth_nanos)
    {
        return Err("Recursive checkpoint is invalid or exceeds its replay limits.");
    }
    Ok(())
}

pub(super) fn resume(
    session: &Session,
    payload: &ReplayPayload,
    policy: SummaryExclusionPolicy,
    revision: RevisionId,
    cancel: &AtomicBool,
    started: Instant,
    duration: Duration,
) -> Result<(SubtreeJob, Progress), &'static str> {
    let duration = duration.min(MAX_REPLAY_DURATION);
    validate_payload(payload)?;
    replay_guard(cancel, started, duration)?;
    if revision != payload.exclusion_revision {
        return Err("Remembered exclusions changed since this scan was saved.");
    }
    let source = session
        .frames
        .last()
        .ok_or("The selected folder is unavailable.")?;
    if source.label != payload.root_label {
        return Err("The selected folder does not match this recursive checkpoint.");
    }
    let before = fstat(&source.fd).map_err(|_| "The selected folder could not be verified.")?;
    replay_guard(cancel, started, duration)?;
    if super::identity(&before) != source.identity
        || FileType::from_raw_mode(before.st_mode) != FileType::Directory
        || super::is_dataless(&before)
        || checkpoint_replay::stamp(&before)? != payload.root_stamp
    {
        return Err("The selected folder does not match this recursive checkpoint.");
    }
    super::validate_session_bindings(session, started, duration)?;
    replay_guard(cancel, started, duration)?;

    let mut job = SubtreeJob::start_with_deadline(session, policy, revision, started, duration)?;
    replay_guard(cancel, started, duration)?;
    if job.root_stamp() != &payload.root_stamp || job.root_label() != payload.root_label.as_str() {
        return Err("The selected folder changed while replay was starting.");
    }
    let mut progress = None;
    for _ in 0..payload.batches {
        replay_guard(cancel, started, duration)?;
        let next = job.advance(cancel, started, duration)?;
        if !next.can_continue || next.scan_finished {
            return Err("The saved recursive scan could not be replayed completely.");
        }
        progress = Some(next);
    }
    let progress = progress.ok_or("Recursive checkpoint has no replay batches.")?;
    replay_guard(cancel, started, duration)?;
    if job.completed_batches() != payload.batches
        || job.entries_observed() != payload.entries_observed
        || job.transcript_digest() != payload.transcript
    {
        return Err("The selected folder's observed metadata differs from this checkpoint.");
    }
    let after = fstat(&source.fd).map_err(|_| "The selected folder could not be verified.")?;
    replay_guard(cancel, started, duration)?;
    if !super::root_metadata_matches(&before, &after)
        || checkpoint_replay::stamp(&after)? != payload.root_stamp
    {
        return Err("The selected folder changed during recursive checkpoint replay.");
    }
    super::validate_session_bindings(session, started, duration)?;
    replay_guard(cancel, started, duration)?;
    Ok((job, progress))
}

fn replay_guard(
    cancel: &AtomicBool,
    started: Instant,
    duration: Duration,
) -> Result<(), &'static str> {
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err("Recursive checkpoint replay was canceled.");
    }
    if started.elapsed() >= duration {
        return Err("Recursive checkpoint replay exceeded its cooperative time limit.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_exclusions::SummaryExclusionPolicy;
    use std::io::Write;
    use std::sync::atomic::AtomicBool;
    use tempfile::tempdir;

    fn make_files(path: &std::path::Path, count: usize) {
        for index in 0..count {
            std::fs::write(path.join(format!("item-{index:05}")), b"data").unwrap();
        }
    }

    fn fresh_session(path: &std::path::Path) -> Session {
        super::super::selected_root_session(path, 512, MAX_REPLAY_DURATION)
            .unwrap()
            .0
    }

    fn advance(job: &mut SubtreeJob) -> Progress {
        job.advance(&AtomicBool::new(false), Instant::now(), MAX_REPLAY_DURATION)
            .unwrap()
    }

    #[test]
    fn replays_completed_batches_into_a_fresh_session_and_continues_exactly() {
        let root = tempdir().unwrap();
        make_files(root.path(), 1_100);
        let old_session = fresh_session(root.path());
        let old_observation = old_session.observation_id;
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let revision = RevisionId::new();
        let mut old_job = SubtreeJob::start(&old_session, policy, revision).unwrap();
        assert!(advance(&mut old_job).can_continue);
        assert!(advance(&mut old_job).can_continue);
        let payload = old_job
            .capture_replay(Instant::now(), MAX_REPLAY_DURATION)
            .unwrap();
        let old_scan_id = old_job.id();
        assert_eq!(payload.batches, 2);
        assert_eq!(payload.entries_observed, 1_024);
        let serialized = serde_json::to_string(&payload).unwrap();
        assert!(!serialized.contains("item-00000"));
        assert!(!serialized.contains(root.path().to_str().unwrap()));
        drop(old_job);
        drop(old_session);

        let new_session = fresh_session(root.path());
        assert_ne!(new_session.observation_id, old_observation);
        let started = Instant::now();
        let (mut resumed, progress) = resume(
            &new_session,
            &payload,
            SummaryExclusionPolicy::empty_for_tests(),
            revision,
            &AtomicBool::new(false),
            started,
            MAX_REPLAY_DURATION,
        )
        .unwrap();
        assert_ne!(resumed.id(), old_scan_id);
        assert_eq!(progress.summary.observation_id, new_session.observation_id);
        assert_eq!(progress.summary.entries_observed, 1_024);
        assert_eq!(progress.summary.files_observed, 1_024);
        assert_eq!(progress.summary.unique_file_objects, 1_024);
        assert_eq!(progress.summary.unique_file_logical_bytes, "4096");
        assert!(progress.can_continue);

        let final_progress = advance(&mut resumed);
        assert!(final_progress.scan_finished);
        assert_eq!(final_progress.summary.entries_observed, 1_100);
        assert_eq!(final_progress.summary.files_observed, 1_100);
        assert_eq!(final_progress.summary.unique_file_objects, 1_100);
        assert_eq!(final_progress.summary.unique_file_logical_bytes, "4400");
    }

    #[test]
    fn modified_processed_file_is_rejected_even_when_root_stamp_is_unchanged() {
        let root = tempdir().unwrap();
        make_files(root.path(), 700);
        let old_session = fresh_session(root.path());
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let revision = RevisionId::new();
        let mut job = SubtreeJob::start(&old_session, policy, revision).unwrap();
        assert!(advance(&mut job).can_continue);
        let payload = job
            .capture_replay(Instant::now(), MAX_REPLAY_DURATION)
            .unwrap();
        let root_before =
            checkpoint_replay::stamp(&fstat(&old_session.frames[0].fd).unwrap()).unwrap();

        for index in 0..700 {
            let path = root.path().join(format!("item-{index:05}"));
            let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            file.write_all(b"changed").unwrap();
        }
        let root_after =
            checkpoint_replay::stamp(&fstat(&old_session.frames[0].fd).unwrap()).unwrap();
        assert_eq!(root_before, root_after);
        drop(job);
        drop(old_session);

        let new_session = fresh_session(root.path());
        assert!(resume(
            &new_session,
            &payload,
            SummaryExclusionPolicy::empty_for_tests(),
            revision,
            &AtomicBool::new(false),
            Instant::now(),
            MAX_REPLAY_DURATION,
        )
        .is_err());
    }

    #[test]
    fn wrong_root_revision_hash_and_expired_deadline_are_refused() {
        let root_parent = tempdir().unwrap();
        let root = root_parent.path().join("selected");
        std::fs::create_dir(&root).unwrap();
        make_files(&root, 700);
        let old_session = fresh_session(&root);
        let revision = RevisionId::new();
        let mut job = SubtreeJob::start(
            &old_session,
            SummaryExclusionPolicy::empty_for_tests(),
            revision,
        )
        .unwrap();
        assert!(advance(&mut job).can_continue);
        let payload = job
            .capture_replay(Instant::now(), MAX_REPLAY_DURATION)
            .unwrap();

        let other_parent = tempdir().unwrap();
        let other_root = other_parent.path().join("selected");
        std::fs::create_dir(&other_root).unwrap();
        make_files(&other_root, 700);
        let other_session = fresh_session(&other_root);
        assert!(resume(
            &other_session,
            &payload,
            SummaryExclusionPolicy::empty_for_tests(),
            revision,
            &AtomicBool::new(false),
            Instant::now(),
            MAX_REPLAY_DURATION,
        )
        .is_err());

        let current_session = fresh_session(&root);
        assert!(resume(
            &current_session,
            &payload,
            SummaryExclusionPolicy::empty_for_tests(),
            RevisionId::new(),
            &AtomicBool::new(false),
            Instant::now(),
            MAX_REPLAY_DURATION,
        )
        .is_err());

        let mut tampered = payload.clone();
        tampered.transcript[0] ^= 1;
        assert!(resume(
            &current_session,
            &tampered,
            SummaryExclusionPolicy::empty_for_tests(),
            revision,
            &AtomicBool::new(false),
            Instant::now(),
            MAX_REPLAY_DURATION,
        )
        .is_err());

        assert!(resume(
            &current_session,
            &payload,
            SummaryExclusionPolicy::empty_for_tests(),
            revision,
            &AtomicBool::new(false),
            Instant::now(),
            Duration::ZERO,
        )
        .is_err());
    }
}
