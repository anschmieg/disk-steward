//! Bounded format inspection for two explicitly selected pytest cache records.
//! The result describes JSON shape only; it does not establish cache ownership,
//! test idleness, cleanup eligibility, or safe regeneration.

use super::{
    check_summary_cancellation, identity, is_dataless, is_managed_boundary,
    validate_session_bindings, FolderInventoryState, Session,
};
use crate::global_exclusions::{ExclusionState, SummaryExclusionPolicy};
use crate::summary_control::SummaryControlState;
use disk_steward_core::{InventoryItemId, RevisionId, SummaryAttemptId};
use rustix::fs::{fstat, openat, statat, AtFlags, FileType, Mode, OFlags, Stat};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_DURATION: Duration = Duration::from_secs(2);
const MAX_RECORD_BYTES: u64 = disk_steward_generated_evidence::MAX_INPUT_BYTES as u64;
const MAX_TOTAL_RECORD_BYTES: u64 = MAX_RECORD_BYTES * 2;
const RETAINED_REVIEW_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InspectRequest {
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

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecordKind {
    NodeIds,
    LastFailed,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InspectedRecord {
    entry_id: InventoryItemId,
    kind: RecordKind,
    record_count: u32,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Inspection {
    observation_id: InventoryItemId,
    inspection_id: InventoryItemId,
    detector_version: String,
    reference_version: String,
    bytes_read: String,
    records: Vec<InspectedRecord>,
}

pub(super) struct RetainedInspection {
    inspection: Inspection,
    exclusion_revision: RevisionId,
    created_at: Instant,
    items: Vec<disk_steward_plan_model::InventoryItem>,
    stamps: Vec<(InventoryItemId, super::checkpoint_store::RootStamp)>,
}

fn invalidate_retained_inspection(session: &mut Session) {
    // An accepted explicit attempt supersedes prior importable evidence even
    // when that attempt later fails validation, reading, or policy checks.
    session.pytest_evidence = None;
}

#[tauri::command]
pub(crate) async fn inspect_pytest_cache_records(
    folders: tauri::State<'_, FolderInventoryState>,
    control: tauri::State<'_, SummaryControlState>,
    exclusions: tauri::State<'_, ExclusionState>,
    request: InspectRequest,
) -> Result<Inspection, String> {
    if !request.allow_content_read {
        return Err(
            "Explicit content-read consent is required to inspect selected records.".into(),
        );
    }
    let native_guard = crate::NativeCheckGuard::acquire()?;
    let registration = control
        .register(request.expected_observation_id, request.attempt_id)
        .map_err(|_| "A metadata operation is already active.".to_owned())?;
    let sessions = Arc::clone(&folders.0);
    let exclusions = exclusions.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _native_guard = native_guard;
        let _registration = registration;
        let cancellation = _registration.cancellation_flag();
        let started = Instant::now();
        let mut current = sessions
            .lock()
            .map_err(|_| "Folder observation is unavailable.".to_owned())?;
        let session = current
            .as_mut()
            .ok_or("Inspect a project folder before selecting cache records.")?;
        if session.observation_id != request.expected_observation_id
            || session.snapshot.observation_id != request.expected_observation_id
        {
            return Err("Folder observation is stale. Inspect it again.".into());
        }
        invalidate_retained_inspection(session);
        check(cancellation, started)?;
        let (revision, policy) = exclusions
            .summary_policy(started, MAX_DURATION)
            .map_err(str::to_owned)?;
        let inspection = inspect_session(
            session,
            &request.entry_ids,
            &policy,
            cancellation,
            started,
            MAX_DURATION,
        )?;
        check(cancellation, started)?;
        let (after_revision, after_policy) = exclusions
            .summary_policy(started, MAX_DURATION)
            .map_err(str::to_owned)?;
        check(cancellation, started)?;
        if after_revision != revision || after_policy != policy {
            return Err("Remembered exclusions changed during record inspection.".into());
        }
        revalidate_named_records(session, &request.entry_ids, started, cancellation)?;
        validate_session_bindings(session, started, MAX_DURATION).map_err(str::to_owned)?;
        let frame = session
            .frames
            .last()
            .ok_or("The current folder observation is unavailable.")?;
        super::duplicate_scope::ensure_allowed(&frame.fd, started, MAX_DURATION)
            .map_err(str::to_owned)?;
        policy
            .ensure_ancestor_scope_allowed(&frame.fd, started, MAX_DURATION)
            .map_err(str::to_owned)?;
        revalidate_named_records(session, &request.entry_ids, started, cancellation)?;
        validate_session_bindings(session, started, MAX_DURATION).map_err(str::to_owned)?;
        check(cancellation, started)?;
        retain_after_success(session, &inspection, revision).map_err(str::to_owned)?;
        Ok(inspection)
    })
    .await
    .map_err(|_| "Pytest record inspection did not complete reliably.".to_owned())?
}

#[tauri::command]
pub(crate) async fn cancel_pytest_cache_inspection(
    control: tauri::State<'_, SummaryControlState>,
    request: CancelRequest,
) -> Result<(), String> {
    control
        .cancel(request.expected_observation_id, request.attempt_id)
        .map_err(|_| "The pytest record inspection is no longer active.".to_owned())
}

struct SelectedRecord {
    entry_id: InventoryItemId,
    kind: RecordKind,
    name: CString,
    fd: OwnedFd,
    stamp: super::checkpoint_store::RootStamp,
    size: u64,
}

fn check(cancel: &std::sync::atomic::AtomicBool, started: Instant) -> Result<(), String> {
    check_summary_cancellation(cancel)?;
    if started.elapsed() >= MAX_DURATION {
        Err("Pytest record inspection exceeded its time limit.".into())
    } else {
        Ok(())
    }
}

fn inspect_session(
    session: &Session,
    entry_ids: &[InventoryItemId],
    policy: &SummaryExclusionPolicy,
    cancellation: &std::sync::atomic::AtomicBool,
    started: Instant,
    duration: Duration,
) -> Result<Inspection, String> {
    check(cancellation, started)?;
    if session.observation_id != session.snapshot.observation_id {
        return Err("Folder observation is stale. Inspect it again.".into());
    }
    if entry_ids.is_empty() || entry_ids.len() > 2 {
        return Err("Select one or two pytest cache records.".into());
    }
    let mut seen = HashSet::with_capacity(entry_ids.len());
    if entry_ids.iter().any(|id| !seen.insert(*id)) {
        return Err("Each selected cache record must be unique.".into());
    }
    validate_cache_context(session)?;
    validate_session_bindings(session, started, duration)?;
    let frame = session
        .frames
        .last()
        .ok_or("The current folder observation is unavailable.")?;
    super::duplicate_scope::ensure_allowed(&frame.fd, started, duration).map_err(str::to_owned)?;
    policy
        .ensure_ancestor_scope_allowed(&frame.fd, started, duration)
        .map_err(str::to_owned)?;

    let mut selected = Vec::with_capacity(entry_ids.len());
    let mut total = 0u64;
    for entry_id in entry_ids {
        check(cancellation, started)?;
        let observed = session
            .entries
            .iter()
            .find(|entry| entry.id == *entry_id)
            .ok_or("A selected cache record is no longer in the observation.")?;
        let display = session
            .snapshot
            .entries
            .iter()
            .find(|entry| entry.id == *entry_id)
            .ok_or("A selected cache record is no longer in the observation.")?;
        let kind = match observed.name.as_slice() {
            b"nodeids" => RecordKind::NodeIds,
            b"lastfailed" => RecordKind::LastFailed,
            _ => return Err("Only nodeids and lastfailed records can be inspected.".into()),
        };
        if observed.file_type != FileType::RegularFile
            || display.kind != "file"
            || display.availability == "dataless"
            || observed.name.first() == Some(&b'.')
            || observed.identity.device != session.root_device
        {
            return Err(
                "Selected records must be visible regular files on the observed volume.".into(),
            );
        }
        let stamp = observed
            .content_stamp
            .as_ref()
            .ok_or("Selected record metadata is unavailable.")?;
        let size = display
            .logical_bytes
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or("Selected record size is unavailable.")?;
        if size > MAX_RECORD_BYTES || u64::try_from(stamp.size).ok() != Some(size) {
            return Err("A selected cache record exceeds 64 KiB.".into());
        }
        total = total
            .checked_add(size)
            .ok_or("Selected cache records exceed the read limit.")?;
        if total > MAX_TOTAL_RECORD_BYTES {
            return Err("Selected cache records exceed 128 KiB.".into());
        }
        let name = CString::new(observed.name.as_slice())
            .map_err(|_| "A selected cache record name is invalid.")?;
        check(cancellation, started)?;
        let named = statat(&frame.fd, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| "A selected cache record changed. Refresh the observation.")?;
        validate_record_stat(&named, observed, stamp, session.root_device)?;
        check(cancellation, started)?;
        let fd = openat(
            &frame.fd,
            name.as_c_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| "A selected cache record could not be opened safely.")?;
        check(cancellation, started)?;
        let held = fstat(&fd).map_err(|_| "A selected cache record changed during open.")?;
        validate_record_stat(&held, observed, stamp, session.root_device)?;
        if super::checkpoint_replay::stamp(&named).map_err(str::to_owned)?
            != super::checkpoint_replay::stamp(&held).map_err(str::to_owned)?
        {
            return Err("A selected cache record changed during open.".into());
        }
        selected.push(SelectedRecord {
            entry_id: *entry_id,
            kind,
            name,
            fd,
            stamp: stamp.clone(),
            size,
        });
    }

    // Complete all selected-item validation before the first content read.
    validate_session_bindings(session, started, duration).map_err(str::to_owned)?;
    let mut records = Vec::with_capacity(selected.len());
    let mut bytes_read = 0u64;
    for record in &selected {
        check(cancellation, started)?;
        validate_record_binding(record, &frame.fd)?;
        check(cancellation, started)?;
        let mut reader = File::from(
            record
                .fd
                .try_clone()
                .map_err(|_| "A selected cache record could not be read safely.")?,
        );
        let capacity = usize::try_from(record.size)
            .map_err(|_| "A selected cache record exceeds its size limit.")?;
        let mut bytes = Vec::with_capacity(capacity);
        let mut buffer = [0u8; 8192];
        while (bytes.len() as u64) < record.size {
            check(cancellation, started)?;
            let remaining = record.size.saturating_sub(bytes.len() as u64);
            let request = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| "A selected cache record exceeds its size limit.")?;
            let count = reader
                .read(&mut buffer[..request])
                .map_err(|_| "A selected cache record could not be read safely.")?;
            check(cancellation, started)?;
            if count == 0 {
                return Err("A selected cache record ended before its observed size.".into());
            }
            bytes_read = bytes_read
                .checked_add(count as u64)
                .ok_or("Selected cache records exceeded the read limit.")?;
            if bytes_read > MAX_TOTAL_RECORD_BYTES + 2 {
                return Err("Selected cache records exceeded the read limit.".into());
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        let mut probe = [0u8; 1];
        check(cancellation, started)?;
        let extra = reader
            .read(&mut probe)
            .map_err(|_| "A selected cache record could not be read safely.")?;
        check(cancellation, started)?;
        bytes_read = bytes_read
            .checked_add(extra as u64)
            .ok_or("Selected cache records exceeded the read limit.")?;
        if bytes_read > MAX_TOTAL_RECORD_BYTES + 2 || extra != 0 {
            return Err("A selected cache record changed length during inspection.".into());
        }
        let record_count = validate_record_json(record.kind, &bytes)?;
        drop(bytes); // Raw cache contents are discarded before metadata revalidation.
        check(cancellation, started)?;
        validate_record_binding(record, &frame.fd)?;
        records.push(InspectedRecord {
            entry_id: record.entry_id,
            kind: record.kind,
            record_count,
        });
    }
    validate_session_bindings(session, started, duration).map_err(str::to_owned)?;
    super::duplicate_scope::ensure_allowed(&frame.fd, started, duration).map_err(str::to_owned)?;
    policy
        .ensure_ancestor_scope_allowed(&frame.fd, started, duration)
        .map_err(str::to_owned)?;
    for record in &selected {
        check(cancellation, started)?;
        validate_record_binding(record, &frame.fd)?;
    }
    validate_session_bindings(session, started, duration).map_err(str::to_owned)?;
    check(cancellation, started)?;
    Ok(Inspection {
        observation_id: session.observation_id,
        inspection_id: InventoryItemId::new(),
        detector_version: disk_steward_generated_evidence::DETECTOR_VERSION.to_owned(),
        reference_version: disk_steward_generated_evidence::REFERENCE_PYTEST_VERSION.to_owned(),
        bytes_read: bytes_read.to_string(),
        records,
    })
}

fn validate_cache_context(session: &Session) -> Result<(), String> {
    let suffix = [b".pytest_cache".as_slice(), b"v", b"cache"];
    if session.frames.len() < suffix.len() + 1 {
        return Err(
            "Select a project root and navigate explicitly to .pytest_cache/v/cache.".into(),
        );
    }
    let tail = &session.frames[session.frames.len() - suffix.len()..];
    for (frame, expected) in tail.iter().zip(suffix) {
        let actual = frame
            .opened_from
            .as_ref()
            .map(|(name, _)| name.as_slice())
            .ok_or("Cache scope is not bound to explicit project navigation.")?;
        if actual != expected {
            return Err(
                "Select records only inside explicitly navigated .pytest_cache/v/cache.".into(),
            );
        }
    }
    if session.frames.iter().skip(1).any(|frame| {
        frame
            .opened_from
            .as_ref()
            .is_some_and(|(name, _)| is_managed_boundary(name))
    }) {
        return Err("Cache records inside a managed folder boundary cannot be inspected.".into());
    }
    Ok(())
}

fn validate_record_stat(
    stat: &Stat,
    observed: &super::ObservedEntry,
    expected_stamp: &super::checkpoint_store::RootStamp,
    root_device: u64,
) -> Result<(), String> {
    if super::checkpoint_replay::stamp(stat).map_err(str::to_owned)? != *expected_stamp
        || FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_dev as u64 != root_device
        || identity(stat) != observed.identity
        || stat.st_nlink != 1
        || is_dataless(stat)
    {
        return Err("A selected cache record changed or is not a private regular file.".into());
    }
    Ok(())
}

fn validate_record_binding(record: &SelectedRecord, parent: &OwnedFd) -> Result<(), String> {
    let held =
        fstat(&record.fd).map_err(|_| "A selected cache record changed during inspection.")?;
    let named = statat(parent, record.name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| "A selected cache record changed during inspection.")?;
    if is_dataless(&held)
        || is_dataless(&named)
        || super::checkpoint_replay::stamp(&held).map_err(str::to_owned)? != record.stamp
        || super::checkpoint_replay::stamp(&named).map_err(str::to_owned)? != record.stamp
    {
        return Err("A selected cache record changed during inspection.".into());
    }
    Ok(())
}

fn revalidate_named_records(
    session: &Session,
    entry_ids: &[InventoryItemId],
    started: Instant,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    validate_cache_context(session)?;
    validate_session_bindings(session, started, MAX_DURATION).map_err(str::to_owned)?;
    let frame = session
        .frames
        .last()
        .ok_or("The current folder observation is unavailable.")?;
    for entry_id in entry_ids {
        check(cancellation, started)?;
        let observed = session
            .entries
            .iter()
            .find(|entry| entry.id == *entry_id)
            .ok_or("A selected cache record is no longer in the observation.")?;
        let display = session
            .snapshot
            .entries
            .iter()
            .find(|entry| entry.id == *entry_id)
            .ok_or("A selected cache record is no longer in the observation.")?;
        if !matches!(observed.name.as_slice(), b"nodeids" | b"lastfailed")
            || observed.file_type != FileType::RegularFile
            || display.kind != "file"
            || display.availability == "dataless"
            || observed.identity.device != session.root_device
        {
            return Err("A selected cache record is no longer eligible for inspection.".into());
        }
        let name = CString::new(observed.name.as_slice())
            .map_err(|_| "A selected cache record name is invalid.")?;
        let expected_stamp = observed
            .content_stamp
            .as_ref()
            .ok_or("Selected record metadata is unavailable.")?;
        let expected_size = display
            .logical_bytes
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or("Selected record size is unavailable.")?;
        if u64::try_from(expected_stamp.size).ok() != Some(expected_size)
            || expected_size > MAX_RECORD_BYTES
        {
            return Err("A selected cache record no longer matches its observed size.".into());
        }
        let named = statat(&frame.fd, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| "A selected cache record changed during inspection.")?;
        validate_record_stat(&named, observed, expected_stamp, session.root_device)?;
        check(cancellation, started)?;
    }
    validate_session_bindings(session, started, MAX_DURATION).map_err(str::to_owned)?;
    Ok(())
}

fn validate_record_json(kind: RecordKind, bytes: &[u8]) -> Result<u32, String> {
    let classifier_kind = match kind {
        RecordKind::NodeIds => disk_steward_generated_evidence::RecordKind::NodeIds,
        RecordKind::LastFailed => disk_steward_generated_evidence::RecordKind::LastFailed,
    };
    disk_steward_generated_evidence::classify_pytest_record(classifier_kind, bytes)
        .map(|evidence| evidence.record_count)
        .map_err(|_| "Selected cache record has an unsupported format.".to_owned())
}

fn retain_after_success(
    session: &mut Session,
    inspection: &Inspection,
    exclusion_revision: RevisionId,
) -> Result<(), &'static str> {
    use disk_steward_plan_model::{
        Blocker, Choice, EvidenceRef, InventoryItem, ItemKind, Suggestion,
    };

    if inspection.observation_id != session.observation_id
        || session.snapshot.observation_id != session.observation_id
        || inspection.detector_version != disk_steward_generated_evidence::DETECTOR_VERSION
        || inspection.reference_version != disk_steward_generated_evidence::REFERENCE_PYTEST_VERSION
        || inspection.records.is_empty()
        || inspection.records.len() > 2
        || inspection
            .bytes_read
            .parse::<u64>()
            .ok()
            .is_none_or(|bytes| bytes > MAX_TOTAL_RECORD_BYTES + 2)
    {
        return Err("Pytest inspection could not be retained for this observation.");
    }
    let inspection_text = serde_json::to_value(inspection.inspection_id)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or("Pytest inspection could not be retained for this observation.")?;
    let observation_text = serde_json::to_value(inspection.observation_id)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or("Pytest inspection could not be retained for this observation.")?;
    let mut seen = HashSet::new();
    let mut items = Vec::with_capacity(inspection.records.len());
    let mut stamps = Vec::with_capacity(inspection.records.len());
    for record in &inspection.records {
        if !seen.insert(record.entry_id) || record.record_count > 512 {
            return Err("Pytest inspection contains invalid record evidence.");
        }
        let observed = session
            .entries
            .iter()
            .find(|entry| entry.id == record.entry_id)
            .ok_or("Pytest inspection no longer matches the observation.")?;
        let display = session
            .snapshot
            .entries
            .iter()
            .find(|entry| entry.id == record.entry_id)
            .ok_or("Pytest inspection no longer matches the observation.")?;
        let expected_name: &[u8] = match record.kind {
            RecordKind::NodeIds => b"nodeids",
            RecordKind::LastFailed => b"lastfailed",
        };
        if observed.name.as_slice() != expected_name
            || observed.file_type != FileType::RegularFile
            || observed.identity.device != session.root_device
            || display.kind != "file"
            || display.availability == "dataless"
            || display.link_count.as_deref() != Some("1")
        {
            return Err("Pytest inspection no longer matches eligible cache records.");
        }
        let stamp = observed
            .content_stamp
            .clone()
            .ok_or("Pytest record metadata is unavailable.")?;
        let size = display
            .logical_bytes
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|size| {
                *size <= MAX_RECORD_BYTES && i64::try_from(*size).ok() == Some(stamp.size)
            })
            .ok_or("Pytest record size is unavailable.")?;
        let kind_label = match record.kind {
            RecordKind::NodeIds => "nodeids",
            RecordKind::LastFailed => "lastfailed",
        };
        let summary = format!(
            "Native inspection {inspection_text} for observation {observation_text}. {} Selected pytest {kind_label} record parsed with detector {} using reference pytest {}; {} record entries were counted. This structural format evidence does not establish the installed pytest version, project/cache ownership, active-test state, regenerability, or cleanup eligibility. It is historical as of inspection; importing does not refresh the cache, and later changes remain unknown. Record contents were discarded after explicit inspection. No content digest or path is retained.",
            super::observation_window_evidence(&session.snapshot.observation_window),
            inspection.detector_version,
            inspection.reference_version,
            record.record_count,
        );
        if summary.len() > 2048 {
            return Err("Pytest inspection evidence exceeded its size limit.");
        }
        items.push(InventoryItem {
            id: record.entry_id,
            parent_id: None,
            label: display.name.clone(),
            kind: ItemKind::File,
            logical_bytes: Some(size),
            recovery_lower_bytes: None,
            recovery_upper_bytes: None,
            storage_identity: None,
            blockers: vec![
                Blocker {
                    code: "read_only_pytest".to_owned(),
                    reason: "Pytest record-format evidence supports review only and does not establish that this cache record is safe to remove.".to_owned(),
                },
                Blocker {
                    code: "pytest_ownership_unverified".to_owned(),
                    reason: "The project and cache ownership have not been verified.".to_owned(),
                },
                Blocker {
                    code: "pytest_active_use_unknown".to_owned(),
                    reason: "Whether tests or another process are using this cache is unknown.".to_owned(),
                },
                Blocker {
                    code: "pytest_regeneration_unverified".to_owned(),
                    reason: "A safe regeneration path has not been verified.".to_owned(),
                },
                Blocker {
                    code: "no_execution_adapter".to_owned(),
                    reason: "No native cleanup adapter is available for this item.".to_owned(),
                },
            ],
            evidence: vec![EvidenceRef {
                id: disk_steward_core::EvidenceId::new(),
                summary,
            }],
            suggestion: Suggestion {
                choice: Choice::Keep,
                rationale: "Keep by default; format recognition does not establish cleanup safety.".to_owned(),
            },
        });
        stamps.push((record.entry_id, stamp));
    }
    session.pytest_evidence = Some(RetainedInspection {
        inspection: inspection.clone(),
        exclusion_revision,
        created_at: Instant::now(),
        items,
        stamps,
    });
    Ok(())
}

pub(super) fn review_bundle(
    session: &Session,
    expected_observation_id: InventoryItemId,
    inspection_id: InventoryItemId,
    current_policy: &SummaryExclusionPolicy,
    revision: RevisionId,
    started: Instant,
    duration: Duration,
) -> Result<Vec<disk_steward_plan_model::InventoryItem>, &'static str> {
    let check_deadline = || {
        if started.elapsed() >= duration {
            Err("Pytest review evidence exceeded its time limit.")
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
        .pytest_evidence
        .as_ref()
        .ok_or("No retained pytest inspection is available.")?;
    if retained.inspection.inspection_id != inspection_id
        || retained.inspection.observation_id != expected_observation_id
        || retained.exclusion_revision != revision
        || retained.items.is_empty()
        || retained.items.len() > 2
        || retained.items.len() != retained.inspection.records.len()
        || retained.stamps.len() != retained.items.len()
    {
        return Err("Pytest inspection evidence is stale or unavailable.");
    }
    if started
        .checked_duration_since(retained.created_at)
        .is_none_or(|age| age >= RETAINED_REVIEW_TTL)
    {
        return Err("Pytest inspection evidence expired. Inspect the records again.");
    }
    validate_cache_context(session).map_err(|_| "Pytest cache context changed.")?;
    validate_session_bindings(session, started, duration)?;
    let frame = session
        .frames
        .last()
        .ok_or("The current folder observation is unavailable.")?;
    super::duplicate_scope::ensure_allowed(&frame.fd, started, duration)?;
    current_policy.ensure_ancestor_scope_allowed(&frame.fd, started, duration)?;

    let validate_retained_names = || -> Result<(), &'static str> {
        for ((entry_id, expected_stamp), record) in retained
            .stamps
            .iter()
            .zip(retained.inspection.records.iter())
        {
            check_deadline()?;
            if *entry_id != record.entry_id {
                return Err("Retained pytest inspection metadata is inconsistent.");
            }
            let observed = session
                .entries
                .iter()
                .find(|entry| entry.id == *entry_id)
                .ok_or("Pytest evidence no longer matches the observation.")?;
            let display = session
                .snapshot
                .entries
                .iter()
                .find(|entry| entry.id == *entry_id)
                .ok_or("Pytest evidence no longer matches the observation.")?;
            let expected_name: &[u8] = match record.kind {
                RecordKind::NodeIds => b"nodeids",
                RecordKind::LastFailed => b"lastfailed",
            };
            let item = retained
                .items
                .iter()
                .find(|item| item.id == *entry_id)
                .ok_or("Retained pytest item metadata is inconsistent.")?;
            if observed.name.as_slice() != expected_name
                || observed.file_type != FileType::RegularFile
                || observed.identity.device != session.root_device
                || display.kind != "file"
                || display.availability == "dataless"
                || display.link_count.as_deref() != Some("1")
                || observed.content_stamp.as_ref() != Some(expected_stamp)
                || item.label != display.name
                || item.parent_id.is_some()
                || item.logical_bytes
                    != display
                        .logical_bytes
                        .as_deref()
                        .and_then(|value| value.parse::<u64>().ok())
                || item.kind != disk_steward_plan_model::ItemKind::File
                || item.storage_identity.is_some()
                || item.recovery_lower_bytes.is_some()
                || item.recovery_upper_bytes.is_some()
                || item.suggestion.choice != disk_steward_plan_model::Choice::Keep
            {
                return Err("Pytest evidence no longer matches eligible observed records.");
            }
            let name = CString::new(observed.name.as_slice())
                .map_err(|_| "Pytest record name is unavailable.")?;
            let stat = statat(&frame.fd, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| "A pytest record changed since inspection.")?;
            if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
                || stat.st_dev as u64 != session.root_device
                || identity(&stat) != observed.identity
                || stat.st_nlink != 1
                || is_dataless(&stat)
                || super::checkpoint_replay::stamp(&stat)
                    .map_err(|_| "Pytest record metadata is unavailable.")?
                    != *expected_stamp
            {
                return Err("A pytest record changed since inspection. Inspect it again.");
            }
        }
        Ok(())
    };
    validate_retained_names()?;
    check_deadline()?;
    validate_session_bindings(session, started, duration)?;
    super::duplicate_scope::ensure_allowed(&frame.fd, started, duration)?;
    current_policy.ensure_ancestor_scope_allowed(&frame.fd, started, duration)?;
    validate_retained_names()?;
    validate_session_bindings(session, started, duration)?;
    check_deadline()?;
    if retained.created_at.elapsed() >= RETAINED_REVIEW_TTL {
        return Err("Pytest inspection evidence expired. Inspect the records again.");
    }
    Ok(retained.items.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_rejects_paths_unknown_fields_and_missing_consent() {
        let observation = serde_json::to_value(InventoryItemId::new()).unwrap();
        let attempt = serde_json::to_value(SummaryAttemptId::new()).unwrap();
        let id = serde_json::to_value(InventoryItemId::new()).unwrap();
        let mut value = serde_json::json!({
            "expectedObservationId": observation,
            "entryIds": [id],
            "attemptId": attempt,
            "allowContentRead": true
        });
        value["path"] = serde_json::json!("/private/user/project");
        assert!(serde_json::from_value::<InspectRequest>(value).is_err());

        let missing_consent = serde_json::json!({
            "expectedObservationId": serde_json::to_value(InventoryItemId::new()).unwrap(),
            "entryIds": [serde_json::to_value(InventoryItemId::new()).unwrap()],
            "attemptId": serde_json::to_value(SummaryAttemptId::new()).unwrap()
        });
        assert!(serde_json::from_value::<InspectRequest>(missing_consent).is_err());

        let extra_cancel = serde_json::json!({
            "expectedObservationId": serde_json::to_value(InventoryItemId::new()).unwrap(),
            "attemptId": serde_json::to_value(SummaryAttemptId::new()).unwrap(),
            "entryIds": []
        });
        assert!(serde_json::from_value::<CancelRequest>(extra_cancel).is_err());
    }

    #[test]
    fn classifier_contract_reports_only_shape_counts_and_generated_reference() {
        let nodeids = disk_steward_generated_evidence::classify_pytest_record(
            disk_steward_generated_evidence::RecordKind::NodeIds,
            include_bytes!("../../../probes/generated_cache/fixtures/pytest-9.2.0/nodeids.json"),
        )
        .unwrap();
        let lastfailed = disk_steward_generated_evidence::classify_pytest_record(
            disk_steward_generated_evidence::RecordKind::LastFailed,
            include_bytes!("../../../probes/generated_cache/fixtures/pytest-9.2.0/lastfailed.json"),
        )
        .unwrap();
        assert_eq!(nodeids.detector_version, "pytest-cache-format-v1");
        assert_eq!(nodeids.reference_version, "9.2.0");
        assert!(nodeids.record_count > 0);
        assert!(lastfailed.record_count > 0);
        assert!(validate_record_json(RecordKind::NodeIds, b"{bad json}").is_err());
        assert!(validate_record_json(RecordKind::LastFailed, br#"{"test":false}"#).is_err());
    }

    #[cfg(unix)]
    fn typed_id(id: InventoryItemId) -> String {
        serde_json::to_value(id)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[cfg(unix)]
    fn selected_cache_session(
        project: &std::path::Path,
        nodeids: &[u8],
        lastfailed: &[u8],
        nodeids_is_directory: bool,
    ) -> (Session, Vec<InventoryItemId>) {
        use super::super::{navigate_session, NavigationRequest, MAX_DURATION, MAX_RAW_ENTRIES};

        let cache = project.join(".pytest_cache/v/cache");
        std::fs::create_dir_all(&cache).unwrap();
        if nodeids_is_directory {
            std::fs::create_dir(cache.join("nodeids")).unwrap();
        } else {
            std::fs::write(cache.join("nodeids"), nodeids).unwrap();
        }
        std::fs::write(cache.join("lastfailed"), lastfailed).unwrap();
        std::fs::write(cache.join("local-notes"), b"unknown record").unwrap();
        let (mut session, _) =
            super::super::selected_root_session(project, MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        for name in [".pytest_cache", "v", "cache"] {
            let entry_id = session
                .snapshot
                .entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap()
                .id;
            let expected_observation_id = typed_id(session.observation_id);
            navigate_session(
                &mut session,
                NavigationRequest {
                    expected_observation_id,
                    entry_id: Some(typed_id(entry_id)),
                },
                MAX_RAW_ENTRIES,
                MAX_DURATION,
            )
            .unwrap();
        }
        let ids = session
            .snapshot
            .entries
            .iter()
            .filter(|entry| entry.name == "nodeids" || entry.name == "lastfailed")
            .map(|entry| entry.id)
            .collect();
        (session, ids)
    }

    #[cfg(target_os = "macos")]
    fn retained_fixture() -> (tempfile::TempDir, Session, Inspection, RevisionId) {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        let nodeids =
            include_bytes!("../../../probes/generated_cache/fixtures/pytest-9.2.0/nodeids.json");
        let lastfailed =
            include_bytes!("../../../probes/generated_cache/fixtures/pytest-9.2.0/lastfailed.json");
        let (mut session, ids) = selected_cache_session(&project, nodeids, lastfailed, false);
        let revision = RevisionId::new();
        let inspection = inspect_session(
            &session,
            &ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            &std::sync::atomic::AtomicBool::new(false),
            Instant::now(),
            Duration::from_secs(5),
        )
        .unwrap();
        retain_after_success(&mut session, &inspection, revision).unwrap();
        (temp, session, inspection, revision)
    }

    #[test]
    fn native_context_requires_exact_raw_navigation_suffix() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        let (mut session, _) = selected_cache_session(&project, b"[]", b"{}", false);
        validate_cache_context(&session).unwrap();
        session
            .frames
            .last_mut()
            .unwrap()
            .opened_from
            .as_mut()
            .unwrap()
            .0 = b"cache-alias".to_vec();
        assert!(validate_cache_context(&session).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_inspection_reads_only_explicit_generated_records_and_returns_counts() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        let nodeids_bytes =
            include_bytes!("../../../probes/generated_cache/fixtures/pytest-9.2.0/nodeids.json");
        let lastfailed_bytes =
            include_bytes!("../../../probes/generated_cache/fixtures/pytest-9.2.0/lastfailed.json");
        let (session, ids) =
            selected_cache_session(&project, nodeids_bytes, lastfailed_bytes, false);
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let result = inspect_session(
            &session,
            &ids,
            &policy,
            &std::sync::atomic::AtomicBool::new(false),
            Instant::now(),
            MAX_DURATION,
        )
        .unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.detector_version, "pytest-cache-format-v1");
        assert_eq!(result.reference_version, "9.2.0");
        assert_eq!(
            result.bytes_read,
            (nodeids_bytes.len() + lastfailed_bytes.len()).to_string()
        );
        let wire = serde_json::to_string(&result).unwrap();
        assert!(!wire.contains("test_generated.py"));
        assert!(!wire.contains(project.to_str().unwrap()));
        assert!(!wire.contains("lastfailed.json"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn retained_pytest_review_bundle_is_stable_blocked_and_metadata_only() {
        let (temp, session, inspection, revision) = retained_fixture();
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let first = review_bundle(
            &session,
            session.observation_id,
            inspection.inspection_id,
            &policy,
            revision,
            Instant::now(),
            Duration::from_secs(30),
        )
        .unwrap();
        let repeated = review_bundle(
            &session,
            session.observation_id,
            inspection.inspection_id,
            &policy,
            revision,
            Instant::now(),
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(first, repeated);
        assert_eq!(first.len(), 2);
        for item in &first {
            assert_eq!(item.parent_id, None);
            assert_eq!(item.kind, disk_steward_plan_model::ItemKind::File);
            assert_eq!(
                item.suggestion.choice,
                disk_steward_plan_model::Choice::Keep
            );
            assert_eq!(item.storage_identity, None);
            assert_eq!(item.recovery_lower_bytes, None);
            assert_eq!(item.recovery_upper_bytes, None);
            for code in [
                "read_only_pytest",
                "pytest_ownership_unverified",
                "pytest_active_use_unknown",
                "pytest_regeneration_unverified",
                "no_execution_adapter",
            ] {
                assert!(item.blockers.iter().any(|blocker| blocker.code == code));
            }
            assert!(item.evidence[0].summary.contains("pytest-cache-format-v1"));
            assert!(item.evidence[0].summary.contains("pytest 9.2.0"));
            assert!(item.evidence[0].summary.contains("active-test state"));
            assert!(item.evidence[0].summary.len() <= 2048);
            assert!(!item.evidence[0]
                .summary
                .contains(temp.path().to_str().unwrap()));
            assert!(!item.evidence[0].summary.contains("test_generated.py"));
            assert!(!item.evidence[0].summary.contains("test_one"));
            assert!(!item.evidence[0].summary.contains("test_two"));
        }
        assert!(review_bundle(
            &session,
            session.observation_id,
            InventoryItemId::new(),
            &policy,
            revision,
            Instant::now(),
            Duration::from_secs(30),
        )
        .is_err());
        assert!(review_bundle(
            &session,
            InventoryItemId::new(),
            inspection.inspection_id,
            &policy,
            revision,
            Instant::now(),
            Duration::from_secs(30),
        )
        .is_err());
        assert!(review_bundle(
            &session,
            session.observation_id,
            inspection.inspection_id,
            &policy,
            RevisionId::new(),
            Instant::now(),
            Duration::from_secs(30),
        )
        .is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn retained_pytest_review_refuses_changed_files_expiry_and_new_failed_attempts() {
        let (temp, mut session, inspection, revision) = retained_fixture();
        let policy = SummaryExclusionPolicy::empty_for_tests();
        let cache = temp.path().join("pytest-project/.pytest_cache/v/cache");
        std::fs::write(cache.join("nodeids"), b"[]").unwrap();
        assert!(review_bundle(
            &session,
            session.observation_id,
            inspection.inspection_id,
            &policy,
            revision,
            Instant::now(),
            Duration::from_secs(30),
        )
        .is_err());

        let (_temp, mut expired_session, expired_inspection, expired_revision) = retained_fixture();
        expired_session.pytest_evidence.as_mut().unwrap().created_at =
            Instant::now() - RETAINED_REVIEW_TTL - Duration::from_secs(1);
        assert!(review_bundle(
            &expired_session,
            expired_session.observation_id,
            expired_inspection.inspection_id,
            &policy,
            expired_revision,
            Instant::now(),
            Duration::from_secs(30),
        )
        .is_err());

        let invalid_ids = vec![InventoryItemId::new()];
        invalidate_retained_inspection(&mut session);
        assert!(inspect_session(
            &session,
            &invalid_ids,
            &policy,
            &std::sync::atomic::AtomicBool::new(false),
            Instant::now(),
            Duration::from_secs(5),
        )
        .is_err());
        assert!(session.pytest_evidence.is_none());
        assert!(review_bundle(
            &session,
            session.observation_id,
            inspection.inspection_id,
            &policy,
            revision,
            Instant::now(),
            Duration::from_secs(30),
        )
        .is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn retained_manager_corpus_import_persists_without_contents_or_execution_authority() {
        use disk_steward_plan_store::{FixtureScale, GeneralPlanStore};
        let (temp, session, inspection, revision) = retained_fixture();
        let bundle = review_bundle(
            &session,
            session.observation_id,
            inspection.inspection_id,
            &SummaryExclusionPolicy::empty_for_tests(),
            revision,
            Instant::now(),
            Duration::from_secs(2),
        )
        .unwrap();
        let path = temp.path().join("review-fixture.sqlite3");
        let mut store = GeneralPlanStore::open(&path, FixtureScale::EmptyReview).unwrap();
        let before = store.get_plan().unwrap();
        let imported = store
            .append_pytest_evidence(before.revision_id, bundle.clone())
            .unwrap();
        assert_eq!(imported.items.len(), 2);
        assert!(!imported.execution_available);
        for row in &imported.items {
            assert_eq!(row.suggestion.choice, disk_steward_plan_model::Choice::Keep);
            assert!(row
                .blockers
                .iter()
                .any(|blocker| blocker.code == "read_only_pytest"));
        }
        assert_eq!(
            store
                .append_pytest_evidence(imported.revision_id, bundle)
                .unwrap(),
            imported
        );
        drop(store);
        let mut reopened = GeneralPlanStore::open(&path, FixtureScale::EmptyReview).unwrap();
        assert_eq!(reopened.get_plan().unwrap(), imported);
        let frozen = reopened.freeze_review(imported.revision_id).unwrap();
        assert!(!frozen.execution_available);
        let serialized = serde_json::to_string(&frozen).unwrap();
        assert!(!serialized.contains("test_generated.py"));
        assert!(!serialized.contains(temp.path().to_str().unwrap()));
        assert!(serialized.contains("record entries were counted"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn retained_import_rechecks_excluded_scope_and_deadline() {
        let (_temp, session, inspection, revision) = retained_fixture();
        let root = identity(&fstat(&session.frames[0].fd).unwrap());
        let excluded = SummaryExclusionPolicy::for_tests(vec![(root.device, root.inode)]);
        assert!(review_bundle(
            &session,
            session.observation_id,
            inspection.inspection_id,
            &excluded,
            revision,
            Instant::now(),
            Duration::from_secs(2)
        )
        .is_err());
        assert!(review_bundle(
            &session,
            session.observation_id,
            inspection.inspection_id,
            &SummaryExclusionPolicy::empty_for_tests(),
            revision,
            Instant::now(),
            Duration::ZERO
        )
        .is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_context_type_format_bounds_and_cancellation_fail_closed() {
        use crate::global_exclusions::SummaryExclusionPolicy;

        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        let (session, ids) = selected_cache_session(&project, b"{invalid", b"{}", false);
        assert!(inspect_session(
            &session,
            &ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            &std::sync::atomic::AtomicBool::new(false),
            Instant::now(),
            MAX_DURATION,
        )
        .is_err());

        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        let (session, ids) = selected_cache_session(&project, b"[]", b"{}", true);
        assert!(inspect_session(
            &session,
            &ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            &std::sync::atomic::AtomicBool::new(false),
            Instant::now(),
            MAX_DURATION,
        )
        .is_err());

        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        let too_large = vec![b' '; disk_steward_generated_evidence::MAX_INPUT_BYTES + 1];
        let (session, ids) = selected_cache_session(&project, &too_large, b"{}", false);
        assert!(inspect_session(
            &session,
            &ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            &std::sync::atomic::AtomicBool::new(false),
            Instant::now(),
            MAX_DURATION,
        )
        .is_err());

        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        let (session, ids) = selected_cache_session(&project, b"[]", b"{}", false);
        let unknown_id = session
            .snapshot
            .entries
            .iter()
            .find(|entry| entry.name == "local-notes")
            .unwrap()
            .id;
        assert!(inspect_session(
            &session,
            &[unknown_id],
            &SummaryExclusionPolicy::empty_for_tests(),
            &std::sync::atomic::AtomicBool::new(false),
            Instant::now(),
            MAX_DURATION,
        )
        .is_err());
        assert!(inspect_session(
            &session,
            &[ids[0], ids[0], ids[1]],
            &SummaryExclusionPolicy::empty_for_tests(),
            &std::sync::atomic::AtomicBool::new(false),
            Instant::now(),
            MAX_DURATION,
        )
        .is_err());
        assert!(inspect_session(
            &session,
            &ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            &std::sync::atomic::AtomicBool::new(true),
            Instant::now(),
            MAX_DURATION,
        )
        .is_err());

        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir_all(project.join("other/v/cache")).unwrap();
        std::fs::write(project.join("other/v/cache/nodeids"), b"[]").unwrap();
        let (session, _) = super::super::selected_root_session(
            &project,
            super::super::MAX_RAW_ENTRIES,
            super::super::MAX_DURATION,
        )
        .unwrap();
        let ids = vec![session.entries[0].id];
        assert!(inspect_session(
            &session,
            &ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            &std::sync::atomic::AtomicBool::new(false),
            Instant::now(),
            MAX_DURATION,
        )
        .is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_same_size_mutation_after_observation_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        let (session, ids) = selected_cache_session(&project, b"[]", b"{}", false);
        let path = project.join(".pytest_cache/v/cache/nodeids");
        std::fs::write(&path, b"{}").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let changed = std::time::SystemTime::now() + Duration::from_secs(2);
        file.set_times(std::fs::FileTimes::new().set_modified(changed))
            .unwrap();
        let error = inspect_session(
            &session,
            &ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            &std::sync::atomic::AtomicBool::new(false),
            Instant::now(),
            MAX_DURATION,
        )
        .unwrap_err();
        assert!(error.contains("changed"), "{error}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_links_exclusions_expiry_and_late_publication_changes_refuse() {
        for replace_with_link in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let project = temp.path().join("pytest-project");
            std::fs::create_dir(&project).unwrap();
            let (session, ids) = selected_cache_session(&project, b"[]", b"{}", false);
            let path = project.join(".pytest_cache/v/cache/nodeids");
            if replace_with_link {
                std::fs::rename(&path, path.with_file_name("sentinel")).unwrap();
                std::os::unix::fs::symlink("sentinel", &path).unwrap();
            } else {
                std::fs::hard_link(&path, path.with_file_name("other-name")).unwrap();
            }
            assert!(inspect_session(
                &session,
                &ids,
                &SummaryExclusionPolicy::empty_for_tests(),
                &std::sync::atomic::AtomicBool::new(false),
                Instant::now(),
                MAX_DURATION
            )
            .is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"[]");
        }
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        let (session, ids) = selected_cache_session(&project, b"[]", b"{}", false);
        let root = identity(&fstat(&session.frames[0].fd).unwrap());
        let excluded = SummaryExclusionPolicy::for_tests(vec![(root.device, root.inode)]);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        assert!(inspect_session(
            &session,
            &ids,
            &excluded,
            &cancel,
            Instant::now(),
            MAX_DURATION
        )
        .is_err());
        assert!(inspect_session(
            &session,
            &ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            &cancel,
            Instant::now() - Duration::from_secs(3),
            MAX_DURATION
        )
        .is_err());
        assert!(inspect_session(
            &session,
            &[ids[0], ids[0]],
            &SummaryExclusionPolicy::empty_for_tests(),
            &cancel,
            Instant::now(),
            MAX_DURATION
        )
        .is_err());
        inspect_session(
            &session,
            &ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            &cancel,
            Instant::now(),
            MAX_DURATION,
        )
        .unwrap();
        std::fs::write(project.join(".pytest_cache/v/cache/nodeids"), b"{} ").unwrap();
        assert!(revalidate_named_records(&session, &ids, Instant::now(), &cancel).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_dataless_flag_refuses_even_when_identity_and_stamp_match() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        let (session, _) = selected_cache_session(&project, b"[]", b"{}", false);
        let observed = session
            .entries
            .iter()
            .find(|entry| entry.name == b"nodeids")
            .unwrap();
        let name = CString::new("nodeids").unwrap();
        let mut stat = statat(
            &session.frames.last().unwrap().fd,
            name.as_c_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .unwrap();
        let stamp = observed.content_stamp.as_ref().unwrap();
        validate_record_stat(&stat, observed, stamp, session.root_device).unwrap();
        stat.st_flags |= super::super::SF_DATALESS;
        assert!(validate_record_stat(&stat, observed, stamp, session.root_device).is_err());
    }

    #[test]
    fn native_classifier_refuses_duplicate_keys_false_values_and_traversal() {
        for bytes in [
            br#"{"a.py::test":true,"a.py::test":true}"#.as_slice(),
            br#"{"a.py::test":false}"#,
            br#"{"../a.py::test":true}"#,
        ] {
            assert!(validate_record_json(RecordKind::LastFailed, bytes).is_err());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_fails_closed_when_native_scope_verification_is_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pytest-project");
        std::fs::create_dir(&project).unwrap();
        let (session, ids) = selected_cache_session(&project, b"[]", b"{}", false);
        assert!(inspect_session(
            &session,
            &ids,
            &SummaryExclusionPolicy::empty_for_tests(),
            &std::sync::atomic::AtomicBool::new(false),
            Instant::now(),
            MAX_DURATION,
        )
        .is_err());
    }
}
