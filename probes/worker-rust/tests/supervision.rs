use std::process::{Command, Stdio};

fn probe() -> Command {
    Command::new(env!("CARGO_BIN_EXE_disk-steward-worker-probe"))
}

#[test]
fn default_is_dry_run_and_starts_no_child() {
    let output = probe().output().expect("run dry-run probe");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("dry run"));
    assert!(!stdout.contains("case="));
}

#[test]
fn run_supervises_three_fixed_child_cases() {
    let output = probe().arg("--run").output().expect("run fixed probe");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("case=flood stop=OutputLimit"), "{stdout}");
    assert!(stdout.contains("case=stall stop=Deadline"), "{stdout}");
    assert!(
        stdout.contains("case=cancel stop=Canceled bytes=1"),
        "{stdout}"
    );
    assert_eq!(stdout.matches("reader_joined=true").count(), 3, "{stdout}");
    assert_eq!(stdout.matches("child_exit=").count(), 3, "{stdout}");
    assert!(stdout.contains("bytes=65537"), "{stdout}");
    assert!(
        stdout.contains("case=stall stop=Deadline bytes=1"),
        "{stdout}"
    );
}

#[test]
fn store_worker_records_stopped_only_after_joined_child_shutdown() {
    let output = probe()
        .arg("--run-store-worker")
        .output()
        .expect("run store worker");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(
        stdout.contains("store_state=NeedsReconciliation"),
        "{stdout}"
    );
    assert!(stdout.contains("worker_pid="), "{stdout}");
    assert!(stdout.contains("child_exit="), "{stdout}");
    assert!(stdout.contains("reader_joined=true bytes=1"), "{stdout}");
    assert!(stdout.contains("cleanup=not_authorized"), "{stdout}");
}

#[test]
fn creator_process_death_allows_read_only_store_reinspection() {
    let output = probe()
        .arg("--run-store-reinspect")
        .output()
        .expect("reinspect fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("store_state=NeedsReconciliation"),
        "{stdout}"
    );
    assert!(stdout.contains("creator_stopped=true"), "{stdout}");
    assert!(stdout.contains("reader_joined=true"), "{stdout}");
    assert!(stdout.contains("cleanup=not_authorized"), "{stdout}");
    assert!(
        stdout.contains("process_death_not_power_loss=true"),
        "{stdout}"
    );
}

#[test]
fn indexed_store_worker_reopens_as_reconciliation_without_cleanup_authority() {
    let output = probe()
        .arg("--run-indexed-store-worker")
        .output()
        .expect("run indexed worker");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    for evidence in [
        "worker_joined=true",
        "registered_stopped=true",
        "reopened_reconciliation=true",
        "prior_stopped=true",
        "reserved_bytes=131072",
        "reservation_retry_refused=true",
        "cleanup=not_authorized",
    ] {
        assert!(report.contains(evidence), "{report}");
    }
    let refused = probe()
        .args(["--run-indexed-store-worker", "extra"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    let combined = probe()
        .args([
            "--run-indexed-store-worker",
            "--run-store-cleanup",
            "--confirm-own-store-cleanup",
        ])
        .output()
        .unwrap();
    assert!(!combined.status.success());
    assert!(combined.stdout.is_empty());
}

#[test]
fn arbitrary_parent_arguments_are_rejected() {
    let output = probe()
        .arg("/bin/sh")
        .output()
        .expect("run invalid-argument check");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("accepted arguments"));
}

#[test]
fn private_dispatcher_rejects_unknown_modes_and_extra_arguments() {
    for args in [
        vec!["--child"],
        vec!["--child", "/bin/sh"],
        vec!["--child", "stall", "unexpected"],
        vec!["--run", "unexpected"],
        vec!["--run-store-worker", "unexpected"],
        vec!["--run-store-reinspect", "unexpected"],
        vec!["--child", "store-abandon", "unexpected"],
        vec!["--run-store-cleanup"],
    ] {
        let output = probe()
            .args(&args)
            .output()
            .expect("reject invalid arguments");
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn cleanup_requires_both_exact_confirmation_arguments_without_starting_work() {
    let output = probe()
        .arg("--run-store-cleanup")
        .output()
        .expect("reject unconfirmed cleanup");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing store cleanup"));
}

#[test]
fn explicitly_confirmed_cleanup_uses_real_child_shutdown() {
    let output = probe()
        .args(["--run-store-cleanup", "--confirm-own-store-cleanup"])
        .output()
        .expect("run confirmed own-fixture cleanup");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("store_removed=true"), "{report}");
    assert!(
        report.contains("retained_parent_journal=cleanup.journal"),
        "{report}"
    );
    assert!(report.contains("worker_pid="), "{report}");
}

#[test]
fn fixed_payload_command_caps_writes_and_rejects_extra_authority() {
    for args in [
        vec!["--run-store-payload-fixture", "/tmp/caller-path"],
        vec![
            "--run-store-payload-fixture",
            "--run-store-cleanup",
            "--confirm-own-store-cleanup",
        ],
    ] {
        let output = probe()
            .args(args)
            .output()
            .expect("reject extra payload arguments");
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let output = probe()
        .arg("--run-store-payload-fixture")
        .output()
        .expect("run owned payload fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(),
        "payload_written=32768 payload_limit=32768 cap_reached=true state=NeedsReconciliation cleanup=not_authorized\n");
}

#[test]
fn fixed_worker_launches_discard_parent_environment() {
    const SENTINEL: &str = "DISK_STEWARD_WORKER_ENV_SENTINEL";
    // Negative control proves the private child actually detects inheritance;
    // the sentinel value must never be copied into diagnostics.
    let direct = probe()
        .args(["--child", "stall"])
        .env(SENTINEL, "synthetic-parent-only-value")
        .output()
        .expect("run inheritance negative control");
    assert!(!direct.status.success());
    assert!(direct.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&direct.stderr).contains("synthetic-parent-only-value"));
    for flag in [
        "--run",
        "--run-store-worker",
        "--run-store-reinspect",
        "--run-indexed-store-worker",
    ] {
        let output = probe()
            .arg(flag)
            .env(SENTINEL, "synthetic-parent-only-value")
            .output()
            .expect("run environment-cleared fixed parent");
        assert!(
            output.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.contains("reader_joined=true") || stdout.contains("worker_joined=true"),
            "{flag}: {stdout}"
        );
        assert!(!stdout.contains("synthetic-parent-only-value"));
    }
}

#[test]
fn indexed_store_descriptor_handoff_requires_exact_capability_and_arguments() {
    for args in [
        vec!["--run-indexed-store-handoff", "/tmp/caller-path"],
        vec!["--run-indexed-store-handoff", "--confirm-own-store-cleanup"],
        vec!["--child", "store-handoff", "extra"],
    ] {
        let output = probe().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let missing = probe()
        .args(["--child", "store-handoff"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(
        missing.stdout.is_empty(),
        "no readiness without directory capability"
    );

    let output = probe()
        .arg("--run-indexed-store-handoff")
        .env(
            "DISK_STEWARD_WORKER_ENV_SENTINEL",
            "synthetic-handoff-parent-only",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    for evidence in [
        "descriptor_handoff=true",
        "worker_joined=true",
        "registered_stopped=true",
        "reopened_reconciliation=true",
        "cleanup=not_authorized",
    ] {
        assert!(report.contains(evidence), "{report}");
    }
    assert!(!report.contains("synthetic-handoff-parent-only"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-handoff-parent-only"));
}
