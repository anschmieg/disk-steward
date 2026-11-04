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
