//! Narrow backend-owned example-plan bridge. No path, SQL or execution input.
use std::sync::{Arc, Mutex};

use disk_steward_core::{ActionId, RevisionId};
use disk_steward_review_plan::{Decision, FrozenReview, PlanView, ReviewPlanStore, StoreError};
use serde::{Deserialize, Serialize};
use tauri::Manager;

pub(crate) struct ExamplePlanState(Arc<Mutex<Result<ReviewPlanStore, PlanCommandError>>>);

impl ExamplePlanState {
    pub(crate) fn open(app: &tauri::AppHandle) -> Self {
        // A failed/unknown store stays unavailable. Never reset or replace it.
        let store = (|| {
            let directory = app.path().app_data_dir().map_err(|_| unavailable())?;
            std::fs::create_dir_all(&directory).map_err(|_| unavailable())?;
            let metadata = std::fs::symlink_metadata(&directory).map_err(|_| unavailable())?;
            if !metadata.file_type().is_dir() {
                return Err(unavailable());
            }
            ReviewPlanStore::open(directory.join("review-example-v1.sqlite3")).map_err(public_error)
        })();
        Self(Arc::new(Mutex::new(store)))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EditRequest {
    expected_revision_id: RevisionId,
    action_id: ActionId,
    decision: Decision,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReviewRequest {
    expected_revision_id: RevisionId,
}

#[derive(Clone, Copy, Serialize)]
pub(crate) struct PlanCommandError {
    code: &'static str,
    message: &'static str,
}

fn unavailable() -> PlanCommandError {
    PlanCommandError {
        code: "storage_unavailable",
        message: "The local example plan is unavailable. No user files can be changed.",
    }
}

fn public_error(error: StoreError) -> PlanCommandError {
    match error.code() {
        "stale_revision" => PlanCommandError {
            code: "stale_revision",
            message: "This plan changed. Reload it before saving another decision.",
        },
        "unknown_action" => PlanCommandError {
            code: "unknown_action",
            message: "That item is not part of this example plan. Reload the plan.",
        },
        "nothing_selected" => PlanCommandError {
            code: "nothing_selected",
            message: "Include an item before saving a review.",
        },
        "unsupported_schema" => PlanCommandError {
            code: "unsupported_schema",
            message: "This saved example uses an unsupported format. It has not been reset.",
        },
        _ => PlanCommandError {
            code: "storage_unavailable",
            message:
                "The save could not be confirmed. Reload the example plan before trying again.",
        },
    }
}

async fn with_store<T, F>(state: &ExamplePlanState, operation: F) -> Result<T, PlanCommandError>
where
    T: Send + 'static,
    F: FnOnce(&mut ReviewPlanStore) -> Result<T, StoreError> + Send + 'static,
{
    let store = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = store.lock().map_err(|_| unavailable())?;
        let store = guard.as_mut().map_err(|error| *error)?;
        operation(store).map_err(public_error)
    })
    .await
    .map_err(|_| unavailable())?
}

#[tauri::command]
pub(crate) async fn get_example_plan(
    state: tauri::State<'_, ExamplePlanState>,
) -> Result<PlanView, PlanCommandError> {
    with_store(&state, |store| store.get_plan()).await
}

#[tauri::command]
pub(crate) async fn edit_example_plan(
    request: EditRequest,
    state: tauri::State<'_, ExamplePlanState>,
) -> Result<PlanView, PlanCommandError> {
    with_store(&state, move |store| {
        store.edit(
            request.expected_revision_id,
            request.action_id,
            request.decision,
        )
    })
    .await
}

#[tauri::command]
pub(crate) async fn freeze_example_review(
    request: ReviewRequest,
    state: tauri::State<'_, ExamplePlanState>,
) -> Result<FrozenReview, PlanCommandError> {
    with_store(&state, move |store| {
        store.freeze_review(request.expected_revision_id)
    })
    .await
}
