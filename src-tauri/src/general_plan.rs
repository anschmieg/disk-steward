//! Typed bridge to the non-executable general editor. Profiles select fixed
//! backend-owned databases, never renderer paths or cleanup capabilities.
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use disk_steward_core::{ReviewId, RevisionId};
use disk_steward_plan_model::{EditOutcome, EditorSnapshot, FrozenReview, PlanEdit};
use disk_steward_plan_store::{FixtureScale, GeneralPlanStore, ReviewSummary, StoreError};
use serde::{Deserialize, Serialize};
use tauri::Manager;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Profile {
    Pilot,
    Stress,
}
impl Profile {
    fn filename(self) -> &'static str {
        match self {
            Self::Pilot => "editor-pilot-v1.sqlite3",
            Self::Stress => "editor-stress-v1.sqlite3",
        }
    }
    fn scale(self) -> FixtureScale {
        match self {
            Self::Pilot => FixtureScale::Pilot1000,
            Self::Stress => FixtureScale::Stress10000,
        }
    }
}

struct Stores {
    directory: Result<PathBuf, GeneralPlanError>,
    profiles: BTreeMap<Profile, Result<GeneralPlanStore, GeneralPlanError>>,
}
pub(crate) struct GeneralPlanState(Arc<Mutex<Stores>>);
impl GeneralPlanState {
    pub(crate) fn open(app: &tauri::AppHandle) -> Self {
        let directory = (|| {
            let path = app.path().app_data_dir().map_err(|_| unavailable())?;
            std::fs::create_dir_all(&path).map_err(|_| unavailable())?;
            if !std::fs::symlink_metadata(&path)
                .map_err(|_| unavailable())?
                .file_type()
                .is_dir()
            {
                return Err(unavailable());
            }
            Ok(path)
        })();
        // The 10,000-item workspace is created only when explicitly selected.
        Self(Arc::new(Mutex::new(Stores {
            directory,
            profiles: BTreeMap::new(),
        })))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProfileRequest {
    profile: Profile,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EditRequest {
    profile: Profile,
    expected_revision_id: RevisionId,
    edit: PlanEdit,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RevisionRequest {
    profile: Profile,
    expected_revision_id: RevisionId,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SavedReviewRequest {
    profile: Profile,
    review_id: ReviewId,
}

#[derive(Clone, Copy, Serialize)]
pub(crate) struct GeneralPlanError {
    code: &'static str,
    message: &'static str,
}
fn unavailable() -> GeneralPlanError {
    GeneralPlanError {
        code: "storage_unavailable",
        message: "The example editor is unavailable. Refresh before trying another change.",
    }
}
fn public_error(error: StoreError) -> GeneralPlanError {
    let code = error.code();
    let message = match code {
        "stale_revision" => "This plan changed. Refresh it before saving another choice.",
        "fixture_profile_mismatch" => {
            "This example workspace has a different saved profile. It has not been reset."
        }
        "unsupported_schema" => {
            "The saved editor uses an unsupported format. It has not been reset."
        }
        "corrupt_data" | "invalid_plan" => {
            "The saved plan could not be verified. Its data has been retained."
        }
        "review_limit_reached" => {
            "This workspace has reached its saved-review limit. Earlier reviews have been retained."
        }
        "review_not_found" => {
            "That saved review is unavailable in this workspace. Refresh the review list."
        }
        "invalid_edit" => {
            "That change is not valid for the current plan. Refresh and inspect the item."
        }
        _ => "The save could not be confirmed. Refresh before trying another change.",
    };
    GeneralPlanError { code, message }
}
fn edit_guard(
    demo: &crate::fixture_demo::FixtureDemoState,
) -> Result<crate::NativeCheckGuard, GeneralPlanError> {
    if !demo.permits_plan_edits() {
        return Err(GeneralPlanError { code: "plan_in_use", message: "Another Disk Steward instance owns this workspace. Return to that instance to edit it." });
    }
    crate::NativeCheckGuard::acquire().map_err(|_| GeneralPlanError {
        code: "native_check_busy",
        message: "Finish the active native review or demo operation before editing.",
    })
}
async fn with_store<T, F>(
    state: &GeneralPlanState,
    profile: Profile,
    operation: F,
) -> Result<T, GeneralPlanError>
where
    T: Send + 'static,
    F: FnOnce(&mut GeneralPlanStore) -> Result<T, StoreError> + Send + 'static,
{
    let stores = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || {
        let mut stores = stores.lock().map_err(|_| unavailable())?;
        let directory = stores.directory.as_ref().map_err(|error| *error)?.clone();
        let stored = stores.profiles.entry(profile).or_insert_with(|| {
            GeneralPlanStore::open(directory.join(profile.filename()), profile.scale())
                .map_err(public_error)
        });
        let store = stored.as_mut().map_err(|error| *error)?;
        operation(store).map_err(public_error)
    })
    .await
    .map_err(|_| unavailable())?
}

#[tauri::command]
pub(crate) async fn get_general_plan(
    request: ProfileRequest,
    state: tauri::State<'_, GeneralPlanState>,
) -> Result<EditorSnapshot, GeneralPlanError> {
    with_store(&state, request.profile, |store| store.get_plan()).await
}
#[tauri::command]
pub(crate) async fn edit_general_plan(
    request: EditRequest,
    state: tauri::State<'_, GeneralPlanState>,
    demo: tauri::State<'_, crate::fixture_demo::FixtureDemoState>,
) -> Result<EditOutcome, GeneralPlanError> {
    let guard = edit_guard(&demo)?;
    with_store(&state, request.profile, move |store| {
        let _guard = guard;
        store.edit(request.expected_revision_id, request.edit)
    })
    .await
}
#[tauri::command]
pub(crate) async fn undo_general_plan(
    request: RevisionRequest,
    state: tauri::State<'_, GeneralPlanState>,
    demo: tauri::State<'_, crate::fixture_demo::FixtureDemoState>,
) -> Result<EditorSnapshot, GeneralPlanError> {
    let guard = edit_guard(&demo)?;
    with_store(&state, request.profile, move |store| {
        let _guard = guard;
        store.undo(request.expected_revision_id)
    })
    .await
}
#[tauri::command]
pub(crate) async fn redo_general_plan(
    request: RevisionRequest,
    state: tauri::State<'_, GeneralPlanState>,
    demo: tauri::State<'_, crate::fixture_demo::FixtureDemoState>,
) -> Result<EditorSnapshot, GeneralPlanError> {
    let guard = edit_guard(&demo)?;
    with_store(&state, request.profile, move |store| {
        let _guard = guard;
        store.redo(request.expected_revision_id)
    })
    .await
}
#[tauri::command]
pub(crate) async fn freeze_general_review(
    request: RevisionRequest,
    state: tauri::State<'_, GeneralPlanState>,
    demo: tauri::State<'_, crate::fixture_demo::FixtureDemoState>,
) -> Result<FrozenReview, GeneralPlanError> {
    let guard = edit_guard(&demo)?;
    with_store(&state, request.profile, move |store| {
        let _guard = guard;
        store.freeze_review(request.expected_revision_id)
    })
    .await
}
#[tauri::command]
pub(crate) async fn list_general_reviews(
    request: ProfileRequest,
    state: tauri::State<'_, GeneralPlanState>,
) -> Result<Vec<ReviewSummary>, GeneralPlanError> {
    with_store(&state, request.profile, |store| store.list_reviews()).await
}
#[tauri::command]
pub(crate) async fn get_general_review(
    request: SavedReviewRequest,
    state: tauri::State<'_, GeneralPlanState>,
) -> Result<FrozenReview, GeneralPlanError> {
    with_store(&state, request.profile, move |store| {
        store.get_saved_review(request.review_id)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_and_edit_requests_cannot_supply_paths_or_approval() {
        assert!(serde_json::from_value::<ProfileRequest>(
            serde_json::json!({"profile":"/tmp/arbitrary"})
        )
        .is_err());
        assert!(serde_json::from_value::<ProfileRequest>(
            serde_json::json!({"profile":"pilot","path":"/tmp/arbitrary"})
        )
        .is_err());
        let request = serde_json::json!({"profile":"pilot","expectedRevisionId":RevisionId::new(),"edit":{"kind":"setChoice","itemId":disk_steward_core::InventoryItemId::new(),"choice":"keep"}});
        assert!(serde_json::from_value::<EditRequest>(request.clone()).is_ok());
        let mut extra = request.clone();
        extra["approved"] = serde_json::json!(true);
        assert!(serde_json::from_value::<EditRequest>(extra).is_err());
        let mut wrong_id = request;
        wrong_id["edit"]["itemId"] = serde_json::json!(ReviewId::new());
        assert!(serde_json::from_value::<EditRequest>(wrong_id).is_err());
    }
}
