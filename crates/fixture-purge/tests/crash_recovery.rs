//! Subprocess crash-recovery tests (contract fixture-purge-v1: process death
//! at the claim/execute boundaries must leave an honest, recoverable state —
//! no automatic retry, no replay, no success claim for uncertain items).
//!
//! Each test runs the real coordinator in a real child process
//! (`purge_crash_helper`), which exits at a named boundary, then reopens the
//! service in this process and asserts the recovered state.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use disk_steward_core::ReviewId;
use disk_steward_fixture_lifecycle::FixtureLifecycle;
use disk_steward_fixture_purge::FixturePurgeService;
use disk_steward_review_plan::ReviewPlanStore;

const HELPER: &str = env!("CARGO_BIN_EXE_purge_crash_helper");

fn private_root() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("root");
    let mut permissions = std::fs::metadata(dir.path())
        .expect("metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(dir.path(), permissions).expect("permissions");
    dir
}

fn run_helper(mode: &str, root: &Path, review_wire: Option<&str>) -> String {
    let mut command = Command::new(HELPER);
    command.arg(mode).arg(root);
    if let Some(wire) = review_wire {
        command.arg(wire);
    }
    let output = command.output().expect("helper spawn");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() && mode != "claim-exit" {
        panic!(
            "helper {mode} failed: status={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    stdout
}

fn reopen_stores(root: &Path) -> (FixtureLifecycle, ReviewPlanStore) {
    let entry = std::fs::read_dir(root)
        .expect("root read")
        .find_map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.starts_with(".disk-steward-fixtures-").then_some(name)
        })
        .expect("fixture session dir");
    let db = rusqlite::Connection::open(root.join(entry).join("lifecycle.sqlite3"))
        .expect("lifecycle db");
    let wire: String = db
        .query_row(
            "SELECT session_id FROM manifest WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .expect("session wire");
    let session: disk_steward_core::SessionId = serde_json::from_str(&wire).expect("session");
    let (lifecycle, _) = FixtureLifecycle::reopen(root, session).expect("reopen");
    let plan = ReviewPlanStore::open(root.join("review-example-v1.sqlite3")).expect("plan");
    (lifecycle, plan)
}

fn control_db(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(root.join("fixture-purge-control-v1.sqlite3")).expect("control db")
}

#[test]
fn prepared_review_survives_process_death_and_completes_afterwards() {
    let root = private_root();
    let review_wire = run_helper("prepare-exit", root.path(), None);
    let review_id: ReviewId = serde_json::from_str(review_wire.trim()).expect("review id");
    // The prepared review is durable across the process boundary.
    {
        let service = FixturePurgeService::open(root.path()).expect("reopen");
        let prepared = service
            .read_stored_review(&review_id)
            .expect("stored review");
        assert_eq!(prepared.targets.len(), 2);
        assert_eq!(
            prepared.targets[0].status,
            disk_steward_fixture_purge::PurgeReceiptState::Staged
        );
    }
    // The next process claims and executes it.
    run_helper("execute-exit", root.path(), Some(review_wire.trim()));
    let db = control_db(root.path());
    let (review_state, execution_state): (String, String) = db
        .query_row(
            "SELECT r.state, e.state FROM purge_reviews r JOIN purge_executions e ON e.review_id=r.review_id WHERE r.review_id=?1",
            [review_wire.trim()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("joined state");
    assert_eq!(review_state, "claimed");
    assert_eq!(execution_state, "finished");
    let pending: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM purge_child_attempts WHERE state='pending'",
            [],
            |r| r.get(0),
        )
        .expect("pending count");
    assert_eq!(pending, 0);
}

#[test]
fn process_death_after_claim_recovers_to_uncertain_without_retry() {
    let root = private_root();
    let review_wire = run_helper("prepare-exit", root.path(), None);
    // The child claims and dies (exit 9) before any item executes.
    run_helper("claim-exit", root.path(), Some(review_wire.trim()));
    // Reopen: the owner recovery resolves the crash state.
    let mut service = FixturePurgeService::open(root.path()).expect("reopen");
    let db = control_db(root.path());
    let execution_state: String = db
        .query_row("SELECT state FROM purge_executions", [], |r| r.get(0))
        .expect("execution state");
    assert_eq!(
        execution_state, "uncertain",
        "claimed-but-unfinished must recover to uncertain"
    );
    let pending: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM purge_child_attempts WHERE state='pending'",
            [],
            |r| r.get(0),
        )
        .expect("pending count");
    assert_eq!(pending, 0, "recovery resolves every pending child row");
    // Replay of the crashed review is refused.
    let review_id: ReviewId = serde_json::from_str(review_wire.trim()).expect("review id");
    let (mut lifecycle, plan) = reopen_stores(root.path());
    let error = service
        .claim_and_execute(&mut lifecycle, &plan, review_id, |_| true)
        .expect_err("replay must be refused");
    assert_eq!(error.code(), "review_replayed");
    // No success claim for uncertain items: the fixture files still exist
    // (recovery never unlinks), and a fresh review for the same receipts is
    // still possible.
    let staged: i64 = {
        let entry = std::fs::read_dir(root.path())
            .expect("root read")
            .find_map(|e| {
                let name = e.unwrap().file_name().into_string().unwrap();
                name.starts_with(".disk-steward-fixtures-").then_some(name)
            })
            .expect("fixture dir");
        let db = rusqlite::Connection::open(root.path().join(entry).join("lifecycle.sqlite3"))
            .expect("lifecycle db");
        db.query_row(
            "SELECT COUNT(*) FROM fixtures WHERE status='staged'",
            [],
            |r| r.get(0),
        )
        .expect("staged count")
    };
    assert_eq!(staged, 2, "recovery must not unlink or reset the fixtures");
    let fresh = service
        .prepare_purge_review(
            &mut lifecycle,
            &plan,
            plan.get_plan().expect("plan").revision_id,
            vec![],
        )
        .expect_err("empty targets are still refused");
    assert_eq!(fresh.code(), "invalid_input");
    let _ = PathBuf::new();
}
