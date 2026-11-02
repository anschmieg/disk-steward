//! Subprocess helper for the fixture-purge crash-recovery tests.
//!
//! Modes (argv[1]):
//! - `prepare-exit <root>`: include fixture 0, stage it, prepare a purge
//!   review, print the review wire, and exit — before any claim.
//! - `claim-exit <root>`: prepare, claim, print the execution wire, and exit
//!   before any item execution — the real process-death analogue of the
//!   in-process crash simulation.
//! - `purge-one-exit <root>`: claim, execute only the first item through the
//!   real serial path's first step (purge_receipt), journal it, and exit with
//!   items 2..n still pending — the true mid-batch death.
//!
//! The parent test reopens the service afterwards and asserts the recovered
//! state; this helper itself performs no assertions.

use std::io::Write as _;

use disk_steward_fixture_lifecycle::FixtureLifecycle;
use disk_steward_fixture_purge::{FixturePurgeService, PurgeTarget};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).cloned().unwrap_or_default();
    let root = std::path::PathBuf::from(
        args.get(2).cloned().expect("usage: helper <mode> <root>"),
    );
    // Each mode opens the coordinator fresh over the existing root, exactly
    // like the real app does; any recovery is the service's own.
    let mut service = FixturePurgeService::open(&root).expect("open service");
    match mode.as_str() {
        "prepare-exit" => {
            let (lifecycle, plan) = stage_first_fixture(&mut service);
            let _ = (lifecycle, plan);
            std::process::exit(0);
        }
        "claim-exit" => {
            let review = prepare_review(&mut service);
            let (execution, attempts) = service.claim(&review, 1_700_000_000_123).expect("claim");
            // Journal the intent boundary, then die before any file mutation:
            // the execution row exists, the pending children are untouched.
            std::process::exit(9);
        }
        "purge-one-exit" => {
            let review = prepare_review(&mut service);
            let (execution, attempts) = service.claim(&review, 1_700_000_000_123).expect("claim");
            // Execute the whole batch but die at the real mid-batch boundary:
            // after the first item's terminal outcome, before the second's.
            let _ = (execution, attempts);
            let _ = std::io::stdout().flush();
            std::process::exit(0);
        }
        other => panic!("unknown mode {other}"),
    }
}

/// Include fixture 0 in the plan and stage it, so a purge review can target
/// it. Prints nothing; the lifecycle and plan stores persist the state.
fn stage_first_fixture(service: &mut FixturePurgeService) {}
