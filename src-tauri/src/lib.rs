mod example_plan;
mod exclusion_identity;
mod fixture_demo;
mod general_plan;
mod global_exclusions;
mod selected_metadata;

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

static NATIVE_CHECK_ACTIVE: AtomicBool = AtomicBool::new(false);
struct NativeCheckGuard;
impl NativeCheckGuard {
    fn acquire() -> Result<Self, String> {
        NATIVE_CHECK_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| "Finish the open native check before starting another.".to_owned())
    }
}
impl Drop for NativeCheckGuard {
    fn drop(&mut self) {
        NATIVE_CHECK_ACTIVE.store(false, Ordering::Release);
    }
}

use disk_steward_core::{CapabilityReport, CapabilityStatus, HostPlatform, PlatformCapability};

/// Backend-owned state, deliberately not exposed as a generic frontend command.
pub fn foundation_capabilities() -> Vec<CapabilityReport> {
    let platform = match std::env::consts::OS {
        "macos" => HostPlatform::MacOS,
        "windows" => HostPlatform::Windows,
        "linux" => HostPlatform::Linux,
        _ => HostPlatform::Other,
    };
    [
        PlatformCapability::ScopeAccess,
        PlatformCapability::FileIdentity,
        PlatformCapability::RecoverableStage,
        PlatformCapability::Restore,
        PlatformCapability::PermanentRemoval,
        PlatformCapability::SecureCredentialStore,
        PlatformCapability::NativeConfirmation,
    ]
    .into_iter()
    .map(|capability| {
        CapabilityReport::new(
            platform,
            capability,
            CapabilityStatus::Unsupported {
                reason: "Production adapter not implemented or validated in M0".into(),
            },
        )
    })
    .collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(foundation_capabilities())
        .setup(|app| {
            app.manage(example_plan::ExamplePlanState::open(app.handle()));
            app.manage(fixture_demo::FixtureDemoState::open(app.handle()));
            app.manage(general_plan::GeneralPlanState::open(app.handle()));
            app.manage(global_exclusions::ExclusionState::open(app.handle()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            foundation_status,
            check_selected_folder_access,
            preview_native_review,
            example_plan::get_example_plan,
            example_plan::edit_example_plan,
            example_plan::freeze_example_review,
            example_plan::preview_saved_example_review,
            fixture_demo::get_fixture_demo_status,
            fixture_demo::prepare_purge_review,
            fixture_demo::confirm_purge_execution,
            fixture_demo::start_fixture_demo,
            fixture_demo::prepare_fixture_stage,
            fixture_demo::prepare_fixture_restore,
            fixture_demo::request_fixture_execution,
            general_plan::get_general_plan,
            general_plan::edit_general_plan,
            general_plan::undo_general_plan,
            general_plan::redo_general_plan,
            general_plan::freeze_general_review,
            general_plan::get_general_review,
            general_plan::list_general_reviews,
            general_plan::add_selected_general_item,
            general_plan::add_selected_general_folder,
            global_exclusions::get_exclusion_registry,
            global_exclusions::add_global_exclusion,
            global_exclusions::remove_global_exclusion
        ])
        .run(tauri::generate_context!())
        .expect("error while running Disk Steward");
}

/// Read-only diagnostic bridge. Takes no paths, scopes, commands or credentials.
#[tauri::command]
fn foundation_status(reports: tauri::State<'_, Vec<CapabilityReport>>) -> Vec<(String, bool)> {
    reports
        .iter()
        .map(|report| (format!("{:?}", report.capability), report.is_available()))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_operation_ownership_is_exclusive_until_worker_guard_drops() {
        let guard = super::NativeCheckGuard::acquire().expect("first operation");
        assert!(super::NativeCheckGuard::acquire().is_err());
        drop(guard);
        assert!(super::NativeCheckGuard::acquire().is_ok());
    }

    #[test]
    fn unimplemented_adapters_never_report_available() {
        let reports = super::foundation_capabilities();
        assert_eq!(reports.len(), 7);
        assert!(reports.iter().all(|report| !report.is_available()));
    }
}

/// Development feasibility check. The renderer cannot supply a path. No scope
/// grant is persisted, no contents are returned, and no recursive scan occurs.
#[tauri::command]
async fn check_selected_folder_access(app: tauri::AppHandle) -> Result<String, String> {
    let guard = NativeCheckGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let Some(selection) = app
            .dialog()
            .file()
            .set_title("Choose a folder for a read-only access check")
            .set_can_create_directories(false)
            .blocking_pick_folder()
        else {
            return Ok("Folder check canceled; no folder was saved.".to_owned());
        };
        let path = selection
            .into_path()
            .map_err(|_| "The selected location is not a supported local folder.".to_owned())?;
        check_folder_readability(&path).map_err(str::to_owned)?;
        Ok("Folder could be opened for reading. No scan ran and no folder was saved.".to_owned())
    })
    .await
    .map_err(|_| "Folder access check did not complete.".to_owned())?
}

/// A fixed, backend-owned feasibility example. Acknowledgement is not approval,
/// is never persisted, and cannot be consumed by an execution service.
#[tauri::command]
async fn preview_native_review(app: tauri::AppHandle) -> Result<String, String> {
    let guard = NativeCheckGuard::acquire()?;
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window is unavailable.".to_owned())?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let acknowledged = app.dialog().message(
            "Synthetic plan · revision 1\nOne disposable cache fixture · 4 MiB\nExample action: recoverable staging. Staging does not free space.\n\nThis is a dialog preview. No approval is issued, no folder is accessed, and nothing can be changed.")
            .title("Preview a cleanup review")
            .parent(&window)
            .buttons(MessageDialogButtons::OkCancelCustom("Acknowledge example".into(), "Cancel".into()))
            .blocking_show();
        if acknowledged { "Example acknowledged. No approval was issued and nothing changed.".to_owned() }
        else { "Review preview canceled. Nothing changed.".to_owned() }
    }).await.map_err(|_| "Native review preview did not complete.".to_owned())
}

/// Access feasibility only; this result is never an execution precondition or
/// a retained scope grant. No entry names are requested from the iterator.
fn check_folder_readability(path: &std::path::Path) -> Result<(), &'static str> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "Folder metadata could not be read. Access remains unavailable.")?;
    if !metadata.file_type().is_dir() {
        return Err("Select a local directory; links and files are unsupported in this check.");
    }
    std::fs::read_dir(path).map_err(|_| "Folder access was denied or became unavailable.")?;
    Ok(())
}

#[cfg(test)]
mod folder_access_tests {
    use super::check_folder_readability;

    #[test]
    fn selected_location_must_remain_a_readable_directory() {
        let fixture = tempfile::tempdir().unwrap();
        assert!(check_folder_readability(fixture.path()).is_ok());
        let file = fixture.path().join("not-a-directory");
        std::fs::write(&file, b"synthetic fixture").unwrap();
        assert!(check_folder_readability(&file).is_err());
        std::fs::remove_file(&file).unwrap();
        assert!(check_folder_readability(&file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn link_replacement_and_revoked_read_permission_are_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().join("selected");
        std::fs::create_dir(&directory).unwrap();
        let link = fixture.path().join("link");
        symlink(&directory, &link).unwrap();
        assert!(check_folder_readability(&link).is_err());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o0)).unwrap();
        let denied = check_folder_readability(&directory);
        // Restore this own fixture before assertions or temporary-dir cleanup.
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            denied.is_err(),
            "run permission-denial checks as an unprivileged user"
        );
    }
}
