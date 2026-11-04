//! Explicit, bounded byte comparison for selected regular files in the current observation.
//! Source bytes are read only for this request and are never retained or returned.

use super::{
    check_summary_cancellation, identity, is_dataless, is_managed_boundary, root_metadata_matches,
    validate_session_bindings, FolderInventoryState, Session,
};
use crate::global_exclusions::{ExclusionState, SummaryExclusionPolicy};
use crate::summary_control::SummaryControlState;
use disk_steward_core::{DuplicateGroupId, InventoryItemId, RevisionId, SummaryAttemptId};
use rustix::fs::{fstat, openat, statat, AtFlags, FileType, Mode, OFlags, Stat};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::os::fd::OwnedFd;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::duplicate_content::{self, ReadBudget};

const RETAINED_REVIEW_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CompareRequest {
    expected_observation_id: InventoryItemId,
    entry_ids: Vec<InventoryItemId>,
    attempt_id: SummaryAttemptId,
    allow_content_read: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CancelRequest {
    expected_observation_id: InventoryItemId,
    attempt_id: SummaryAttemptId,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Comparison {
    observation_id: InventoryItemId,
    comparison_id: InventoryItemId,
    detector_version: String,
    files_compared: u32,
    bytes_read: String,
    groups: Vec<DuplicateGroup>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DuplicateGroup {
    id: DuplicateGroupId,
    entry_ids: Vec<InventoryItemId>,
    logical_bytes: String,
}

pub(super) struct RetainedComparison {
    comparison: Comparison,
    exclusion_revision: RevisionId,
    created_at: Instant,
    items: Vec<disk_steward_plan_model::InventoryItem>,
    duplicate_sets: Vec<disk_steward_plan_model::DuplicateSet>,
}

#[tauri::command]
pub(crate) async fn compare_observed_files(
    folders: tauri::State<'_, FolderInventoryState>,
    control: tauri::State<'_, SummaryControlState>,
    exclusions: tauri::State<'_, ExclusionState>,
    request: CompareRequest,
) -> Result<Comparison, String> {
    if !request.allow_content_read {
        return Err("Explicit content-read consent is required for duplicate comparison.".into());
    }
    let guard = crate::NativeCheckGuard::acquire()?;
    let registration = control
        .register(request.expected_observation_id, request.attempt_id)
        .map_err(|_| "A metadata operation is already active.".to_owned())?;
    let sessions = Arc::clone(&folders.0);
    let exclusions = exclusions.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let _registration = registration;
        let cancellation = _registration.cancellation_flag();
        let started = Instant::now();
        let duration = duplicate_content::MAX_DURATION;
        let mut current = sessions
            .lock()
            .map_err(|_| "Folder observation is unavailable.".to_owned())?;
        let session = current
            .as_mut()
            .ok_or("Choose a folder before comparing selected files.")?;
        if session.observation_id != request.expected_observation_id
            || session.snapshot.observation_id != request.expected_observation_id
        {
            return Err("Folder observation is stale. Inspect it again.".into());
        }
        // A new explicit attempt supersedes the previous importable proof immediately.
        session.duplicate_evidence = None;
        check_summary_cancellation(cancellation)?;
        let (revision, policy) = exclusions
            .summary_policy(started, duration)
            .map_err(str::to_owned)?;
        let result = compare_session(
            session,
            &request.entry_ids,
            &policy,
            revision,
            cancellation,
            started,
            duration,
        )?;
        check_summary_cancellation(cancellation)?;
        let (after_revision, after_policy) = exclusions
            .summary_policy(started, duration)
            .map_err(str::to_owned)?;
        check_summary_cancellation(cancellation)?;
        if after_revision != revision || after_policy != policy {
            return Err("Remembered exclusions changed during duplicate comparison.".into());
        }
        if started.elapsed() >= duration {
            return Err("Duplicate comparison exceeded its cooperative time limit.".into());
        }
        retain_after_success(session, &result, revision).map_err(str::to_owned)?;
        if check_summary_cancellation(cancellation).is_err() || started.elapsed() >= duration {
            session.duplicate_evidence = None;
            return Err("Duplicate comparison ended without a current importable proof.".into());
        }
        Ok(result)
    })
    .await
    .map_err(|_| "Duplicate comparison did not complete reliably.".to_owned())?
}

#[tauri::command]
pub(crate) async fn cancel_duplicate_comparison(
    control: tauri::State<'_, SummaryControlState>,
    request: CancelRequest,
) -> Result<(), String> {
    control
        .cancel(request.expected_observation_id, request.attempt_id)
        .map_err(|_| "The duplicate comparison is no longer active.".to_owned())
}

struct SelectedFile {
    id: InventoryItemId,
    name: CString,
    fd: OwnedFd,
    stamp: super::checkpoint_store::RootStamp,
    size: u64,
}

fn validate_selected(file: &SelectedFile, parent: &OwnedFd) -> Result<(), String> {
    let held = fstat(&file.fd).map_err(|_| "A selected file changed during comparison.")?;
    let named = statat(parent, file.name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| "A selected file changed during comparison.")?;
    if is_dataless(&held)
        || is_dataless(&named)
        || super::checkpoint_replay::stamp(&held).map_err(str::to_owned)? != file.stamp
        || super::checkpoint_replay::stamp(&named).map_err(str::to_owned)? != file.stamp
    {
        return Err("A selected file changed during comparison. Refresh and try again.".into());
    }
    Ok(())
}

fn compare_session(
    session: &Session,
    requested_ids: &[InventoryItemId],
    policy: &SummaryExclusionPolicy,
    _revision: RevisionId,
    cancellation: &AtomicBool,
    started: Instant,
    duration: Duration,
) -> Result<Comparison, String> {
    let duration = duration.min(duplicate_content::MAX_DURATION);
    let check = || -> Result<(), String> {
        check_summary_cancellation(cancellation)?;
        if started.elapsed() >= duration {
            return Err("Duplicate-content inspection exceeded its cooperative time limit.".into());
        }
        Ok(())
    };
    check()?;
    if session.observation_id != session.snapshot.observation_id {
        return Err("Folder observation is stale. Inspect it again.".into());
    }
    if !(2..=duplicate_content::MAX_FILES).contains(&requested_ids.len()) {
        return Err("Select between 2 and 32 observed files to compare.".into());
    }
    let mut unique = std::collections::HashSet::with_capacity(requested_ids.len());
    if requested_ids.iter().any(|id| !unique.insert(*id)) {
        return Err("Each selected file must be unique.".into());
    }
    validate_session_bindings(session, started, duration).map_err(str::to_owned)?;
    let frame = session
        .frames
        .last()
        .ok_or("The current folder observation is unavailable.")?;
    if session.frames.iter().skip(1).any(|frame| {
        frame
            .opened_from
            .as_ref()
            .is_some_and(|(name, _)| is_managed_boundary(name))
    }) {
        return Err("Files inside a managed folder boundary cannot be compared.".into());
    }
    super::duplicate_scope::ensure_allowed(&frame.fd, started, duration).map_err(str::to_owned)?;
    policy
        .ensure_ancestor_scope_allowed(&frame.fd, started, duration)
        .map_err(str::to_owned)?;

    let mut selected = Vec::with_capacity(requested_ids.len());
    let mut identities = std::collections::HashSet::with_capacity(requested_ids.len());
    let mut input_bytes = 0u64;
    for id in requested_ids {
        check()?;
        let observed = session
            .entries
            .iter()
            .find(|entry| entry.id == *id)
            .ok_or("A selected file is no longer in the current observation.")?;
        let display = session
            .snapshot
            .entries
            .iter()
            .find(|entry| entry.id == *id)
            .ok_or("A selected file is no longer in the current observation.")?;
        if observed.file_type != FileType::RegularFile
            || display.kind != "file"
            || display.availability == "dataless"
            || observed.name.first() == Some(&b'.')
            || is_managed_boundary(&observed.name)
            || display.logical_bytes.is_none()
        {
            return Err("Every selected item must be a visible, ordinary observed file.".into());
        }
        if observed.identity.device != session.root_device {
            return Err("Cross-volume files cannot be compared.".into());
        }
        if !identities.insert((observed.identity.device, observed.identity.inode)) {
            return Err("Selected names refer to the same file object.".into());
        }
        let observed_size = display
            .logical_bytes
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or("An observed file size is unavailable.")?;
        if observed_size > duplicate_content::MAX_FILE_BYTES {
            return Err("A selected file exceeds the 16 MiB comparison limit.".into());
        }
        input_bytes = input_bytes
            .checked_add(observed_size)
            .ok_or("Selected files exceed the comparison size limit.")?;
        if input_bytes > duplicate_content::MAX_INPUT_BYTES {
            return Err("Selected files exceed the 64 MiB comparison limit.".into());
        }
        let name = CString::new(observed.name.as_slice())
            .map_err(|_| "A selected file name cannot be represented safely.")?;
        check()?;
        let named = statat(&frame.fd, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| "A selected file changed. Refresh the observation and try again.")?;
        validate_file_stat(&named, observed, observed_size, session.root_device)?;
        check()?;
        let fd = openat(
            &frame.fd,
            name.as_c_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| "A selected file could not be opened safely.")?;
        check()?;
        let held = fstat(&fd).map_err(|_| "A selected file changed. Refresh and try again.")?;
        validate_file_stat(&held, observed, observed_size, session.root_device)?;
        if !root_metadata_matches(&named, &held) {
            return Err(
                "A selected file changed while it was opened. Refresh and try again.".into(),
            );
        }
        let stamp = super::checkpoint_replay::stamp(&held).map_err(str::to_owned)?;
        selected.push(SelectedFile {
            id: *id,
            name,
            fd,
            stamp,
            size: observed_size,
        });
    }

    // The complete set is opened and validated before reading any file content.
    check()?;
    validate_session_bindings(session, started, duration).map_err(str::to_owned)?;
    super::duplicate_scope::ensure_allowed(&frame.fd, started, duration).map_err(str::to_owned)?;
    policy
        .ensure_ancestor_scope_allowed(&frame.fd, started, duration)
        .map_err(str::to_owned)?;
    let mut budget = ReadBudget::new(cancellation, started, duration);
    let mut fingerprints = BTreeMap::<(u64, [u8; 32]), Vec<usize>>::new();
    for (index, file) in selected.iter_mut().enumerate() {
        check()?;
        validate_selected(file, &frame.fd)?;
        check()?;
        let mut reader = File::from(
            file.fd
                .try_clone()
                .map_err(|_| "A selected file could not be read safely.")?,
        );
        // Cloned descriptors share offsets on Unix, so rewind the retained descriptor first.
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|_| "A selected file could not be read safely.")?;
        let digest = duplicate_content::fingerprint(&mut reader, file.size, &mut budget)
            .map_err(str::to_owned)?;
        fingerprints
            .entry((file.size, digest))
            .or_default()
            .push(index);
    }

    let mut groups = Vec::new();
    for ((size, _digest), members) in fingerprints {
        check()?;
        if members.len() < 2 {
            continue;
        }
        let representative = members[0];
        for member in members.iter().skip(1) {
            check()?;
            validate_selected(&selected[representative], &frame.fd)?;
            validate_selected(&selected[*member], &frame.fd)?;
            check()?;
            let left_fd = selected[representative]
                .fd
                .try_clone()
                .map_err(|_| "A selected file could not be read safely.")?;
            let right_fd = selected[*member]
                .fd
                .try_clone()
                .map_err(|_| "A selected file could not be read safely.")?;
            let mut left = File::from(left_fd);
            let mut right = File::from(right_fd);
            left.seek(SeekFrom::Start(0))
                .map_err(|_| "A selected file could not be read safely.")?;
            right
                .seek(SeekFrom::Start(0))
                .map_err(|_| "A selected file could not be read safely.")?;
            if !duplicate_content::equal(&mut left, &mut right, size, &mut budget)
                .map_err(str::to_owned)?
            {
                return Err("Files sharing a content fingerprint did not compare consistently. Retry the comparison.".into());
            }
        }
        groups.push(DuplicateGroup {
            id: DuplicateGroupId::new(),
            entry_ids: members.iter().map(|index| selected[*index].id).collect(),
            logical_bytes: size.to_string(),
        });
    }

    // Recheck every held descriptor and every name binding before publishing any result.
    check()?;
    validate_session_bindings(session, started, duration).map_err(str::to_owned)?;
    for file in &selected {
        check()?;
        validate_selected(file, &frame.fd)?;
    }
    super::duplicate_scope::ensure_allowed(&frame.fd, started, duration).map_err(str::to_owned)?;
    policy
        .ensure_ancestor_scope_allowed(&frame.fd, started, duration)
        .map_err(str::to_owned)?;
    validate_session_bindings(session, started, duration).map_err(str::to_owned)?;
    check()?;
    Ok(Comparison {
        observation_id: session.observation_id,
        comparison_id: InventoryItemId::new(),
        detector_version: "exact-regular-file-v1".to_owned(),
        files_compared: u32::try_from(selected.len())
            .map_err(|_| "Selected file count exceeded its supported range.")?,
        bytes_read: budget.bytes_read().to_string(),
        groups,
    })
}

fn retain_after_success(
    session: &mut Session,
    comparison: &Comparison,
    exclusion_revision: RevisionId,
) -> Result<(), &'static str> {
    use disk_steward_plan_model::{
        Blocker, Choice, DuplicateSet, EvidenceRef, InventoryItem, ItemKind, Suggestion,
    };

    if comparison.observation_id != session.observation_id
        || session.snapshot.observation_id != session.observation_id
        || comparison.groups.len() > 16
        || !(2..=32).contains(&comparison.files_compared)
        || comparison.detector_version != "exact-regular-file-v1"
    {
        return Err("Duplicate comparison could not be retained for this observation.");
    }
    let observation_id = serde_json::to_value(session.observation_id)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or("Duplicate comparison could not be retained for this observation.")?;
    let comparison_id = serde_json::to_value(comparison.comparison_id)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or("Duplicate comparison could not be retained for this observation.")?;
    let mut items = Vec::new();
    let mut duplicate_sets = Vec::with_capacity(comparison.groups.len());
    let mut retained_ids = std::collections::HashSet::new();
    for group in &comparison.groups {
        if !(2..=32).contains(&group.entry_ids.len()) {
            return Err("Duplicate comparison contained an invalid group.");
        }
        let group_size = group
            .logical_bytes
            .parse::<u64>()
            .map_err(|_| "Duplicate comparison contained an invalid size.")?;
        for entry_id in &group.entry_ids {
            if !retained_ids.insert(*entry_id)
                || retained_ids.len() > comparison.files_compared as usize
            {
                return Err("Duplicate comparison contained overlapping groups.");
            }
            let entry = session
                .snapshot
                .entries
                .iter()
                .find(|entry| entry.id == *entry_id)
                .ok_or("Duplicate comparison no longer matches the observation.")?;
            let observed = session
                .entries
                .iter()
                .find(|entry| entry.id == *entry_id)
                .ok_or("Duplicate comparison no longer matches the observation.")?;
            if entry.kind != "file"
                || observed.file_type != FileType::RegularFile
                || observed.identity.device != session.root_device
                || observed.name.first() == Some(&b'.')
                || observed.content_stamp.is_none()
                || entry
                    .logical_bytes
                    .as_deref()
                    .and_then(|value| value.parse::<u64>().ok())
                    != Some(group_size)
            {
                return Err("Duplicate comparison no longer matches the observed files.");
            }
            let evidence = format!(
                "Selected-file comparison {comparison_id}, detector {}, observation {observation_id}: exact full-content match for {} files was verified at comparison time. Logical bytes per file: {}. This is historical evidence; import requires unchanged native metadata, without rereading contents; later changes remain unknown. APFS physical deduplication and reclaimable space are unknown. Cleanup eligibility is not established and no execution adapter is available.",
                comparison.detector_version,
                group.entry_ids.len(),
                group.logical_bytes,
            );
            if evidence.len() > 2048 {
                return Err("Duplicate comparison evidence exceeded its size limit.");
            }
            items.push(InventoryItem {
                id: *entry_id,
                parent_id: None,
                label: entry.name.clone(),
                kind: ItemKind::File,
                logical_bytes: Some(group_size),
                recovery_lower_bytes: None,
                recovery_upper_bytes: None,
                storage_identity: None,
                blockers: vec![
                    Blocker {
                        code: "read_only_comparison".to_owned(),
                        reason: "The duplicate match is evidence for review only; it does not establish that either copy is safe to remove.".to_owned(),
                    },
                    Blocker {
                        code: "no_execution_adapter".to_owned(),
                        reason: "No native cleanup adapter is available for this item.".to_owned(),
                    },
                ],
                evidence: vec![EvidenceRef {
                    id: disk_steward_core::EvidenceId::new(),
                    summary: evidence,
                }],
                suggestion: Suggestion {
                    choice: Choice::Keep,
                    rationale: "Keep by default; a content match does not establish cleanup safety.".to_owned(),
                },
            });
        }
        duplicate_sets.push(DuplicateSet {
            id: group.id,
            item_ids: group.entry_ids.clone(),
            survivor_id: None,
        });
    }
    session.duplicate_evidence = Some(RetainedComparison {
        comparison: comparison.clone(),
        exclusion_revision,
        created_at: Instant::now(),
        items,
        duplicate_sets,
    });
    Ok(())
}

pub(super) fn review_bundle(
    session: &Session,
    expected_observation_id: InventoryItemId,
    comparison_id: InventoryItemId,
    current_policy: &SummaryExclusionPolicy,
    revision: RevisionId,
    started: Instant,
    duration: Duration,
) -> Result<
    (
        Vec<disk_steward_plan_model::InventoryItem>,
        Vec<disk_steward_plan_model::DuplicateSet>,
    ),
    &'static str,
> {
    let check_deadline = || {
        if started.elapsed() >= duration {
            Err("Duplicate evidence review exceeded its time limit.")
        } else {
            Ok(())
        }
    };
    check_deadline()?;
    if session.observation_id != expected_observation_id
        || session.snapshot.observation_id != expected_observation_id
    {
        return Err("Folder observation is stale.");
    }
    let retained = session
        .duplicate_evidence
        .as_ref()
        .ok_or("No retained duplicate comparison is available.")?;
    if retained.comparison.comparison_id != comparison_id
        || retained.comparison.observation_id != expected_observation_id
        || retained.exclusion_revision != revision
        || retained.duplicate_sets.is_empty()
        || retained.items.is_empty()
        || retained.duplicate_sets.len() > 16
        || retained.items.len() > 32
    {
        return Err("Duplicate comparison evidence is stale or unavailable.");
    }
    if started
        .checked_duration_since(retained.created_at)
        .is_none_or(|age| age >= RETAINED_REVIEW_TTL)
    {
        return Err("Duplicate comparison evidence expired. Compare the files again.");
    }
    validate_session_bindings(session, started, duration)?;
    let frame = session
        .frames
        .last()
        .ok_or("The current folder observation is unavailable.")?;
    super::duplicate_scope::ensure_allowed(&frame.fd, started, duration)?;
    current_policy.ensure_ancestor_scope_allowed(&frame.fd, started, duration)?;

    let mut seen = std::collections::HashSet::new();
    for set in &retained.duplicate_sets {
        check_deadline()?;
        if !(2..=32).contains(&set.item_ids.len()) {
            return Err("Retained duplicate group is invalid.");
        }
        for item_id in &set.item_ids {
            check_deadline()?;
            if !seen.insert(*item_id) {
                return Err("Retained duplicate groups overlap.");
            }
            let observed = session
                .entries
                .iter()
                .find(|entry| entry.id == *item_id)
                .ok_or("Duplicate evidence no longer matches the observation.")?;
            let display = session
                .snapshot
                .entries
                .iter()
                .find(|entry| entry.id == *item_id)
                .ok_or("Duplicate evidence no longer matches the observation.")?;
            if display.kind != "file"
                || observed.file_type != FileType::RegularFile
                || observed.identity.device != session.root_device
                || observed.name.first() == Some(&b'.')
                || is_managed_boundary(&observed.name)
                || display.availability == "dataless"
            {
                return Err("Duplicate evidence no longer matches eligible observed files.");
            }
            let name = CString::new(observed.name.as_slice())
                .map_err(|_| "Duplicate evidence name is unavailable.")?;
            let stat = statat(&frame.fd, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| "A duplicate member changed since comparison.")?;
            let current_stamp = super::checkpoint_replay::stamp(&stat)?;
            if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
                || stat.st_dev as u64 != session.root_device
                || identity(&stat) != observed.identity
                || stat.st_nlink != 1
                || is_dataless(&stat)
                || observed.content_stamp.as_ref() != Some(&current_stamp)
            {
                return Err(
                    "A duplicate member changed since comparison. Refresh and compare again.",
                );
            }
        }
    }
    check_deadline()?;
    validate_session_bindings(session, started, duration)?;
    super::duplicate_scope::ensure_allowed(&frame.fd, started, duration)?;
    current_policy.ensure_ancestor_scope_allowed(&frame.fd, started, duration)?;
    check_deadline()?;
    if retained.created_at.elapsed() >= RETAINED_REVIEW_TTL {
        return Err("Duplicate comparison evidence expired. Compare the files again.");
    }
    Ok((retained.items.clone(), retained.duplicate_sets.clone()))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::global_exclusions::SummaryExclusionPolicy;
    use std::os::unix::fs as unix_fs;
    use std::path::Path;

    fn fixture(path: &Path) -> (Session, Vec<InventoryItemId>) {
        let (session, snapshot) = super::super::selected_root_session(
            path,
            super::super::MAX_RAW_ENTRIES,
            super::super::MAX_DURATION,
        )
        .unwrap();
        let ids = snapshot.entries.iter().map(|entry| entry.id).collect();
        (session, ids)
    }

    fn compare(session: &Session, ids: &[InventoryItemId]) -> Result<Comparison, String> {
        compare_session(
            session,
            ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            RevisionId::new(),
            &AtomicBool::new(false),
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
    }

    #[test]
    fn groups_identical_separate_files_and_leaves_sources_unchanged() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("first.txt"), b"same bytes").unwrap();
        std::fs::write(root.path().join("second.txt"), b"same bytes").unwrap();
        std::fs::write(root.path().join("different.txt"), b"other bytes").unwrap();
        let (session, ids) = fixture(root.path());
        let result = compare(&session, &ids).unwrap();
        assert_eq!(result.files_compared, 3);
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].entry_ids.len(), 2);
        assert_eq!(
            std::fs::read(root.path().join("first.txt")).unwrap(),
            b"same bytes"
        );
        assert_eq!(
            std::fs::read(root.path().join("second.txt")).unwrap(),
            b"same bytes"
        );
    }

    #[test]
    fn refuses_hidden_symlink_hardlink_and_unknown_ids_before_comparing() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("visible-a"), b"same").unwrap();
        std::fs::write(root.path().join("visible-b"), b"same").unwrap();
        std::fs::write(root.path().join(".hidden"), b"same").unwrap();
        unix_fs::symlink("visible-a", root.path().join("link")).unwrap();
        std::fs::hard_link(root.path().join("visible-a"), root.path().join("alias")).unwrap();
        let (session, ids) = fixture(root.path());
        for name in [".hidden", "link", "alias"] {
            let id = session
                .snapshot
                .entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap()
                .id;
            let requested = vec![ids[0], id];
            assert!(compare(&session, &requested).is_err(), "{name}");
        }
        assert!(compare(&session, &[ids[0], InventoryItemId::new()]).is_err());
        let duplicate = vec![ids[0], ids[0]];
        assert!(compare(&session, &duplicate).is_err());
    }

    #[test]
    fn held_file_revalidation_rejects_growth_and_named_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selected");
        std::fs::write(&path, b"same").unwrap();
        let parent = rustix::fs::open(
            root.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let fd = openat(
            &parent,
            "selected",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let baseline = super::super::checkpoint_replay::stamp(&fstat(&fd).unwrap()).unwrap();
        let selected = SelectedFile {
            id: InventoryItemId::new(),
            name: CString::new("selected").unwrap(),
            fd,
            stamp: baseline,
            size: 4,
        };
        validate_selected(&selected, &parent).unwrap();
        std::fs::write(&path, b"grown").unwrap();
        assert!(validate_selected(&selected, &parent).is_err());
        // Capture the new held baseline, then replace only the named binding.
        let selected = SelectedFile {
            stamp: super::super::checkpoint_replay::stamp(&fstat(&selected.fd).unwrap()).unwrap(),
            ..selected
        };
        std::fs::rename(&path, root.path().join("retained-original")).unwrap();
        std::fs::write(&path, b"grown").unwrap();
        assert!(validate_selected(&selected, &parent).is_err());
        assert_eq!(
            std::fs::read(root.path().join("retained-original")).unwrap(),
            b"grown"
        );
    }

    #[test]
    fn empty_files_match_without_inventing_read_bytes_and_excluded_scope_refuses() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("one"), b"").unwrap();
        std::fs::write(root.path().join("two"), b"").unwrap();
        let (session, ids) = fixture(root.path());
        let result = compare(&session, &ids).unwrap();
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].logical_bytes, "0");
        assert_eq!(result.bytes_read, "0");
        let root_identity = session.frames.last().unwrap().identity;
        let policy =
            SummaryExclusionPolicy::for_tests(vec![(root_identity.device, root_identity.inode)]);
        assert!(compare_session(
            &session,
            &ids,
            &policy,
            RevisionId::new(),
            &AtomicBool::new(false),
            Instant::now(),
            duplicate_content::MAX_DURATION
        )
        .is_err());
        assert!(compare_session(
            &session,
            &ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            RevisionId::new(),
            &AtomicBool::new(true),
            Instant::now(),
            duplicate_content::MAX_DURATION
        )
        .is_err());
    }

    #[test]
    fn same_size_change_since_observation_requires_refresh_before_content_read() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("one"), b"same").unwrap();
        std::fs::write(root.path().join("two"), b"same").unwrap();
        let (session, ids) = fixture(root.path());
        std::fs::write(root.path().join("one"), b"diff").unwrap();
        std::fs::File::open(root.path().join("one"))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1234))
            .unwrap();
        let error = compare(&session, &ids).unwrap_err();
        assert!(error.contains("changed since its observation"));
        assert_eq!(std::fs::read(root.path().join("one")).unwrap(), b"diff");
    }

    #[test]
    fn retained_review_bundle_contains_only_blocked_matched_metadata_and_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("one"), b"same bytes").unwrap();
        std::fs::write(root.path().join("two"), b"same bytes").unwrap();
        std::fs::write(root.path().join("unmatched"), b"other data").unwrap();
        let (mut session, ids) = fixture(root.path());
        let revision = RevisionId::new();
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let result = compare_session(
            &session,
            &ids,
            &policy,
            revision,
            &AtomicBool::new(false),
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
        .unwrap();
        assert_eq!(result.groups.len(), 1);
        retain_after_success(&mut session, &result, revision).unwrap();

        let first = review_bundle(
            &session,
            session.observation_id,
            result.comparison_id,
            &policy,
            revision,
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
        .unwrap();
        let second = review_bundle(
            &session,
            session.observation_id,
            result.comparison_id,
            &policy,
            revision,
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.0.len(), 2);
        assert_eq!(first.1.len(), 1);
        assert_eq!(first.1[0].survivor_id, None);
        for item in &first.0 {
            assert_eq!(
                item.suggestion.choice,
                disk_steward_plan_model::Choice::Keep
            );
            assert_eq!(item.parent_id, None);
            assert_eq!(item.storage_identity, None);
            assert_eq!(item.recovery_lower_bytes, None);
            assert_eq!(item.recovery_upper_bytes, None);
            assert!(item
                .blockers
                .iter()
                .any(|blocker| blocker.code == "read_only_comparison"));
            assert!(item
                .blockers
                .iter()
                .any(|blocker| blocker.code == "no_execution_adapter"));
            let evidence = &item.evidence[0].summary;
            assert!(evidence.contains("exact full-content match"));
            assert!(evidence.contains("APFS physical deduplication"));
            assert!(evidence.len() <= 2048);
            assert!(!evidence.contains(root.path().to_str().unwrap()));
            assert!(!evidence.contains("same bytes"));
            assert!(!evidence.contains("ba7816bf"));
        }
        assert!(review_bundle(
            &session,
            session.observation_id,
            InventoryItemId::new(),
            &policy,
            revision,
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
        .is_err());
        assert!(review_bundle(
            &session,
            session.observation_id,
            result.comparison_id,
            &policy,
            RevisionId::new(),
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
        .is_err());
    }

    #[test]
    fn changed_member_and_successful_refresh_invalidate_retained_proof() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("one"), b"same").unwrap();
        std::fs::write(root.path().join("two"), b"same").unwrap();
        let (mut session, ids) = fixture(root.path());
        let revision = RevisionId::new();
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let result = compare_session(
            &session,
            &ids,
            &policy,
            revision,
            &AtomicBool::new(false),
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
        .unwrap();
        retain_after_success(&mut session, &result, revision).unwrap();
        std::fs::write(root.path().join("one"), b"diff").unwrap();
        assert!(review_bundle(
            &session,
            session.observation_id,
            result.comparison_id,
            &policy,
            revision,
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
        .is_err());

        session.duplicate_evidence = None; // A new explicit attempt revokes the old import proof.
        assert!(compare_session(
            &session,
            &ids,
            &policy,
            revision,
            &AtomicBool::new(false),
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
        .is_err());
        assert!(review_bundle(
            &session,
            session.observation_id,
            result.comparison_id,
            &policy,
            revision,
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
        .is_err());

        let current = session.observation_id;
        super::super::refresh_session(
            &mut session,
            super::super::RefreshInventoryRequest {
                expected_observation_id: current,
            },
            super::super::MAX_RAW_ENTRIES,
            super::super::MAX_DURATION,
        )
        .unwrap();
        assert!(session.duplicate_evidence.is_none());
    }

    #[test]
    fn retained_review_proof_expires_on_monotonic_five_minute_ttl() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("one"), b"same").unwrap();
        std::fs::write(root.path().join("two"), b"same").unwrap();
        let (mut session, ids) = fixture(root.path());
        let revision = RevisionId::new();
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let result = compare_session(
            &session,
            &ids,
            &policy,
            revision,
            &AtomicBool::new(false),
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
        .unwrap();
        retain_after_success(&mut session, &result, revision).unwrap();
        session.duplicate_evidence.as_mut().unwrap().created_at =
            Instant::now() - RETAINED_REVIEW_TTL - Duration::from_secs(1);
        assert!(review_bundle(
            &session,
            session.observation_id,
            result.comparison_id,
            &policy,
            revision,
            Instant::now(),
            duplicate_content::MAX_DURATION,
        )
        .is_err());
    }

    #[test]
    fn rejects_stale_observation_and_renderer_path_fields() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("one"), b"1").unwrap();
        std::fs::write(root.path().join("two"), b"2").unwrap();
        let (mut session, ids) = fixture(root.path());
        session.observation_id = InventoryItemId::new();
        assert!(compare(&session, &ids).is_err());

        let value = serde_json::json!({
            "expectedObservationId": serde_json::to_value(InventoryItemId::new()).unwrap(),
            "entryIds": [serde_json::to_value(InventoryItemId::new()).unwrap()],
            "attemptId": serde_json::to_value(SummaryAttemptId::new()).unwrap(),
            "allowContentRead": true,
            "path": "/tmp/secret"
        });
        assert!(serde_json::from_value::<CompareRequest>(value).is_err());
    }
}

fn validate_file_stat(
    stat: &Stat,
    observed: &super::ObservedEntry,
    observed_size: u64,
    root_device: u64,
) -> Result<(), String> {
    if observed.content_stamp.as_ref()
        != Some(&super::checkpoint_replay::stamp(stat).map_err(str::to_owned)?)
    {
        return Err(
            "A selected file changed since its observation. Refresh before allowing content reads."
                .into(),
        );
    }
    let stat_size =
        u64::try_from(stat.st_size).map_err(|_| "A selected file size is unavailable.")?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_dev as u64 != root_device
        || identity(stat) != observed.identity
        || stat_size != observed_size
        || stat.st_nlink != 1
        || is_dataless(stat)
    {
        return Err("A selected item changed or is not a private regular file.".into());
    }
    Ok(())
}
