#![cfg(all(unix, any(target_os = "linux", target_os = "macos")))]

use disk_steward_demo_execution::DemoExecutionService;
use disk_steward_fixture_lifecycle::{FixtureLifecycle, FixtureStatus};
use disk_steward_review_plan::{Decision, ReviewPlanStore};
use rusqlite::Connection;
use std::os::unix::fs::PermissionsExt;

const PLAN_DB: &str = "review-pytest-920-v1.sqlite3";
const CONTROL_DB: &str = "demo-execution-pytest-920-v1.sqlite3";
const LEDGER_DB: &str = "approval-ledger-pytest-920-v1.sqlite3";

#[test]
fn changed_profile_or_plan_during_confirmation_cannot_claim_or_mutate() {
    for change in ["control_identity", "plan_revision", "session_binding"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut service = DemoExecutionService::open_pytest_corpus(dir.path()).unwrap();
        let initial = service.start_demo().unwrap();
        let session = initial.session_id.unwrap();
        let (_, before) = FixtureLifecycle::reopen_pytest_corpus(dir.path(), session).unwrap();
        let mut editor = ReviewPlanStore::open_pytest_corpus(dir.path().join(PLAN_DB)).unwrap();
        let plan = editor.get_plan().unwrap();
        let plan = editor
            .edit(plan.revision_id, plan.actions[0].id, Decision::Include)
            .unwrap();
        let review = service.prepare_stage(plan.revision_id).unwrap();
        let result = service.confirm_and_execute(review.review_id, |shown| {
            assert_eq!(shown.selected_bytes, 70);
            assert_eq!(shown.items.len(), 1);
            if change == "control_identity" {
                let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
                db.pragma_update(None, "application_id", 0x4453_4445_i64)
                    .unwrap();
            } else if change == "session_binding" {
                let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
                let foreign = serde_json::to_string(&disk_steward_core::SessionId::new()).unwrap();
                db.execute(
                    "UPDATE control SET session_id=?1 WHERE singleton=1",
                    [foreign],
                )
                .unwrap();
            } else {
                editor
                    .edit(plan.revision_id, plan.actions[1].id, Decision::Include)
                    .unwrap();
            }
            true
        });
        assert!(
            result.is_err(),
            "changed {change} must invalidate confirmation"
        );
        let (_, after) = FixtureLifecycle::reopen_pytest_corpus(dir.path(), session).unwrap();
        assert_eq!(
            after, before,
            "confirmation cannot add newly discovered targets"
        );
        assert!(after
            .fixtures
            .iter()
            .all(|file| file.status == FixtureStatus::Available));
        let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
        let executions: i64 = db
            .query_row("SELECT count(*) FROM executions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(executions, 0);
        let ledger = Connection::open(dir.path().join(LEDGER_DB)).unwrap();
        let approvals: i64 = ledger
            .query_row("SELECT count(*) FROM approvals", [], |row| row.get(0))
            .unwrap();
        let intents: i64 = ledger
            .query_row("SELECT count(*) FROM execution_intents", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!((approvals, intents), (0, 0));
    }
}

#[test]
fn declining_corpus_stage_preserves_the_unclaimed_review_and_both_records() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut service = DemoExecutionService::open_pytest_corpus(dir.path()).unwrap();
    let initial = service.start_demo().unwrap();
    let mut editor = ReviewPlanStore::open_pytest_corpus(dir.path().join(PLAN_DB)).unwrap();
    let plan = editor.get_plan().unwrap();
    let plan = editor
        .edit(plan.revision_id, plan.actions[1].id, Decision::Include)
        .unwrap();
    let review = service.prepare_stage(plan.revision_id).unwrap();
    assert!(service
        .confirm_and_execute(review.review_id, |_| false)
        .is_err());
    assert_eq!(service.status().unwrap().fixtures, initial.fixtures);
    let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
    let claimed: i64 = db
        .query_row("SELECT claimed FROM prepared_reviews", [], |row| row.get(0))
        .unwrap();
    assert_eq!(claimed, 0);
    let result = service
        .confirm_and_execute(review.review_id, |shown| {
            assert_eq!(shown.selected_bytes, 42);
            true
        })
        .unwrap();
    assert_eq!(result.state, "finished");
    assert_eq!(result.staged_bytes, 42);
    assert!(service
        .confirm_and_execute(review.review_id, |_| panic!(
            "replay must refuse before confirmation"
        ))
        .is_err());
}

#[test]
fn changed_session_profile_or_revision_during_purge_cannot_claim_or_unlink() {
    for change in ["control_identity", "plan_revision", "session_binding"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut service = DemoExecutionService::open_pytest_corpus(dir.path()).unwrap();
        let initial = service.start_demo().unwrap();
        let session = initial.session_id.unwrap();
        let mut editor = ReviewPlanStore::open_pytest_corpus(dir.path().join(PLAN_DB)).unwrap();
        let plan = editor.get_plan().unwrap();
        let plan = editor
            .edit(plan.revision_id, plan.actions[1].id, Decision::Include)
            .unwrap();
        let stage = service.prepare_stage(plan.revision_id).unwrap();
        service
            .confirm_and_execute(stage.review_id, |_| true)
            .unwrap();
        let (_, before) = FixtureLifecycle::reopen_pytest_corpus(dir.path(), session).unwrap();
        let target = &before.fixtures[1];
        let review = service
            .prepare_purge_review(
                plan.revision_id,
                vec![disk_steward_fixture_purge::PurgeTarget {
                    action_id: target.action_id,
                    receipt_id: target.receipt_id.unwrap(),
                }],
            )
            .unwrap();
        let result = service.confirm_purge_execution(review.review_id, |_| {
            if change == "control_identity" {
                let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
                db.pragma_update(None, "application_id", 0x4453_4445_i64)
                    .unwrap();
            } else if change == "session_binding" {
                let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
                let foreign = serde_json::to_string(&disk_steward_core::SessionId::new()).unwrap();
                db.execute(
                    "UPDATE control SET session_id=?1 WHERE singleton=1",
                    [foreign],
                )
                .unwrap();
            } else {
                editor
                    .edit(plan.revision_id, plan.actions[0].id, Decision::Include)
                    .unwrap();
            }
            true
        });
        assert!(
            result.is_err(),
            "changed {change} must invalidate purge confirmation"
        );
        let (_, after) = FixtureLifecycle::reopen_pytest_corpus(dir.path(), session).unwrap();
        assert_eq!(after, before);
        let purge = Connection::open(dir.path().join("pytest-corpus-purge-v1.sqlite3")).unwrap();
        let executions: i64 = purge
            .query_row("SELECT count(*) FROM purge_executions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(executions, 0);
        let state: String = purge
            .query_row("SELECT state FROM purge_reviews", [], |row| row.get(0))
            .unwrap();
        assert_eq!(state, "prepared");
        let ledger = Connection::open(dir.path().join(LEDGER_DB)).unwrap();
        let approvals: i64 = ledger
            .query_row("SELECT count(*) FROM approvals", [], |row| row.get(0))
            .unwrap();
        let intents: i64 = ledger
            .query_row("SELECT count(*) FROM execution_intents", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            (approvals, intents),
            (1, 1),
            "only the earlier stage was approved"
        );
    }
}
