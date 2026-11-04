//! Explicit private recursive checkpoints. Renderer supplies opaque references only.
use super::{
    subtree_checkpoint_store::Store, subtree_replay::ReplayInfo, FolderInventorySnapshot,
    FolderInventoryState,
};
use disk_steward_core::InventoryItemId;
use serde::{Deserialize, Serialize};
use std::{
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::Instant,
};
use tauri::Manager;

#[derive(Clone)]
pub(crate) struct SubtreeCheckpointState(Arc<Mutex<Result<Store, String>>>);
impl SubtreeCheckpointState {
    pub(crate) fn open(app: &tauri::AppHandle) -> Self {
        let result = (|| {
            let base = app
                .path()
                .app_data_dir()
                .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
            let private = super::checkpoint_commands::private_directory(&base)?;
            Store::open(private.join("subtree-v1.sqlite3"))
        })();
        Self(Arc::new(Mutex::new(result)))
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SaveRequest {
    expected_observation_id: InventoryItemId,
    scan_id: InventoryItemId,
    expected_checkpoint_id: Option<InventoryItemId>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CheckpointRequest {
    expected_checkpoint_id: InventoryItemId,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Resumed {
    snapshot: FolderInventorySnapshot,
    progress: super::subtree_job::Progress,
}
#[tauri::command]
pub(crate) async fn get_subtree_checkpoint(
    state: tauri::State<'_, SubtreeCheckpointState>,
) -> Result<Option<ReplayInfo>, String> {
    let guard = crate::NativeCheckGuard::acquire()?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut lock = state
            .0
            .lock()
            .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
        Ok(lock
            .as_mut()
            .map_err(|e| e.clone())?
            .get()?
            .as_ref()
            .map(ReplayInfo::from))
    })
    .await
    .map_err(|_| "Recursive checkpoint inspection did not complete.".to_owned())?
}
#[tauri::command]
pub(crate) async fn save_subtree_checkpoint(
    state: tauri::State<'_, SubtreeCheckpointState>,
    folders: tauri::State<'_, FolderInventoryState>,
    exclusions: tauri::State<'_, crate::global_exclusions::ExclusionState>,
    request: SaveRequest,
) -> Result<ReplayInfo, String> {
    let guard = crate::NativeCheckGuard::acquire()?;
    let state = state.inner().clone();
    let sessions = Arc::clone(&folders.0);
    let exclusions = exclusions.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let started = Instant::now();
        let current = sessions
            .lock()
            .map_err(|_| "Folder observation is unavailable.".to_owned())?;
        let session = current
            .as_ref()
            .ok_or("Choose a folder before saving a scan.")?;
        if session.observation_id != request.expected_observation_id
            || session.snapshot.observation_id != request.expected_observation_id
        {
            return Err("Folder observation is stale.".into());
        }
        super::validate_session_bindings(session, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let job = session
            .subtree_job
            .as_ref()
            .ok_or("No resumable subtree scan is retained.")?;
        if job.id() != request.scan_id || job.observation_id() != request.expected_observation_id {
            return Err("Subtree scan is stale.".into());
        }
        let (revision, policy) = exclusions
            .summary_policy(started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        if revision != job.revision() || &policy != job.policy() {
            return Err("Remembered exclusions changed. Start a fresh scan.".into());
        }
        let frame = session
            .frames
            .last()
            .ok_or("Folder observation is unavailable.")?;
        policy
            .ensure_ancestor_scope_allowed(&frame.fd, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let payload = job
            .capture_replay(started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let (after_revision, after_policy) = exclusions
            .summary_policy(started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        if revision != after_revision || policy != after_policy {
            return Err("Remembered exclusions changed during save.".into());
        }
        super::validate_session_bindings(session, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let mut lock = state
            .0
            .lock()
            .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
        if started.elapsed() >= super::MAX_DURATION {
            return Err("Checkpoint save exceeded its cooperative time limit.".into());
        }
        lock.as_mut()
            .map_err(|e| e.clone())?
            .save(request.expected_checkpoint_id, payload)
    })
    .await
    .map_err(|_| "Recursive checkpoint save did not complete reliably.".to_owned())?
}
#[tauri::command]
pub(crate) async fn resume_subtree_checkpoint(
    app: tauri::AppHandle,
    state: tauri::State<'_, SubtreeCheckpointState>,
    folders: tauri::State<'_, FolderInventoryState>,
    exclusions: tauri::State<'_, crate::global_exclusions::ExclusionState>,
    request: CheckpointRequest,
) -> Result<Option<Resumed>, String> {
    use tauri_plugin_dialog::DialogExt;
    let guard = crate::NativeCheckGuard::acquire()?;
    let state = state.inner().clone();
    let sessions = Arc::clone(&folders.0);
    let exclusions = exclusions.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let payload = {
            let mut lock = state
                .0
                .lock()
                .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
            lock.as_mut()
                .map_err(|e| e.clone())?
                .get()?
                .ok_or("No recursive checkpoint is saved.")?
        };
        if payload.checkpoint_id != request.expected_checkpoint_id {
            return Err("Saved checkpoint changed. Inspect it again.".into());
        }
        let Some(selection) = app
            .dialog()
            .file()
            .set_title("Choose the same folder to revalidate and resume its saved recursive scan")
            .set_can_create_directories(false)
            .blocking_pick_folder()
        else {
            return Ok(None);
        };
        let path = selection
            .into_path()
            .map_err(|_| "Select a supported local folder.".to_owned())?;
        let started = Instant::now();
        let (revision, policy) = exclusions
            .summary_policy(started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        if revision != payload.exclusion_revision {
            return Err("Remembered exclusions changed. Start a fresh scan.".into());
        }
        // Refuse changed or excluded roots before even the ordinary page enumeration.
        let named = rustix::fs::statat(
            rustix::fs::CWD,
            &path,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| "Selected folder could not be verified.".to_owned())?;
        super::refuse_dataless_directory(&named).map_err(str::to_owned)?;
        if super::checkpoint_replay::stamp(&named).map_err(str::to_owned)? != payload.root_stamp {
            return Err("Selected folder differs from the saved scan.".into());
        }
        let fd = rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| "Selected folder could not be verified.".to_owned())?;
        let selected = rustix::fs::fstat(&fd)
            .map_err(|_| "Selected folder could not be verified.".to_owned())?;
        super::refuse_dataless_directory(&selected).map_err(str::to_owned)?;
        if super::checkpoint_replay::stamp(&selected).map_err(str::to_owned)? != payload.root_stamp
        {
            return Err("Selected folder changed during resume.".into());
        }
        policy
            .ensure_ancestor_scope_allowed(&fd, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        // Root selection performs the same no-follow identity and boundary checks as a fresh scan.
        let (mut session, snapshot) = super::selected_root_session(
            &path,
            super::MAX_RAW_ENTRIES,
            super::MAX_DURATION.saturating_sub(started.elapsed()),
        )
        .map_err(str::to_owned)?;
        let (job, progress) = super::subtree_replay::resume(
            &session,
            &payload,
            policy,
            revision,
            &AtomicBool::new(false),
            started,
            super::MAX_DURATION,
        )
        .map_err(str::to_owned)?;
        let (after_revision, after_policy) = exclusions
            .summary_policy(started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        if revision != after_revision || job.policy() != &after_policy {
            return Err("Remembered exclusions changed during resume.".into());
        }
        super::validate_session_bindings(&session, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let mut lock = state
            .0
            .lock()
            .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
        if lock
            .as_mut()
            .map_err(|e| e.clone())?
            .get()?
            .as_ref()
            .map(|p| p.checkpoint_id)
            != Some(payload.checkpoint_id)
        {
            return Err("Saved checkpoint changed during resume.".into());
        }
        if started.elapsed() >= super::MAX_DURATION {
            return Err("Checkpoint replay exceeded its cooperative time limit.".into());
        }
        session.subtree_job = Some(job);
        let mut current = sessions
            .lock()
            .map_err(|_| "Folder observation is unavailable.".to_owned())?;
        *current = Some(session);
        Ok(Some(Resumed { snapshot, progress }))
    })
    .await
    .map_err(|_| "Recursive checkpoint resume did not complete reliably.".to_owned())?
}
#[tauri::command]
pub(crate) async fn clear_subtree_checkpoint(
    state: tauri::State<'_, SubtreeCheckpointState>,
    request: CheckpointRequest,
) -> Result<(), String> {
    let guard = crate::NativeCheckGuard::acquire()?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut lock = state
            .0
            .lock()
            .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
        lock.as_mut()
            .map_err(|e| e.clone())?
            .clear(request.expected_checkpoint_id)
    })
    .await
    .map_err(|_| "Recursive checkpoint removal did not complete reliably.".to_owned())?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renderer_cannot_supply_source_paths_or_replay_totals() {
        let id = serde_json::to_value(InventoryItemId::new()).unwrap();
        assert!(serde_json::from_value::<SaveRequest>(serde_json::json!({"expectedObservationId":id,"scanId":id,"expectedCheckpointId":null,"entriesObserved":5})).is_err());
        assert!(serde_json::from_value::<CheckpointRequest>(
            serde_json::json!({"expectedCheckpointId":id,"path":"/tmp"})
        )
        .is_err());
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use disk_steward_core::RevisionId;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn recursive_state_survives_private_store_reopen_and_continues_without_double_counting() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        for n in 0..1100 {
            std::fs::write(nested.join(format!("file-{n}")), b"data").unwrap();
        }
        std::fs::hard_link(nested.join("file-0"), nested.join("shared-name")).unwrap();
        let revision = RevisionId::new();
        let policy = crate::global_exclusions::SummaryExclusionPolicy::empty_for_tests();
        let (session, _) =
            super::super::selected_root_session(root.path(), 512, super::super::MAX_DURATION)
                .unwrap();
        let mut job = super::super::subtree_job::SubtreeJob::start(
            &session,
            crate::global_exclusions::SummaryExclusionPolicy::empty_for_tests(),
            revision,
        )
        .unwrap();
        let first = job
            .advance(
                &AtomicBool::new(false),
                Instant::now(),
                super::super::MAX_DURATION,
            )
            .unwrap();
        assert!(first.can_continue);
        let payload = job
            .capture_replay(Instant::now(), super::super::MAX_DURATION)
            .unwrap();
        let private = tempfile::tempdir().unwrap();
        std::fs::set_permissions(private.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = private.path().join("recursive.sqlite3");
        let mut store = Store::open(&path).unwrap();
        store.save(None, payload.clone()).unwrap();
        drop(store);
        drop(job);
        drop(session);
        let store = Store::open(&path).unwrap();
        let saved = store.get().unwrap().unwrap();
        assert_eq!(saved, payload);
        let (session, _) =
            super::super::selected_root_session(root.path(), 512, super::super::MAX_DURATION)
                .unwrap();
        let (mut job, replayed) = super::super::subtree_replay::resume(
            &session,
            &saved,
            policy,
            revision,
            &AtomicBool::new(false),
            Instant::now(),
            super::super::MAX_DURATION,
        )
        .unwrap();
        assert_eq!(
            replayed.summary.entries_observed,
            first.summary.entries_observed
        );
        assert_ne!(replayed.scan_id, first.scan_id);
        assert_ne!(
            replayed.summary.observation_id,
            first.summary.observation_id
        );
        let mut final_result = replayed;
        while final_result.can_continue {
            final_result = job
                .advance(
                    &AtomicBool::new(false),
                    Instant::now(),
                    super::super::MAX_DURATION,
                )
                .unwrap();
        }
        assert!(final_result.scan_finished);
        assert_eq!(final_result.summary.files_observed, 1101);
        assert_eq!(final_result.summary.unique_file_objects, 1100);
        assert_eq!(final_result.summary.duplicate_hardlink_names, 1);
        assert_eq!(final_result.summary.unique_file_logical_bytes, "4400");
        assert_eq!(store.get().unwrap().unwrap(), saved);
        assert_eq!(std::fs::read(nested.join("file-0")).unwrap(), b"data");
    }
}
