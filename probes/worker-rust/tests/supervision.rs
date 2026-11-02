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
fn run_supervises_only_the_two_fixed_child_cases() {
    let output = probe().arg("--run").output().expect("run fixed probe");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("case=flood stop=OutputLimit"), "{stdout}");
    assert!(stdout.contains("case=stall stop=Deadline"), "{stdout}");
    assert_eq!(stdout.matches("reader_joined=true").count(), 2, "{stdout}");
    assert_eq!(stdout.matches("child_exit=").count(), 2, "{stdout}");
    assert!(stdout.contains("bytes=65537"), "{stdout}");
    assert!(
        stdout.contains("case=stall stop=Deadline bytes=1"),
        "{stdout}"
    );
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
    ] {
        let output = probe()
            .args(&args)
            .output()
            .expect("reject invalid arguments");
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}
