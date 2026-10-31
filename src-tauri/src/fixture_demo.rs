//! Narrow native bridge for the isolated disposable-file demo.
//! The renderer supplies typed review identifiers only; all file targets and
//! approval decisions remain owned by DemoExecutionService and the native OS.

use std::sync::{Arc, Mutex};

use disk_steward_core::{ReceiptId, ReviewId, RevisionId};
use disk_steward_demo_execution::{
    DemoError, DemoExecutionResult, DemoExecutionService, DemoStatus, PreparedDemoReview,
};
use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

pub(crate) struct FixtureDemoState {
    service: Arc<Mutex<Result<DemoExecutionService, FixtureDemoError>>>,
    foreign_owner: bool,
}

impl FixtureDemoState {
    pub(crate) fn open(app: &tauri::AppHandle) -> Self {
        let service = (|| {
            let directory = app.path().app_data_dir().map_err(|_| unavailable())?;
            std::fs::create_dir_all(&directory).map_err(|_| unavailable())?;
            let metadata = std::fs::symlink_metadata(&directory).map_err(|_| unavailable())?;
            if !metadata.file_type().is_dir() {
                return Err(unavailable());
            }
            DemoExecutionService::open(&directory).map_err(public_error)
        })();
        Self::from_result(service)
    }

    fn from_result(service: Result<DemoExecutionService, FixtureDemoError>) -> Self {
        let foreign_owner = matches!(&service, Err(error) if error.code == "busy");
        Self {
            service: Arc::new(Mutex::new(service)),
            foreign_owner,
        }
    }

    pub(crate) fn permits_plan_edits(&self) -> bool {
        !self.foreign_owner
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RevisionRequest {
    expected_revision_id: RevisionId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RestoreRequest {
    expected_revision_id: RevisionId,
    receipt_id: ReceiptId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ExecutionRequest {
    review_id: ReviewId,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FixtureDemoError {
    code: &'static str,
    message: &'static str,
}

fn unavailable() -> FixtureDemoError {
    FixtureDemoError {
        code: "storage_unavailable",
        message: "The disposable demo is unavailable. No user files can be changed.",
    }
}

fn public_error(error: DemoError) -> FixtureDemoError {
    let message = match error {
        DemoError::Busy => "Another demo operation is still running. Try again shortly.",
        DemoError::UnsupportedSchema => "The saved demo uses an unsupported format. It has not been reset.",
        DemoError::CorruptData => "The saved demo could not be verified. Refresh status before continuing.",
        DemoError::PlanUnavailable => "The example plan is unavailable. Reload it before continuing.",
        DemoError::StaleRevision => "The example plan changed. Reload it and prepare a new review.",
        DemoError::NothingSelected => "Include at least one demo item before preparing a review.",
        DemoError::SessionUnavailable => "The demo session is unavailable. Inspect its status before continuing.",
        DemoError::NeedsInspection => "The demo session needs inspection. No new operation was started.",
        DemoError::InvalidReview => "The saved review could not be verified. Prepare a new review.",
        DemoError::ReviewExpired => "This review expired. Prepare a new review.",
        DemoError::ReviewReused => "This review was already used or is no longer available.",
        DemoError::ClockReversed => "The system clock moved backwards. The review cannot be used.",
        DemoError::ConfirmationCancelled => "The native confirmation was canceled. No demo files were changed.",
        DemoError::FixtureChanged => "A demo file changed during review. Refresh status to see any completed or uncertain steps.",
        DemoError::FixtureUnavailable => "A demo file is unavailable. Inspect the session before continuing.",
        DemoError::LedgerUnavailable => "The local approval record is unavailable. Refresh status to see whether any demo steps completed.",
        DemoError::ExecutionUncertain => "The demo operation has an uncertain result. Inspect its status before continuing.",
        DemoError::Storage => "Local demo storage is unavailable. No user files can be changed.",
    };
    FixtureDemoError {
        code: error.code(),
        message,
    }
}

async fn with_service<T, F>(state: &FixtureDemoState, operation: F) -> Result<T, FixtureDemoError>
where
    T: Send + 'static,
    F: FnOnce(&mut DemoExecutionService) -> Result<T, DemoError> + Send + 'static,
{
    let service = Arc::clone(&state.service);
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = service.lock().map_err(|_| unavailable())?;
        let service = guard.as_mut().map_err(|error| *error)?;
        operation(service).map_err(public_error)
    })
    .await
    .map_err(|_| unavailable())?
}

#[tauri::command]
pub(crate) async fn get_fixture_demo_status(
    state: tauri::State<'_, FixtureDemoState>,
) -> Result<DemoStatus, FixtureDemoError> {
    with_service(&state, |service| service.status()).await
}

#[tauri::command]
pub(crate) async fn start_fixture_demo(
    state: tauri::State<'_, FixtureDemoState>,
) -> Result<DemoStatus, FixtureDemoError> {
    with_service(&state, |service| service.start_demo()).await
}

#[tauri::command]
pub(crate) async fn prepare_fixture_stage(
    request: RevisionRequest,
    state: tauri::State<'_, FixtureDemoState>,
) -> Result<PreparedDemoReview, FixtureDemoError> {
    with_service(&state, move |service| {
        service.prepare_stage(request.expected_revision_id)
    })
    .await
}

#[tauri::command]
pub(crate) async fn prepare_fixture_restore(
    request: RestoreRequest,
    state: tauri::State<'_, FixtureDemoState>,
) -> Result<PreparedDemoReview, FixtureDemoError> {
    with_service(&state, move |service| {
        service.prepare_restore(request.expected_revision_id, request.receipt_id)
    })
    .await
}

#[tauri::command]
pub(crate) async fn request_fixture_execution(
    request: ExecutionRequest,
    app: tauri::AppHandle,
    state: tauri::State<'_, FixtureDemoState>,
) -> Result<DemoExecutionResult, FixtureDemoError> {
    let guard = crate::NativeCheckGuard::acquire().map_err(|_| FixtureDemoError {
        code: "native_check_busy",
        message: "Finish the open native review before starting another.",
    })?;
    let window = app.get_webview_window("main").ok_or_else(unavailable)?;
    let service = Arc::clone(&state.service);
    tauri::async_runtime::spawn_blocking(move || {
        // Keep the process-wide guard alive across the native callback and the
        // service's mandatory post-dialog validation and execution.
        let _native_guard = guard;
        let mut guard = service.lock().map_err(|_| unavailable())?;
        let service = guard.as_mut().map_err(|error| *error)?;
        service
            .confirm_and_execute(request.review_id, |prepared| {
                let affirmative = match prepared.operation.as_str() {
                    "stage" => "Stage demo files",
                    "restore" => "Restore demo files",
                    _ => return false,
                };
                let summary = native_summary(prepared);
                app.dialog()
                    .message(summary)
                    .title("Confirm disposable demo operation")
                    .parent(&window)
                    .buttons(MessageDialogButtons::OkCancelCustom(
                        affirmative.into(),
                        "Cancel".into(),
                    ))
                    .blocking_show()
            })
            .map_err(public_error)
    })
    .await
    .map_err(|_| unavailable())?
}

fn native_summary(review: &PreparedDemoReview) -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    native_summary_at(review, now_ms)
}

fn native_summary_at(review: &PreparedDemoReview, now_ms: i64) -> String {
    let (title, destination, consequence) = match review.operation.as_str() {
        "stage" => (
            "Stage the selected disposable demo files",
            "Destination: app-managed demo holding area",
            "Staging keeps recovery data and frees no space.",
        ),
        "restore" => (
            "Restore the selected disposable demo files",
            "Destination: their original demo locations",
            "Restore requires the original filename to be unoccupied in its original location.",
        ),
        _ => (
            "Confirm disposable demo operation",
            "Destination: app-managed demo locations",
            "No user files are targets.",
        ),
    };
    let items = review
        .items
        .iter()
        .map(|item| format!("• {} · {} bytes", item.label, item.bytes))
        .collect::<Vec<_>>()
        .join("\n");
    let remaining_ms = review.expires_at_ms.saturating_sub(now_ms).max(0) as u64;
    let remaining_seconds = remaining_ms.saturating_add(999) / 1000;
    let remaining = format!("{}m {}s", remaining_seconds / 60, remaining_seconds % 60);
    format!(
        "{title}\nOperation: {}\nSelected total: {} bytes\nExpires in {remaining}; checked again after confirmation\n{destination}\n{consequence}\nRecovered space: 0 bytes\n\nExact demo targets:\n{items}\n\nOnly the disposable files listed above are included.",
        review.operation, review.selected_bytes
    )
}

#[cfg(test)]
mod tests {
    use super::native_summary_at;
    use disk_steward_core::{PlanId, ReviewId, RevisionId, SessionId};
    use disk_steward_demo_execution::{PreparedDemoReview, PreparedItem};

    #[cfg(unix)]
    #[test]
    fn second_live_instance_cannot_edit_the_shared_plan() {
        let directory = tempfile::tempdir().expect("isolated app data");
        let owner = super::DemoExecutionService::open(directory.path()).expect("first owner");
        let second = super::FixtureDemoState::from_result(
            super::DemoExecutionService::open(directory.path()).map_err(super::public_error),
        );
        assert!(!second.permits_plan_edits());
        let first = super::FixtureDemoState::from_result(Ok(owner));
        assert!(first.permits_plan_edits());
        drop(first);
        // An existing read-only instance does not silently acquire ownership.
        assert!(!second.permits_plan_edits());
        let reopened = super::FixtureDemoState::from_result(
            super::DemoExecutionService::open(directory.path()).map_err(super::public_error),
        );
        assert!(reopened.permits_plan_edits());
    }

    fn review(operation: &str) -> PreparedDemoReview {
        PreparedDemoReview {
            schema_version: 1,
            kind: "disposable_file_review".into(),
            review_id: ReviewId::new(),
            plan_id: PlanId::new(),
            revision_id: RevisionId::new(),
            session_id: SessionId::new(),
            operation: operation.into(),
            items: vec![PreparedItem {
                action_id: disk_steward_core::ActionId::new(),
                label: "Synthetic cache fixture".into(),
                bytes: 4096,
            }],
            selected_bytes: 4096,
            manifest_sha256: "a".repeat(64),
            expires_at_ms: 1_800_000_000_000,
        }
    }

    #[test]
    fn stage_summary_shows_exact_backend_row_and_no_space_claim() {
        let summary = native_summary_at(&review("stage"), 1_799_999_748_000);
        assert!(summary.contains("Stage the selected disposable demo files"));
        assert!(summary.contains("Synthetic cache fixture · 4096 bytes"));
        assert!(summary.contains("Staging keeps recovery data and frees no space."));
        assert!(summary.contains("Expires in 4m 12s; checked again after confirmation"));
    }

    #[test]
    fn restore_summary_explains_original_space_requirement() {
        let summary = native_summary_at(&review("restore"), 1_799_999_748_000);
        assert!(summary.contains("Restore the selected disposable demo files"));
        assert!(summary.contains(
            "Restore requires the original filename to be unoccupied in its original location."
        ));
        assert!(summary.contains("their original demo locations"));
    }

    #[test]
    fn typed_requests_reject_extra_fields_and_wrong_typed_ids() {
        let revision = RevisionId::new();
        let valid = serde_json::json!({"expectedRevisionId": revision});
        assert!(serde_json::from_value::<super::RevisionRequest>(valid.clone()).is_ok());
        let with_path = serde_json::json!({
            "expectedRevisionId": revision,
            "path": "/tmp/anything"
        });
        assert!(serde_json::from_value::<super::RevisionRequest>(with_path).is_err());
        let wrong_id = serde_json::json!({"expectedRevisionId": ReviewId::new()});
        assert!(serde_json::from_value::<super::RevisionRequest>(wrong_id).is_err());

        let restore = serde_json::json!({
            "expectedRevisionId": revision,
            "receiptId": disk_steward_core::ReceiptId::new(),
            "approved": true
        });
        assert!(serde_json::from_value::<super::RestoreRequest>(restore).is_err());
        let execute = serde_json::json!({"reviewId": ReviewId::new(), "approved": true});
        assert!(serde_json::from_value::<super::ExecutionRequest>(execute).is_err());
    }
}
