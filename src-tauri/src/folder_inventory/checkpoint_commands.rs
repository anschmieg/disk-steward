use super::{
    checkpoint_replay,
    checkpoint_store::{Info, Store},
    FolderInventorySnapshot, FolderInventoryState,
};
use disk_steward_core::InventoryItemId;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};
use tauri::Manager;

#[derive(Clone)]
pub(crate) struct FolderCheckpointState(Arc<Mutex<Result<Store, String>>>);
impl FolderCheckpointState {
    pub(crate) fn open(app: &tauri::AppHandle) -> Self {
        let result = (|| {
            let base = app
                .path()
                .app_data_dir()
                .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
            let private = private_directory(&base)?;
            Store::open(private.join("folder-v1.sqlite3"))
        })();
        Self(Arc::new(Mutex::new(result)))
    }
}
#[cfg(unix)]
pub(super) fn private_directory(base: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let fail = || "Private checkpoint storage is unavailable.".to_owned();
    std::fs::create_dir_all(base).map_err(|_| fail())?;
    let parent = std::fs::symlink_metadata(base).map_err(|_| fail())?;
    let uid = rustix::process::geteuid().as_raw();
    if !parent.is_dir() || parent.uid() != uid || parent.mode() & 0o022 != 0 {
        return Err(fail());
    }
    let private = base.join("private-scan-checkpoints-v1");
    match std::fs::DirBuilder::new().mode(0o700).create(&private) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(fail()),
    }
    let meta = std::fs::symlink_metadata(&private).map_err(|_| fail())?;
    if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o777 != 0o700 {
        return Err(fail());
    }
    Ok(private)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SaveRequest {
    expected_observation_id: InventoryItemId,
    expected_checkpoint_id: Option<InventoryItemId>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CheckpointRequest {
    expected_checkpoint_id: InventoryItemId,
}
fn info(payload: &super::checkpoint_store::Payload) -> Info {
    Info {
        checkpoint_id: payload.checkpoint_id,
        root_label: payload.root_label.clone(),
        page_number: payload.page_number,
        entries_seen: payload.entries_seen.to_string(),
    }
}
#[tauri::command]
pub(crate) async fn get_folder_checkpoint(
    state: tauri::State<'_, FolderCheckpointState>,
) -> Result<Option<Info>, String> {
    let guard = crate::NativeCheckGuard::acquire()?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut locked = state
            .0
            .lock()
            .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
        let store = locked.as_mut().map_err(|e| e.clone())?;
        Ok(store.get()?.as_ref().map(info))
    })
    .await
    .map_err(|_| "Checkpoint inspection did not complete.".to_owned())?
}
#[tauri::command]
pub(crate) async fn save_folder_checkpoint(
    state: tauri::State<'_, FolderCheckpointState>,
    folders: tauri::State<'_, FolderInventoryState>,
    exclusions: tauri::State<'_, crate::global_exclusions::ExclusionState>,
    request: SaveRequest,
) -> Result<Info, String> {
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
            .ok_or("Choose a folder before saving a checkpoint.")?;
        if session.observation_id != request.expected_observation_id
            || session.snapshot.observation_id != request.expected_observation_id
        {
            return Err("Folder observation is stale.".into());
        }
        let (revision, policy) = exclusions
            .summary_policy(started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let frame = session
            .frames
            .last()
            .ok_or("Folder observation is unavailable.")?;
        policy
            .ensure_ancestor_scope_allowed(&frame.fd, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let payload = checkpoint_replay::capture(session, revision).map_err(str::to_owned)?;
        let (after_revision, after_policy) = exclusions
            .summary_policy(started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        if revision != after_revision || policy != after_policy {
            return Err("Remembered exclusions changed. Checkpoint was not saved.".into());
        }
        let mut locked = state
            .0
            .lock()
            .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
        let store = locked.as_mut().map_err(|e| e.clone())?;
        store.save(request.expected_checkpoint_id, payload)
    })
    .await
    .map_err(|_| "Checkpoint save did not complete.".to_owned())?
}
#[tauri::command]
pub(crate) async fn resume_folder_checkpoint(
    app: tauri::AppHandle,
    state: tauri::State<'_, FolderCheckpointState>,
    folders: tauri::State<'_, FolderInventoryState>,
    exclusions: tauri::State<'_, crate::global_exclusions::ExclusionState>,
    request: CheckpointRequest,
) -> Result<Option<FolderInventorySnapshot>, String> {
    use tauri_plugin_dialog::DialogExt;
    let guard = crate::NativeCheckGuard::acquire()?;
    let state = state.inner().clone();
    let sessions = Arc::clone(&folders.0);
    let exclusions = exclusions.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let payload = {
            let mut locked = state
                .0
                .lock()
                .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
            let store = locked.as_mut().map_err(|e| e.clone())?;
            store.get()?.ok_or("No saved checkpoint is available.")?
        };
        if payload.checkpoint_id != request.expected_checkpoint_id {
            return Err("Saved checkpoint changed. Inspect it again.".into());
        }
        let Some(selection) = app
            .dialog()
            .file()
            .set_title("Choose the same unchanged folder to resume its saved paging checkpoint")
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
            return Err(
                "Remembered exclusions changed since this checkpoint. Start a fresh observation."
                    .into(),
            );
        }
        // Check ancestry before any resumed enumeration. The native helper reopens and validates this identity again.
        let named = rustix::fs::statat(
            rustix::fs::CWD,
            &path,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| "Selected folder could not be verified.".to_owned())?;
        super::refuse_dataless_directory(&named).map_err(str::to_owned)?;
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
        if super::identity(&named) != super::identity(&selected) {
            return Err("Selected folder changed during resume.".into());
        }
        policy
            .ensure_ancestor_scope_allowed(&fd, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let (session, snapshot) = checkpoint_replay::resume(
            &path,
            &payload,
            super::MAX_DURATION.saturating_sub(started.elapsed()),
        )
        .map_err(str::to_owned)?;
        let held = session
            .frames
            .last()
            .ok_or("Resumed folder is unavailable.")?;
        if super::identity(&selected) != held.identity {
            return Err("Selected folder changed during resume.".into());
        }
        policy
            .ensure_ancestor_scope_allowed(&held.fd, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let (after_revision, after_policy) = exclusions
            .summary_policy(started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        if revision != after_revision || policy != after_policy {
            return Err("Remembered exclusions changed during resume.".into());
        }
        let mut store_lock = state
            .0
            .lock()
            .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
        let store = store_lock.as_mut().map_err(|e| e.clone())?;
        if store.get()?.as_ref().map(|p| p.checkpoint_id) != Some(payload.checkpoint_id) {
            return Err("Saved checkpoint changed during resume.".into());
        }
        super::validate_session_bindings(&session, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        if started.elapsed() >= super::MAX_DURATION {
            return Err("Checkpoint resume exceeded its cooperative time limit.".into());
        }
        let mut current = sessions
            .lock()
            .map_err(|_| "Folder observation is unavailable.".to_owned())?;
        *current = Some(session);
        Ok(Some(snapshot))
    })
    .await
    .map_err(|_| "Checkpoint resume did not complete.".to_owned())?
}
#[tauri::command]
pub(crate) async fn clear_folder_checkpoint(
    state: tauri::State<'_, FolderCheckpointState>,
    request: CheckpointRequest,
) -> Result<(), String> {
    let guard = crate::NativeCheckGuard::acquire()?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut locked = state
            .0
            .lock()
            .map_err(|_| "Checkpoint storage is unavailable.".to_owned())?;
        let store = locked.as_mut().map_err(|e| e.clone())?;
        store.clear(request.expected_checkpoint_id)
    })
    .await
    .map_err(|_| "Checkpoint removal did not complete.".to_owned())?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_cursor_survives_store_reopen_and_replays_only_the_remaining_names() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        for index in 0..7 {
            std::fs::write(root.path().join(format!("item-{index}")), b"fixture").unwrap();
        }
        let private = tempfile::tempdir().unwrap();
        std::fs::set_permissions(private.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = private.path().join("checkpoint.sqlite3");
        let (session, _) =
            super::super::selected_root_session(root.path(), 2, super::super::MAX_DURATION)
                .unwrap();
        let payload =
            checkpoint_replay::capture(&session, disk_steward_core::RevisionId::new()).unwrap();
        let expected = payload.checkpoint_id;
        let mut store = Store::open(&path).unwrap();
        store.save(None, payload.clone()).unwrap();
        drop(session);
        drop(store);
        let reopened = Store::open(&path).unwrap();
        let durable = reopened.get().unwrap().unwrap();
        assert_eq!(durable.checkpoint_id, expected);
        let (_, next) =
            checkpoint_replay::resume(root.path(), &durable, super::super::MAX_DURATION).unwrap();
        assert_eq!(next.entries.len(), 5);
        assert_eq!(next.page_number, 2);
        assert!(next.enumeration_complete);
        let mut names = payload
            .replay_names
            .iter()
            .map(|name| super::super::safe_label_bytes(name))
            .chain(next.entries.iter().map(|row| row.name.clone()))
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 7);
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 7);
        assert_eq!(reopened.get().unwrap().unwrap(), durable);
    }

    #[test]
    fn renderer_cannot_supply_paths_prefixes_or_approval() {
        let id = serde_json::to_value(InventoryItemId::new()).unwrap();
        assert!(serde_json::from_value::<CheckpointRequest>(
            serde_json::json!({"expectedCheckpointId":id,"path":"/tmp"})
        )
        .is_err());
        assert!(serde_json::from_value::<SaveRequest>(serde_json::json!({"expectedObservationId":id,"expectedCheckpointId":null,"replayNames":[]})).is_err());
    }
}
