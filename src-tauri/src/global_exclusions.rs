//! Native-only preservation rules, separate from example-plan decisions.
use disk_steward_core::{GlobalExclusionId, RevisionId};
use disk_steward_exclusion_store::{NativeBinding, Registry, RegistrySnapshot};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct ExclusionError {
    code: &'static str,
    message: &'static str,
}
fn unavailable() -> ExclusionError {
    ExclusionError {
        code: "registry_unavailable",
        message: "Remembered exclusions are unavailable. Their saved data has been retained.",
    }
}
fn store_error(error: disk_steward_exclusion_store::RegistryError) -> ExclusionError {
    let code = error.code();
    let message = match code {
        "stale_revision" => "Remembered exclusions changed. Refresh before another change.",
        "binding_conflict" => "This location now refers to a different directory. Remove the earlier rule separately before remembering it again.",
        "entry_not_found" => "That remembered exclusion is no longer available. Refresh the list.",
        "registry_limit" => "The remembered-exclusion limit has been reached. Existing rules have been retained.",
        _ => "The remembered exclusions could not be verified or saved. Their data has been retained; refresh before continuing.",
    };
    ExclusionError { code, message }
}
#[derive(Clone)]
pub(crate) struct ExclusionState(Arc<Mutex<Result<Registry, ExclusionError>>>);
impl ExclusionState {
    pub(crate) fn open(app: &tauri::AppHandle) -> Self {
        let registry = (|| {
            let base = app.path().app_data_dir().map_err(|_| unavailable())?;
            let private = private_directory(&base)?;
            Registry::open(private.join("exact-directories-v1.sqlite3")).map_err(store_error)
        })();
        Self(Arc::new(Mutex::new(registry)))
    }
}
#[cfg(unix)]
fn private_directory(base: &Path) -> Result<PathBuf, ExclusionError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let uid = rustix::process::geteuid().as_raw();
    std::fs::create_dir_all(base).map_err(|_| unavailable())?;
    let parent = std::fs::symlink_metadata(base).map_err(|_| unavailable())?;
    if !parent.is_dir() || parent.uid() != uid || parent.permissions().mode() & 0o022 != 0 {
        return Err(unavailable());
    }
    let private = base.join("private-exclusions-v1");
    match std::fs::DirBuilder::new().mode(0o700).create(&private) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(unavailable()),
    }
    let metadata = std::fs::symlink_metadata(&private).map_err(|_| unavailable())?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(unavailable());
    }
    Ok(private)
}
#[cfg(not(unix))]
fn private_directory(_: &Path) -> Result<PathBuf, ExclusionError> {
    Err(unavailable())
}

async fn operate<T, F>(
    state: &ExclusionState,
    guard: Option<crate::NativeCheckGuard>,
    action: F,
) -> Result<T, ExclusionError>
where
    T: Send + 'static,
    F: FnOnce(&mut Registry) -> Result<T, ExclusionError> + Send + 'static,
{
    let registry = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut registry = registry.lock().map_err(|_| unavailable())?;
        action(registry.as_mut().map_err(|error| *error)?)
    })
    .await
    .map_err(|_| unavailable())?
}
fn mutation_guard(
    demo: &crate::fixture_demo::FixtureDemoState,
) -> Result<crate::NativeCheckGuard, ExclusionError> {
    if !demo.permits_plan_edits() {
        return Err(ExclusionError {code:"plan_in_use",message:"Another Disk Steward instance owns these settings. Return to that instance to change them."});
    }
    crate::NativeCheckGuard::acquire().map_err(|_| ExclusionError {
        code: "native_check_busy",
        message: "Finish the active native dialog or operation before changing exclusions.",
    })
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RevisionRequest {
    expected_revision_id: RevisionId,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RemoveRequest {
    expected_revision_id: RevisionId,
    exclusion_id: GlobalExclusionId,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegistryChange {
    snapshot: RegistrySnapshot,
    canceled: bool,
}
fn require_revision(
    registry: &Registry,
    expected: RevisionId,
) -> Result<RegistrySnapshot, ExclusionError> {
    let snapshot = registry.snapshot().map_err(store_error)?;
    if snapshot.revision_id != expected {
        return Err(ExclusionError {
            code: "stale_revision",
            message: "Remembered exclusions changed. Refresh before another change.",
        });
    }
    Ok(snapshot)
}
#[tauri::command]
pub(crate) async fn get_exclusion_registry(
    state: tauri::State<'_, ExclusionState>,
) -> Result<RegistrySnapshot, ExclusionError> {
    operate(&state, None, |registry| {
        registry.snapshot().map_err(store_error)
    })
    .await
}
#[tauri::command]
pub(crate) async fn add_global_exclusion(
    request: RevisionRequest,
    app: tauri::AppHandle,
    state: tauri::State<'_, ExclusionState>,
    demo: tauri::State<'_, crate::fixture_demo::FixtureDemoState>,
) -> Result<RegistryChange, ExclusionError> {
    let guard = mutation_guard(&demo)?;
    operate(&state,Some(guard),move|registry| {
        require_revision(registry,request.expected_revision_id)?;
        let window=app.get_webview_window("main").ok_or_else(unavailable)?;
        let Some(selection)=app.dialog().file().set_title("Choose one exact folder to remember for future scan exclusion").set_can_create_directories(false).blocking_pick_folder() else {
            return Ok(RegistryChange{snapshot:registry.snapshot().map_err(store_error)?,canceled:true});
        };
        let path=selection.into_path().map_err(|_|unavailable())?;
        let observed=crate::exclusion_identity::observe_directory(&path).map_err(|_|ExclusionError{code:"identity_unavailable",message:"This directory's identity could not be established. No exclusion was saved."})?;
        let location=confirmation_location(&path);
        let accepted=app.dialog().message(format!("Remember this exact directory?\n\n{location}\n\nFuture scans may skip this directory and its contents only after checking its identity. Scanning is not available in this build, and the example plan is unchanged. This does not authorize a scan or cleanup. A moved or replaced directory will need review.\n\nNothing in the selected folder will be modified."))
            .title("Remember a scan exclusion").parent(&window).buttons(MessageDialogButtons::OkCancelCustom("Remember exclusion".into(),"Cancel".into())).blocking_show();
        if !accepted {return Ok(RegistryChange{snapshot:registry.snapshot().map_err(store_error)?,canceled:true});}
        if !matches!(crate::exclusion_identity::revalidate_directory(&path,&observed),crate::exclusion_identity::Revalidation::Matched) {
            return Err(ExclusionError{code:"identity_changed",message:"The directory changed or became unavailable during confirmation. No exclusion was saved."});
        }
        let binding=NativeBinding::new(observed.locator_bytes,observed.volume_identity,observed.directory_identity,observed.basename).map_err(store_error)?;
        let snapshot=registry.add_confirmed(request.expected_revision_id,binding).map_err(store_error)?;
        Ok(RegistryChange{snapshot,canceled:false})
    }).await
}
#[tauri::command]
pub(crate) async fn remove_global_exclusion(
    request: RemoveRequest,
    app: tauri::AppHandle,
    state: tauri::State<'_, ExclusionState>,
    demo: tauri::State<'_, crate::fixture_demo::FixtureDemoState>,
) -> Result<RegistryChange, ExclusionError> {
    let guard = mutation_guard(&demo)?;
    operate(&state,Some(guard),move|registry| {
        require_revision(registry,request.expected_revision_id)?;
        let binding=registry.get_binding(request.exclusion_id).map_err(store_error)?;
        let path=path_from_locator(binding.locator_bytes())?;
        let window=app.get_webview_window("main").ok_or_else(unavailable)?;
        let accepted=app.dialog().message(format!("Remove the remembered exclusion for this location?\n\n{}\n\nA future scan may inspect it when you separately grant access. This does not change the example plan or authorize cleanup. Nothing in the selected folder will be modified.",confirmation_location(&path)))
            .title("Remove a scan exclusion").parent(&window).buttons(MessageDialogButtons::OkCancelCustom("Remove exclusion".into(),"Cancel".into())).blocking_show();
        if !accepted {return Ok(RegistryChange{snapshot:registry.snapshot().map_err(store_error)?,canceled:true});}
        let snapshot=registry.remove_confirmed(request.expected_revision_id,request.exclusion_id).map_err(store_error)?;
        Ok(RegistryChange{snapshot,canceled:false})
    }).await
}
#[cfg(unix)]
fn path_from_locator(bytes: &[u8]) -> Result<PathBuf, ExclusionError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec())))
}
#[cfg(not(unix))]
fn path_from_locator(_: &[u8]) -> Result<PathBuf, ExclusionError> {
    Err(unavailable())
}
fn confirmation_location(path: &Path) -> String {
    #[cfg(unix)]
    if path.to_str().is_none() {
        use std::os::unix::ffi::OsStrExt;
        return format!("Path bytes: {}", path.as_os_str().as_bytes().escape_ascii());
    }
    path.to_string_lossy()
        .chars()
        .map(|ch| {
            if ch.is_control() || crate::selected_metadata::is_bidi_control(ch) {
                format!("\\u{{{:x}}}", ch as u32)
            } else {
                ch.to_string()
            }
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_cannot_supply_path_binding_or_confirmation() {
        let revision = RevisionId::new();
        assert!(serde_json::from_value::<RevisionRequest>(
            serde_json::json!({"expectedRevisionId":revision,"path":"/tmp/forged"})
        )
        .is_err());
        assert!(serde_json::from_value::<RemoveRequest>(serde_json::json!({"expectedRevisionId":revision,"exclusionId":GlobalExclusionId::new(),"approved":true})).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn private_registry_directory_refuses_links_or_unsafe_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("app-data");
        let private = private_directory(&base).unwrap();
        assert_eq!(
            std::fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(&base).is_err());
        let other = root.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let link = root.path().join("linked-app-data");
        symlink(&other, &link).unwrap();
        assert!(private_directory(&link).is_err());
    }
    #[test]
    fn confirmation_location_cannot_hide_line_or_direction_controls() {
        let displayed = confirmation_location(Path::new("/tmp/invoice\u{202e}gpj\n\u{206a}.exe"));
        assert!(displayed.contains("\\u{202e}"));
        assert!(displayed.contains("\\u{a}"));
        assert!(displayed.contains("\\u{206a}"));
        assert!(!displayed.contains('\u{206a}'));
        assert!(!displayed.contains('\u{202e}'));
    }
}

#[cfg(unix)]
pub(crate) use crate::summary_exclusions::SummaryExclusionPolicy;

impl ExclusionState {
    /// Caller holds NativeCheckGuard. Private bindings never enter renderer IPC.
    #[cfg(unix)]
    pub(crate) fn summary_policy(
        &self,
        started: std::time::Instant,
        duration: std::time::Duration,
    ) -> Result<(RevisionId, SummaryExclusionPolicy), &'static str> {
        let fail =
            "Remembered exclusions could not be verified. Review saved rules before summarizing.";
        if started.elapsed() >= duration {
            return Err(fail);
        }
        let locked = self.0.lock().map_err(|_| fail)?;
        if started.elapsed() >= duration {
            return Err(fail);
        }
        let registry = locked.as_ref().map_err(|_| fail)?;
        let snapshot = registry.snapshot().map_err(|_| fail)?;
        let mut identities = std::collections::BTreeSet::new();
        for entry in snapshot.entries {
            if started.elapsed() >= duration {
                return Err(fail);
            }
            let binding = registry.get_binding(entry.id).map_err(|_| fail)?;
            let path = path_from_locator(binding.locator_bytes()).map_err(|_| fail)?;
            if started.elapsed() >= duration {
                return Err(fail);
            }
            let observed = crate::exclusion_identity::observe_directory(&path).map_err(|_| fail)?;
            if started.elapsed() >= duration
                || observed.locator_bytes != binding.locator_bytes()
                || observed.volume_identity != binding.volume_identity()
                || observed.directory_identity != binding.directory_identity()
            {
                return Err(fail);
            }
            let bytes = binding.directory_identity();
            if bytes.len() != 35 || !bytes.starts_with(b"DSXDIR\x01") {
                return Err(fail);
            }
            let device = u64::from_le_bytes(bytes[7..15].try_into().map_err(|_| fail)?);
            let inode = u64::from_le_bytes(bytes[15..23].try_into().map_err(|_| fail)?);
            identities.insert((device, inode));
        }
        let after = registry.snapshot().map_err(|_| fail)?;
        if started.elapsed() >= duration || after.revision_id != snapshot.revision_id {
            return Err(fail);
        }
        Ok((
            snapshot.revision_id,
            SummaryExclusionPolicy::from_identities(identities),
        ))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod summary_policy_tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn saved_rule_is_revalidated_and_stale_or_unavailable_registry_refuses_summary() {
        let root = tempfile::tempdir().unwrap();
        let excluded = root.path().join("excluded");
        std::fs::create_dir(&excluded).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut registry = Registry::open(root.path().join("rules.sqlite3")).unwrap();
        let observation = crate::exclusion_identity::observe_directory(&excluded).unwrap();
        let binding = NativeBinding::new(
            observation.locator_bytes,
            observation.volume_identity,
            observation.directory_identity,
            observation.basename,
        )
        .unwrap();
        let revision = registry.snapshot().unwrap().revision_id;
        registry.add_confirmed(revision, binding).unwrap();
        let state = ExclusionState(Arc::new(Mutex::new(Ok(registry))));
        let (_, policy) = state
            .summary_policy(Instant::now(), Duration::from_secs(2))
            .unwrap();
        let stat = rustix::fs::statat(
            rustix::fs::CWD,
            &excluded,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .unwrap();
        assert!(policy.excludes(&stat));
        std::fs::rename(&excluded, root.path().join("moved")).unwrap();
        assert!(state
            .summary_policy(Instant::now(), Duration::from_secs(2))
            .is_err());
        let unavailable_state = ExclusionState(Arc::new(Mutex::new(Err(unavailable()))));
        assert!(unavailable_state
            .summary_policy(Instant::now(), Duration::from_secs(2))
            .is_err());
    }
}
