//! Subprocess helper for the fixture-purge crash-recovery tests.
//!
//! Each mode performs one real coordinator step in a fresh process and exits
//! at a named boundary, so the parent test can prove persistence and honest
//! recovery across a real process death (contract fixture-purge-v1:
//! subprocess tests at claim/execute boundaries).
//!
//! Modes:
//! - `prepare-exit <root>`: include fixture 0, stage it, prepare the purge
//!   review, print the review wire, exit(0) — before any claim.
//! - `claim-exit <root> <review-wire>`: claim, print the execution wire,
//!   exit(9) — after the journal, before any item execution.
//! - `execute-exit <root> <review-wire>`: claim and execute the full batch,
//!   exit(0).

use std::path::{Path, PathBuf};

use disk_steward_core::{ActionId, AttemptId, ReviewId, RevisionId};
use disk_steward_fixture_lifecycle::FixtureLifecycle;
use disk_steward_fixture_purge::{FixturePurgeService, PurgeTarget};
use disk_steward_review_plan::{Decision, ReviewPlanStore};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).cloned().unwrap_or_default();
    let root = PathBuf::from(
        args.get(2)
            .cloned()
            .expect("usage: helper <mode> <root> [review-wire]"),
    );
    match mode.as_str() {
        "prepare-exit" => {
            let (mut lifecycle, plan, targets, revision) = seed_and_stage(&root);
            let mut service = FixturePurgeService::open(&root).expect("open");
            let review = service
                .prepare_purge_review(&mut lifecycle, &plan, revision, targets)
                .expect("prepare");
            let wire = serde_json::to_string(&review.review_id).expect("review wire");
            println!("{wire}");
            std::process::exit(0);
        }
        "claim-exit" | "execute-exit" => {
            let review_wire = args.get(3).cloned().expect("review wire");
            let review_id: ReviewId = serde_json::from_str(&review_wire).expect("review id");
            let mut service = FixturePurgeService::open(&root).expect("open");
            let (mut lifecycle, plan) = reopen_stores(&root);
            if mode == "claim-exit" {
                // Claim only: the journal rows exist, no item has run.
                let prepared = prepared_from(&service, &review_id);
                let now = i64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("clock after epoch")
                        .as_millis(),
                )
                .expect("bounded clock");
                service
                    .claim(&prepared, now)
                    .expect("durable claim before crash boundary");
                std::process::exit(9);
            }
            let result = service
                .claim_and_execute(&mut lifecycle, &plan, review_id, |_| true)
                .expect("claim and execute");
            println!("{}", serde_json::to_string(&result).expect("result wire"));
            std::process::exit(0);
        }
        other => panic!("unknown mode {other}"),
    }
}

fn prepared_from(
    service: &FixturePurgeService,
    review_id: &ReviewId,
) -> disk_steward_fixture_purge::PreparedPurgeReview {
    service
        .read_stored_review(review_id)
        .expect("stored review")
}

fn seed_and_stage(
    root: &Path,
) -> (
    FixtureLifecycle,
    ReviewPlanStore,
    Vec<PurgeTarget>,
    RevisionId,
) {
    let mut plan = ReviewPlanStore::open(root.join("review-example-v1.sqlite3")).expect("plan");
    let plan_view = plan.get_plan().expect("plan view");
    let ids: [ActionId; 5] = plan_view
        .actions
        .iter()
        .map(|a| a.id)
        .collect::<Vec<_>>()
        .try_into()
        .expect("five actions");
    let (mut lifecycle, _) = FixtureLifecycle::create(root, ids).expect("lifecycle");
    let mut revision = plan_view.revision_id;
    let mut targets = Vec::new();
    for id in ids.iter().take(2) {
        let edited = plan.edit(revision, *id, Decision::Include).expect("edit");
        revision = edited.revision_id;
        let receipt = lifecycle.stage(*id, AttemptId::new()).expect("stage");
        targets.push(PurgeTarget {
            action_id: *id,
            receipt_id: receipt,
        });
    }
    (lifecycle, plan, targets, revision)
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
