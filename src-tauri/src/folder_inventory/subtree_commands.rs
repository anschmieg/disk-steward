use super::{
    subtree_job::{Progress, SubtreeJob},
    FolderInventoryState,
};
use disk_steward_core::{InventoryItemId, SummaryAttemptId};
use serde::Deserialize;
use std::{sync::Arc, time::Instant};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StartRequest {
    expected_observation_id: InventoryItemId,
    attempt_id: SummaryAttemptId,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ContinueRequest {
    expected_observation_id: InventoryItemId,
    scan_id: InventoryItemId,
    attempt_id: SummaryAttemptId,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DiscardRequest {
    expected_observation_id: InventoryItemId,
    scan_id: InventoryItemId,
}

async fn run_batch(
    folders: tauri::State<'_, FolderInventoryState>,
    control: tauri::State<'_, crate::summary_control::SummaryControlState>,
    exclusions: tauri::State<'_, crate::global_exclusions::ExclusionState>,
    observation_id: InventoryItemId,
    attempt_id: SummaryAttemptId,
    scan_id: Option<InventoryItemId>,
) -> Result<Progress, String> {
    let guard = crate::NativeCheckGuard::acquire()?;
    let registration = control
        .register(observation_id, attempt_id)
        .map_err(|_| "A metadata operation is already active.".to_owned())?;
    let sessions = Arc::clone(&folders.0);
    let exclusions = exclusions.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let registration = registration;
        let cancellation = registration.cancellation_flag();
        let started = Instant::now();
        let mut current = sessions
            .lock()
            .map_err(|_| "Folder observation is unavailable.".to_owned())?;
        let session = current
            .as_mut()
            .ok_or("Choose a folder before scanning its subtree.")?;
        if session.observation_id != observation_id
            || session.snapshot.observation_id != observation_id
        {
            return Err("Folder observation is stale. Start a new scan.".into());
        }
        if let Some(id) = scan_id {
            if session.subtree_job.as_ref().map(SubtreeJob::id) != Some(id) {
                return Err("The subtree scan is stale or no longer retained.".into());
            }
        }
        // Once work begins, uncertainty must not leave a resumable speculative cursor.
        let previous = session.subtree_job.take();
        super::check_summary_cancellation(cancellation)?;
        super::validate_session_bindings(session, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let (revision, policy) = exclusions
            .summary_policy(started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let mut job = if scan_id.is_some() {
            let job = previous.ok_or("The subtree scan is no longer retained.")?;
            if job.observation_id() != observation_id
                || job.revision() != revision
                || job.policy() != &policy
            {
                return Err("The scope or exclusions changed. Start a new subtree scan.".into());
            }
            job
        } else {
            drop(previous);
            SubtreeJob::start(session, policy, revision).map_err(str::to_owned)?
        };
        let progress = job
            .advance(cancellation, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        super::check_summary_cancellation(cancellation)?;
        super::validate_session_bindings(session, started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        let (after_revision, after_policy) = exclusions
            .summary_policy(started, super::MAX_DURATION)
            .map_err(str::to_owned)?;
        super::check_summary_cancellation(cancellation)?;
        if after_revision != job.revision() || job.policy() != &after_policy {
            return Err("Remembered exclusions changed during scanning.".into());
        }
        if progress.can_continue {
            session.subtree_job = Some(job);
        }
        Ok(progress)
    })
    .await
    .map_err(|_| "Subtree scanning did not complete reliably.".to_owned())?
}
#[tauri::command]
pub(crate) async fn start_subtree_scan(
    folders: tauri::State<'_, FolderInventoryState>,
    control: tauri::State<'_, crate::summary_control::SummaryControlState>,
    exclusions: tauri::State<'_, crate::global_exclusions::ExclusionState>,
    request: StartRequest,
) -> Result<Progress, String> {
    run_batch(
        folders,
        control,
        exclusions,
        request.expected_observation_id,
        request.attempt_id,
        None,
    )
    .await
}
#[tauri::command]
pub(crate) async fn continue_subtree_scan(
    folders: tauri::State<'_, FolderInventoryState>,
    control: tauri::State<'_, crate::summary_control::SummaryControlState>,
    exclusions: tauri::State<'_, crate::global_exclusions::ExclusionState>,
    request: ContinueRequest,
) -> Result<Progress, String> {
    run_batch(
        folders,
        control,
        exclusions,
        request.expected_observation_id,
        request.attempt_id,
        Some(request.scan_id),
    )
    .await
}
#[tauri::command]
pub(crate) async fn discard_subtree_scan(
    folders: tauri::State<'_, FolderInventoryState>,
    request: DiscardRequest,
) -> Result<(), String> {
    let guard = crate::NativeCheckGuard::acquire()?;
    let sessions = Arc::clone(&folders.0);
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut current = sessions
            .lock()
            .map_err(|_| "Folder observation is unavailable.".to_owned())?;
        let session = current
            .as_mut()
            .ok_or("Folder observation is unavailable.")?;
        discard_exact(session, request.expected_observation_id, request.scan_id)
            .map_err(str::to_owned)
    })
    .await
    .map_err(|_| "Subtree scan state could not be released.".to_owned())?
}
fn discard_exact(
    session: &mut super::Session,
    observation_id: InventoryItemId,
    scan_id: InventoryItemId,
) -> Result<(), &'static str> {
    if session.observation_id != observation_id || session.snapshot.observation_id != observation_id
    {
        return Err("Folder observation is stale.");
    }
    if session
        .subtree_job
        .as_ref()
        .is_some_and(|job| job.id() != scan_id)
    {
        return Err("A different subtree scan is retained.");
    }
    session.subtree_job.take();
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_discard_preserves_job_and_scope_changes_release_it() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..3 {
            std::fs::write(root.path().join(format!("file-{index}")), b"fixture").unwrap();
        }
        let (mut session, _) =
            super::super::selected_root_session(root.path(), 1, super::super::MAX_DURATION)
                .unwrap();
        let job = SubtreeJob::start(
            &session,
            crate::global_exclusions::SummaryExclusionPolicy::empty_for_tests(),
            disk_steward_core::RevisionId::new(),
        )
        .unwrap();
        let id = job.id();
        let observation = session.observation_id;
        session.subtree_job = Some(job);
        assert!(discard_exact(&mut session, InventoryItemId::new(), id).is_err());
        assert!(discard_exact(&mut session, observation, InventoryItemId::new()).is_err());
        assert_eq!(session.subtree_job.as_ref().map(SubtreeJob::id), Some(id));
        discard_exact(&mut session, observation, id).unwrap();
        assert!(session.subtree_job.is_none());
        session.subtree_job = Some(
            SubtreeJob::start(
                &session,
                crate::global_exclusions::SummaryExclusionPolicy::empty_for_tests(),
                disk_steward_core::RevisionId::new(),
            )
            .unwrap(),
        );
        super::super::refresh_session(
            &mut session,
            super::super::RefreshInventoryRequest {
                expected_observation_id: observation,
            },
            1,
            super::super::MAX_DURATION,
        )
        .unwrap();
        assert!(session.subtree_job.is_none());
        assert_eq!(
            std::fs::read(root.path().join("file-0")).unwrap(),
            b"fixture"
        );
    }

    #[test]
    fn renderer_can_supply_only_exact_typed_scan_references() {
        let id = serde_json::to_value(InventoryItemId::new()).unwrap();
        let attempt = serde_json::to_value(SummaryAttemptId::new()).unwrap();
        assert!(serde_json::from_value::<StartRequest>(
            serde_json::json!({"expectedObservationId":id,"attemptId":attempt,"path":"/tmp"})
        )
        .is_err());
        assert!(serde_json::from_value::<ContinueRequest>(
            serde_json::json!({"expectedObservationId":id,"attemptId":attempt,"scanId":id})
        )
        .is_ok());
        assert!(serde_json::from_value::<DiscardRequest>(
            serde_json::json!({"expectedObservationId":id,"scanId":id,"approved":true})
        )
        .is_err());
    }
}
