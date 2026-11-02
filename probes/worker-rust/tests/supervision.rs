use std::process::Command;

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
