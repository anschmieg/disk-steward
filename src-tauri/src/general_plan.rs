//! Typed bridge to the non-executable general editor. Profiles select fixed
//! backend-owned databases, never renderer paths or cleanup capabilities.
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use disk_steward_core::{InventoryItemId, ReviewId, RevisionId};
use disk_steward_plan_model::{EditOutcome, EditorSnapshot, FrozenReview, InventoryItem, PlanEdit};
use disk_steward_plan_store::{FixtureScale, GeneralPlanStore, ReviewSummary, StoreError};
use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Profile {
    Pilot,
    Stress,
    Live,
}
impl Profile {
    fn filename(self) -> &'static str {
        match self {
            Self::Pilot => "editor-pilot-v1.sqlite3",
            Self::Stress => "editor-stress-v1.sqlite3",
            Self::Live => "editor-live-v1.sqlite3",
        }
    }
    fn scale(self) -> FixtureScale {
        match self {
            Self::Pilot => FixtureScale::Pilot1000,
            Self::Stress => FixtureScale::Stress10000,
            Self::Live => FixtureScale::EmptyReview,
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
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ImportObservedReviewItemsRequest {
    expected_observation_id: InventoryItemId,
    expected_revision_id: RevisionId,
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

/// Imports the exact latest backend-held immediate-entry observation into the
/// separate empty live review workspace as blocked metadata only.
#[tauri::command]
pub(crate) async fn import_observed_review_items(
    request: ImportObservedReviewItemsRequest,
    state: tauri::State<'_, GeneralPlanState>,
    demo: tauri::State<'_, crate::fixture_demo::FixtureDemoState>,
    inventory: tauri::State<'_, crate::folder_inventory::FolderInventoryState>,
) -> Result<EditorSnapshot, GeneralPlanError> {
    let guard = edit_guard(&demo)?;
    let inventory = inventory.inner().clone();
    let items = inventory
        .blocked_items_for_observation(request.expected_observation_id)
        .map_err(|reason| match reason {
            "stale_observation" => GeneralPlanError {
                code: "stale_observation",
                message: "The folder observation changed. Inspect it again before importing.",
            },
            "no_importable_entries" => GeneralPlanError {
                code: "no_importable_entries",
                message: "This observation has no supported file or folder entries to import.",
            },
            "unsafe_observed_size" => GeneralPlanError {
                code: "unsafe_observed_size",
                message: "An observed logical size exceeds the review editor's safe range. Nothing was imported.",
            },
            _ => GeneralPlanError {
                code: "observation_unavailable",
                message: "The selected-folder observation is unavailable. Inspect it again.",
            },
        })?;
    let expected_revision_id = request.expected_revision_id;
    with_store(&state, Profile::Live, move |store| {
        let _guard = guard;
        append_new_observed_items(store, expected_revision_id, items)
    })
    .await
}
// A page can be imported after its duplicate subset; never overwrite richer
// comparison evidence or user decisions merely to add the remaining entries.
fn append_new_observed_items(
    store: &mut GeneralPlanStore,
    expected_revision: RevisionId,
    items: Vec<InventoryItem>,
) -> Result<EditorSnapshot, StoreError> {
    let current = store.get_plan()?;
    if current.revision_id != expected_revision {
        return Err(StoreError::DomainEdit(
            disk_steward_plan_model::EditError::StaleRevision,
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut missing = Vec::new();
    for item in items {
        if !seen.insert(item.id) {
            return Err(StoreError::DomainEdit(
                disk_steward_plan_model::EditError::InvalidInventory,
            ));
        }
        if let Some(existing) = current.items.iter().find(|row| row.item_id == item.id) {
            if existing.parent_id != item.parent_id
                || existing.kind != item.kind
                || existing.label != item.label
                || existing.logical_bytes != item.logical_bytes
            {
                return Err(StoreError::DomainEdit(
                    disk_steward_plan_model::EditError::InvalidInventory,
                ));
            }
        } else {
            missing.push(item);
        }
    }
    // append_items rechecks the revision in its IMMEDIATE write transaction.
    store.append_items(expected_revision, missing)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ImportDuplicateReviewItemsRequest {
    expected_observation_id: InventoryItemId,
    comparison_id: InventoryItemId,
    expected_revision_id: RevisionId,
}

/// Imports only native-retained matching evidence; renderer metadata cannot create groups.
#[tauri::command]
pub(crate) async fn import_duplicate_review_items(
    request: ImportDuplicateReviewItemsRequest,
    state: tauri::State<'_, GeneralPlanState>,
    demo: tauri::State<'_, crate::fixture_demo::FixtureDemoState>,
    inventory: tauri::State<'_, crate::folder_inventory::FolderInventoryState>,
    exclusions: tauri::State<'_, crate::global_exclusions::ExclusionState>,
) -> Result<EditorSnapshot, GeneralPlanError> {
    let guard = edit_guard(&demo)?;
    #[cfg(unix)]
    {
        let inventory = inventory.inner().clone();
        let exclusions = exclusions.inner().clone();
        let expected_revision = request.expected_revision_id;
        let bundle = tauri::async_runtime::spawn_blocking(move || {
            let unavailable = || GeneralPlanError {
                code: "comparison_unavailable",
                message: "Matching-file evidence changed or is no longer retained. Compare those files again before importing.",
            };
            let started = std::time::Instant::now();
            let duration = std::time::Duration::from_secs(2);
            let (revision, policy) = exclusions.summary_policy(started, duration).map_err(|_| unavailable())?;
            let bundle = inventory.duplicate_review_bundle(request.expected_observation_id, request.comparison_id, &policy, revision, started, duration).map_err(|_| unavailable())?;
            let (after_revision, after_policy) = exclusions.summary_policy(started, duration).map_err(|_| unavailable())?;
            if revision != after_revision || policy != after_policy || started.elapsed() >= duration { return Err(unavailable()); }
            Ok(bundle)
        }).await.map_err(|_| unavailable())??;
        with_store(&state, Profile::Live, move |store| {
            let _guard = guard;
            store.append_duplicate_evidence(expected_revision, bundle.0, bundle.1)
        })
        .await
    }
    #[cfg(not(unix))]
    {
        let _ = (guard, request, state, inventory, exclusions);
        Err(GeneralPlanError {
            code: "comparison_unavailable",
            message: "Matching-file evidence import is unsupported on this platform.",
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ImportPytestReviewItemsRequest {
    expected_observation_id: InventoryItemId,
    inspection_id: InventoryItemId,
    expected_revision_id: RevisionId,
}

/// Imports only native-retained pytest format evidence as blocked review items.
#[tauri::command]
pub(crate) async fn import_pytest_review_items(
    request: ImportPytestReviewItemsRequest,
    state: tauri::State<'_, GeneralPlanState>,
    demo: tauri::State<'_, crate::fixture_demo::FixtureDemoState>,
    inventory: tauri::State<'_, crate::folder_inventory::FolderInventoryState>,
    exclusions: tauri::State<'_, crate::global_exclusions::ExclusionState>,
) -> Result<EditorSnapshot, GeneralPlanError> {
    let guard = edit_guard(&demo)?;
    #[cfg(unix)]
    {
        let inventory = inventory.inner().clone();
        let exclusions = exclusions.inner().clone();
        let expected_revision = request.expected_revision_id;
        let bundle = tauri::async_runtime::spawn_blocking(move || {
            let unavailable = || GeneralPlanError {
                code: "pytest_evidence_unavailable",
                message: "Pytest format evidence changed or is no longer retained. Inspect those records again before importing.",
            };
            let started = std::time::Instant::now();
            let duration = std::time::Duration::from_secs(2);
            let (revision, policy) = exclusions.summary_policy(started, duration).map_err(|_| unavailable())?;
            let bundle = inventory.pytest_review_bundle(request.expected_observation_id, request.inspection_id, &policy, revision, started, duration).map_err(|_| unavailable())?;
            let (after_revision, after_policy) = exclusions.summary_policy(started, duration).map_err(|_| unavailable())?;
            if revision != after_revision || policy != after_policy || started.elapsed() >= duration { return Err(unavailable()); }
            // Policy loading can perform I/O; validate the retained names again
            // after it before handing metadata to the plan transaction.
            let revalidated = inventory.pytest_review_bundle(request.expected_observation_id, request.inspection_id, &after_policy, after_revision, started, duration).map_err(|_| unavailable())?;
            if revalidated != bundle || started.elapsed() >= duration { return Err(unavailable()); }
            Ok(revalidated)
        }).await.map_err(|_| unavailable())??;
        with_store(&state, Profile::Live, move |store| {
            let _guard = guard;
            store.append_pytest_evidence(expected_revision, bundle)
        })
        .await
    }
    #[cfg(not(unix))]
    {
        let _ = (guard, request, state, inventory, exclusions);
        Err(GeneralPlanError {
            code: "pytest_evidence_unavailable",
            message: "Pytest evidence import is unsupported on this platform.",
        })
    }
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AddSelectedItemOutcome {
    snapshot: Option<EditorSnapshot>,
    canceled: bool,
}

/// Adds one file's no-follow metadata to the pilot plan. The dialog, metadata
/// read, and append all run under the same native-operation guard and blocking
/// worker; cancellation returns the unchanged snapshot.
#[tauri::command]
pub(crate) async fn add_selected_general_item(
    request: RevisionRequest,
    app: tauri::AppHandle,
    state: tauri::State<'_, GeneralPlanState>,
    demo: tauri::State<'_, crate::fixture_demo::FixtureDemoState>,
) -> Result<AddSelectedItemOutcome, GeneralPlanError> {
    if request.profile != Profile::Pilot {
        return Err(GeneralPlanError {
            code: "unsupported_profile",
            message: "Adding a native file is available only in the pilot workspace.",
        });
    }
    if !demo.permits_plan_edits() {
        return Err(GeneralPlanError {
            code: "plan_in_use",
            message: "Another Disk Steward instance owns this workspace. Return to that instance to edit it.",
        });
    }
    let guard = edit_guard(&demo)?;
    let stores = Arc::clone(&state.0);
    let expected_revision_id = request.expected_revision_id;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut stores = stores.lock().map_err(|_| unavailable())?;
        let directory = stores.directory.as_ref().map_err(|error| *error)?.clone();
        let stored = stores.profiles.entry(Profile::Pilot).or_insert_with(|| {
            GeneralPlanStore::open(
                directory.join(Profile::Pilot.filename()),
                Profile::Pilot.scale(),
            )
            .map_err(public_error)
        });
        let store = stored.as_mut().map_err(|error| *error)?;

        // Reject stale UI state before opening the native chooser. append_item
        // repeats this check transactionally after metadata collection.
        let before = store.get_plan().map_err(public_error)?;
        if before.revision_id != expected_revision_id {
            return Err(public_error(StoreError::StaleRevision));
        }

        let Some(selection) = app
            .dialog()
            .file()
            .set_title("Add one file's metadata only · blocked from cleanup")
            .set_can_create_directories(false)
            .blocking_pick_file()
        else {
            return Ok(AddSelectedItemOutcome {
                snapshot: Some(before),
                canceled: true,
            });
        };
        let path = selection.into_path().map_err(|_| GeneralPlanError {
            code: "unsupported_selection",
            message: "The selected location is unsupported. Nothing was added.",
        })?;
        let item: InventoryItem =
            crate::selected_metadata::inspect_selected_file(&path).map_err(|message| {
                GeneralPlanError {
                    code: "metadata_unavailable",
                    message,
                }
            })?;
        let snapshot = store
            .append_item(expected_revision_id, item)
            .map_err(public_error)?;
        Ok(AddSelectedItemOutcome {
            snapshot: Some(snapshot),
            canceled: false,
        })
    })
    .await
    .map_err(|_| unavailable())?
}

/// Adds a blocked folder observation without learning or asserting anything
/// about its children. This remains a separate native command from file add.
#[tauri::command]
pub(crate) async fn add_selected_general_folder(
    request: RevisionRequest,
    app: tauri::AppHandle,
    state: tauri::State<'_, GeneralPlanState>,
    demo: tauri::State<'_, crate::fixture_demo::FixtureDemoState>,
) -> Result<AddSelectedItemOutcome, GeneralPlanError> {
    if request.profile != Profile::Pilot {
        return Err(GeneralPlanError {
            code: "unsupported_profile",
            message: "Adding a native folder is available only in the pilot workspace.",
        });
    }
    if !demo.permits_plan_edits() {
        return Err(GeneralPlanError {
            code: "plan_in_use",
            message: "Another Disk Steward instance owns this workspace. Return to that instance to edit it.",
        });
    }
    let guard = edit_guard(&demo)?;
    let stores = Arc::clone(&state.0);
    let expected_revision_id = request.expected_revision_id;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut stores = stores.lock().map_err(|_| unavailable())?;
        let directory = stores.directory.as_ref().map_err(|error| *error)?.clone();
        let stored = stores.profiles.entry(Profile::Pilot).or_insert_with(|| {
            GeneralPlanStore::open(
                directory.join(Profile::Pilot.filename()),
                Profile::Pilot.scale(),
            )
            .map_err(public_error)
        });
        let store = stored.as_mut().map_err(|error| *error)?;

        // Reject stale UI state before opening the native chooser. append_item
        // repeats this check transactionally after the no-follow observation.
        let before = store.get_plan().map_err(public_error)?;
        if before.revision_id != expected_revision_id {
            return Err(public_error(StoreError::StaleRevision));
        }

        let Some(selection) = app
            .dialog()
            .file()
            .set_title("Add folder metadata only · contents not inventoried")
            .set_can_create_directories(false)
            .blocking_pick_folder()
        else {
            return Ok(AddSelectedItemOutcome {
                snapshot: Some(before),
                canceled: true,
            });
        };
        let path = selection.into_path().map_err(|_| GeneralPlanError {
            code: "unsupported_selection",
            message: "The selected folder is unsupported. Nothing was added.",
        })?;
        let item = crate::selected_metadata::inspect_selected_folder(&path).map_err(|message| {
            GeneralPlanError {
                code: "metadata_unavailable",
                message,
            }
        })?;
        let snapshot = store
            .append_item(expected_revision_id, item)
            .map_err(public_error)?;
        Ok(AddSelectedItemOutcome {
            snapshot: Some(snapshot),
            canceled: false,
        })
    })
    .await
    .map_err(|_| unavailable())?
}

#[cfg(test)]
mod tests {
    #[test]
    fn duplicate_import_accepts_only_native_references_and_revision() {
        let observation = serde_json::to_value(disk_steward_core::InventoryItemId::new()).unwrap();
        let comparison = serde_json::to_value(disk_steward_core::InventoryItemId::new()).unwrap();
        let revision = serde_json::to_value(disk_steward_core::RevisionId::new()).unwrap();
        let valid = serde_json::json!({"expectedObservationId":observation,"comparisonId":comparison,"expectedRevisionId":revision});
        assert!(
            serde_json::from_value::<super::ImportDuplicateReviewItemsRequest>(valid.clone())
                .is_ok()
        );
        for (key, value) in [
            ("path", serde_json::json!("/tmp/untrusted")),
            ("items", serde_json::json!([])),
            ("duplicateSets", serde_json::json!([])),
            ("profile", serde_json::json!("pilot")),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            assert!(
                serde_json::from_value::<super::ImportDuplicateReviewItemsRequest>(invalid)
                    .is_err()
            );
        }
    }

    #[test]
    fn pytest_import_accepts_only_native_references_and_revision() {
        let observation = serde_json::to_value(disk_steward_core::InventoryItemId::new()).unwrap();
        let inspection = serde_json::to_value(disk_steward_core::InventoryItemId::new()).unwrap();
        let revision = serde_json::to_value(disk_steward_core::RevisionId::new()).unwrap();
        let valid = serde_json::json!({"expectedObservationId":observation,"inspectionId":inspection,"expectedRevisionId":revision});
        assert!(
            serde_json::from_value::<super::ImportPytestReviewItemsRequest>(valid.clone()).is_ok()
        );
        for (key, value) in [
            ("path", serde_json::json!("/tmp/untrusted")),
            ("items", serde_json::json!([])),
            ("recordCount", serde_json::json!(2)),
            ("records", serde_json::json!([])),
            ("detectorVersion", serde_json::json!("forged")),
            ("profile", serde_json::json!("pilot")),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            assert!(
                serde_json::from_value::<super::ImportPytestReviewItemsRequest>(invalid).is_err()
            );
        }
    }

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

    #[test]
    fn full_page_import_after_duplicate_subset_preserves_evidence_and_survivor() {
        use disk_steward_plan_model::{
            Blocker, Choice, DuplicateSet, EvidenceRef, InventoryItem, ItemKind, Suggestion,
        };
        let item = |label: &str| InventoryItem {
            id: InventoryItemId::new(),
            parent_id: None,
            label: label.into(),
            kind: ItemKind::File,
            logical_bytes: Some(12),
            recovery_lower_bytes: None,
            recovery_upper_bytes: None,
            storage_identity: None,
            blockers: vec![Blocker {
                code: "metadata_only".into(),
                reason: "Observed metadata only".into(),
            }],
            evidence: vec![EvidenceRef {
                id: disk_steward_core::EvidenceId::new(),
                summary: "Metadata fixture".into(),
            }],
            suggestion: Suggestion {
                choice: Choice::Keep,
                rationale: "Keep".into(),
            },
        };
        let first = item("first");
        let second = item("second");
        let third = item("third");
        let mut compared = vec![first.clone(), second.clone()];
        for item in &mut compared {
            item.blockers.push(Blocker {
                code: "read_only_comparison".into(),
                reason: "Non-executable test comparison".into(),
            });
            item.evidence.push(EvidenceRef {
                id: disk_steward_core::EvidenceId::new(),
                summary: "Comparison fixture".into(),
            });
        }
        let temp = tempfile::tempdir().unwrap();
        let mut store =
            GeneralPlanStore::open(temp.path().join("plan.sqlite"), FixtureScale::EmptyReview)
                .unwrap();
        let empty = store.get_plan().unwrap();
        let set_id = disk_steward_core::DuplicateGroupId::new();
        let imported = store
            .append_duplicate_evidence(
                empty.revision_id,
                compared,
                vec![DuplicateSet {
                    id: set_id,
                    item_ids: vec![first.id, second.id],
                    survivor_id: None,
                }],
            )
            .unwrap();
        let chosen = store
            .edit(
                imported.revision_id,
                PlanEdit::SetDuplicateSurvivor {
                    duplicate_set_id: set_id,
                    survivor_id: first.id,
                },
            )
            .unwrap()
            .snapshot;
        let after = append_new_observed_items(
            &mut store,
            chosen.revision_id,
            vec![first.clone(), second.clone(), third.clone()],
        )
        .unwrap();
        assert_eq!(after.items.len(), 3);
        assert_eq!(after.duplicates, chosen.duplicates);
        for old in &chosen.items {
            assert_eq!(
                after
                    .items
                    .iter()
                    .find(|row| row.item_id == old.item_id)
                    .unwrap(),
                old
            );
        }
        let again = append_new_observed_items(
            &mut store,
            after.revision_id,
            vec![first, second, third.clone()],
        )
        .unwrap();
        assert_eq!(again, after);
        let undo = store.undo(after.revision_id).unwrap();
        assert_eq!(undo.items, chosen.items);
        assert_eq!(undo.duplicates, chosen.duplicates);
        assert!(!undo.items.iter().any(|row| row.item_id == third.id));
        assert!(!undo.execution_available);
    }

    #[test]
    fn observed_import_request_is_only_two_typed_ids() {
        let valid = serde_json::json!({
            "expectedObservationId": InventoryItemId::new(),
            "expectedRevisionId": RevisionId::new()
        });
        assert!(serde_json::from_value::<ImportObservedReviewItemsRequest>(valid.clone()).is_ok());
        for field in ["path", "approved", "profile", "entryIds"] {
            let mut invalid = valid.clone();
            invalid[field] = serde_json::json!("forbidden");
            assert!(serde_json::from_value::<ImportObservedReviewItemsRequest>(invalid).is_err());
        }
    }
}
