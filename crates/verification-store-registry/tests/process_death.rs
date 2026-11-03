#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rustix::fs::{fstatvfs, open, openat, Mode, OFlags};
use tempfile::Builder;
use verification_store_registry::{RegistryError, StoreState, VerificationStoreRegistry};

const STORE_ID: &str = "0123456789abcdef0123456789abcdef";
const RESERVATION: u64 = 128 * 1024;
const MAX_OUTPUT: usize = 1024;
const READY_COMMITTED: &[u8] = b"REGISTRY_CHILD_READY:committed\n";
const READY_PARTIAL: &[u8] = b"REGISTRY_CHILD_READY:partial\n";

#[test]
fn committed_reservation_survives_process_death_before_payload() {
    parent_case("committed", false);
}

#[test]
fn committed_reservation_survives_process_death_after_partial_payload() {
    parent_case("partial", true);
}

fn parent_case(phase: &str, expect_payload: bool) {
    let readiness_deadline = Instant::now() + Duration::from_secs(5);
    let fixture = Builder::new()
        .prefix("disk-steward-registry-crash-")
        .tempdir()
        .expect("create disposable task root");
    fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700))
        .expect("set private task root mode");
    let root = fixture.path().canonicalize().expect("canonical task root");
    let root_meta = fs::symlink_metadata(&root).expect("task root metadata");
    let parent_meta = fs::symlink_metadata(root.parent().unwrap()).expect("task parent metadata");

    let spawn_result = spawn_child(&root, root_meta.dev(), root_meta.ino(), phase);
    let (mut child, receiver, reader) = match spawn_result {
        Ok(parts) => parts,
        Err(error) => panic!("could not start process-death child: {error}"),
    };

    // Every path after spawn reaches cleanup before asserting, including readiness,
    // lease-check, kill, wait, and reader-join failures.
    let ready_event =
        receiver.recv_timeout(readiness_deadline.saturating_duration_since(Instant::now()));
    let readiness_ok = matches!(
        &ready_event,
        Ok(Ok(marker)) if marker.as_slice() == if expect_payload { READY_PARTIAL } else { READY_COMMITTED }
    );
    let lease_result = if readiness_ok {
        VerificationStoreRegistry::open(root.join("registry.sqlite"))
            .err()
            .map(|e| matches!(e, RegistryError::LeaseHeld))
            .unwrap_or(false)
    } else {
        false
    };
    let alive_before_kill = child.try_wait();
    let kill_result = child.kill();
    let wait_result = child.wait();
    let reader_result = reader.join();

    assert!(readiness_ok, "child readiness failed: {ready_event:?}");
    assert!(lease_result, "registry lease was not held at readiness");
    assert!(
        matches!(alive_before_kill, Ok(None)),
        "child was not confirmed live: {alive_before_kill:?}"
    );
    assert!(kill_result.is_ok(), "failed to kill child: {kill_result:?}");
    assert!(
        matches!(wait_result, Ok(status) if !status.success()),
        "child was not reaped after termination: {wait_result:?}"
    );
    let output = reader_result.expect("stdout reader panicked");
    assert!(output.is_ok(), "stdout read failed: {output:?}");
    let output = output.unwrap();
    assert!(output.len() <= MAX_OUTPUT, "child stdout exceeded bound");

    let mut registry = VerificationStoreRegistry::open(root.join("registry.sqlite"))
        .expect("reopen registry after child death");
    let record = registry.get(STORE_ID).expect("read persisted row").unwrap();
    assert_eq!(record.reserved_bytes, RESERVATION);
    assert_eq!(
        (
            record.parent_dev,
            record.parent_ino,
            record.root_dev,
            record.root_ino
        ),
        (
            parent_meta.dev(),
            parent_meta.ino(),
            root_meta.dev(),
            root_meta.ino()
        )
    );
    assert_eq!(record.state, StoreState::NeedsReconciliation);
    assert_eq!(record.prior_state, Some(StoreState::Registered));
    assert!(matches!(
        registry.reserve(STORE_ID, RESERVATION, RESERVATION + 1),
        Err(RegistryError::ReconciliationRequired)
    ));

    let payload = root.join("partial.bin");
    if expect_payload {
        let metadata = fs::symlink_metadata(&payload).expect("partial payload exists");
        assert!(metadata.is_file() && !metadata.file_type().is_symlink());
        assert_eq!(metadata.mode() & 0o7777, 0o600);
        assert_eq!(metadata.len(), 5);
        assert_eq!(metadata.uid(), root_meta.uid());
        assert_eq!(metadata.nlink(), 1);
        let mut bytes = Vec::new();
        fs::File::open(payload)
            .unwrap()
            .take(6)
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"PACKf");
    } else {
        assert!(!payload.exists(), "pre-payload phase created payload");
    }
}

type OutputResult = Result<Vec<u8>, String>;
type ChildRun = (
    Child,
    mpsc::Receiver<OutputResult>,
    JoinHandle<OutputResult>,
);

fn spawn_child(root: &Path, root_dev: u64, root_ino: u64, phase: &str) -> Result<ChildRun, String> {
    let mut child = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .arg("--ignored")
        .arg("--exact")
        .arg("process_death_child")
        .arg("--nocapture")
        .env_clear()
        .env("DISK_STEWARD_TASK_ROOT", root)
        .env(
            "DISK_STEWARD_EXPECTED_DEVINO",
            format!("{root_dev}:{root_ino}"),
        )
        .env("DISK_STEWARD_FIXED_PHASE", phase)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        "child stdout pipe unavailable".to_string()
    })?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let expected = if phase == "partial" {
        READY_PARTIAL
    } else {
        READY_COMMITTED
    }
    .to_vec();
    let reader = match thread::Builder::new()
        .name("registry-child-stdout".into())
        .spawn(move || read_bounded_ready(stdout, sender, expected))
    {
        Ok(thread) => thread,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("start bounded stdout reader: {error}"));
        }
    };
    Ok((child, receiver, reader))
}

fn read_bounded_ready(
    mut stdout: impl Read,
    sender: mpsc::SyncSender<Result<Vec<u8>, String>>,
    expected: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let mut all = Vec::new();
    let mut buf = [0u8; 128];
    let mut sent = false;
    loop {
        let remaining = MAX_OUTPUT - all.len();
        if remaining == 0 {
            return Err("child stdout reached its bound".into());
        }
        let next = remaining.min(buf.len());
        let n = stdout.read(&mut buf[..next]).map_err(|e| e.to_string())?;
        if n == 0 {
            if !sent {
                let _ = sender.send(Err("child stdout closed before readiness".into()));
            }
            return Ok(all);
        }
        if all
            .len()
            .checked_add(n)
            .filter(|size| *size <= MAX_OUTPUT)
            .is_none()
        {
            if !sent {
                let _ = sender.send(Err("child stdout exceeded 1024-byte bound".into()));
            }
            return Err("child stdout exceeded 1024-byte bound".into());
        }
        all.extend_from_slice(&buf[..n]);
        if !sent {
            if let Some(index) = all.windows(expected.len()).position(|w| w == expected) {
                let _ = sender.send(Ok(all[index..index + expected.len()].to_vec()));
                sent = true;
            }
        }
    }
}

/// Exact ignored child target. Its environment is intentionally limited to the three
/// values supplied by the parent; it creates no subprocesses and contacts no network.
#[test]
#[ignore = "invoked only by exact process-death parents with a validated task root"]
fn process_death_child() {
    child_run().expect("validated process-death child phase");
}

fn child_run() -> Result<(), String> {
    let root = PathBuf::from(std::env::var("DISK_STEWARD_TASK_ROOT").map_err(|e| e.to_string())?);
    let expected = std::env::var("DISK_STEWARD_EXPECTED_DEVINO").map_err(|e| e.to_string())?;
    let phase = std::env::var("DISK_STEWARD_FIXED_PHASE").map_err(|e| e.to_string())?;
    if !matches!(phase.as_str(), "committed" | "partial") {
        return Err("invalid fixed phase".into());
    }
    if std::env::vars_os().count() != 3 {
        return Err("unexpected child environment entry".into());
    }
    let canonical = root.canonicalize().map_err(|e| e.to_string())?;
    if canonical != root
        || !root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .starts_with("disk-steward-registry-crash-")
    {
        return Err("task root is not canonical or has wrong fixture prefix".into());
    }
    let expected_pair = parse_devino(&expected)?;
    let dirfd = open(
        &root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| format!("open no-follow task root: {e}"))?;
    let dirfd = fs::File::from(dirfd);
    let held_meta = dirfd
        .metadata()
        .map_err(|e| format!("held root metadata: {e}"))?;
    let root_meta = fs::symlink_metadata(&root).map_err(|e| e.to_string())?;
    if (held_meta.dev(), held_meta.ino()) != expected_pair
        || (root_meta.dev(), root_meta.ino()) != expected_pair
        || !root_meta.is_dir()
        || root_meta.file_type().is_symlink()
        || root_meta.uid() != rustix::process::geteuid().as_raw()
        || root_meta.mode() & 0o7777 != 0o700
    {
        return Err("task root identity, owner, type, or mode mismatch".into());
    }
    let parent = root.parent().ok_or("task root has no parent")?;
    let parent_meta = fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
    let space = fstatvfs(&dirfd).map_err(|e| format!("fstatvfs task root: {e}"))?;
    let available = (space.f_bavail)
        .checked_mul(space.f_frsize)
        .ok_or("available byte count overflow")?;
    let required = (2u64 * 1024 * 1024 * 1024)
        .checked_add(1024 * 1024)
        .and_then(|v| v.checked_add(RESERVATION))
        .ok_or("space threshold overflow")?;
    if available < required {
        return Err("insufficient native free space for bounded fixture".into());
    }

    let registry_path = root.join("registry.sqlite");
    let mut registry = VerificationStoreRegistry::open(&registry_path)
        .map_err(|e| format!("open registry: {e}"))?;
    registry
        .register(
            STORE_ID,
            parent_meta.dev(),
            parent_meta.ino(),
            root_meta.dev(),
            root_meta.ino(),
        )
        .map_err(|e| format!("register fixture row: {e}"))?;
    registry
        .reserve(STORE_ID, 0, RESERVATION)
        .map_err(|e| format!("commit fixture reservation: {e}"))?;
    let marker = if phase == "partial" {
        let still_available =
            fstatvfs(&dirfd).map_err(|e| format!("recheck native free space: {e}"))?;
        let available = (still_available.f_bavail)
            .checked_mul(still_available.f_frsize)
            .ok_or("available byte count overflow")?;
        if available < required {
            return Err("native free-space headroom changed before payload".into());
        }
        let fd = openat(
            &dirfd,
            "partial.bin",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits(0o600).unwrap(),
        )
        .map_err(|e| format!("create exclusive partial payload: {e}"))?;
        let mut file = fs::File::from(fd);
        file.write_all(b"PACKf")
            .map_err(|e| format!("write fixed partial payload: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("sync fixed partial payload: {e}"))?;
        READY_PARTIAL
    } else {
        READY_COMMITTED
    };
    std::io::stdout()
        .write_all(marker)
        .and_then(|_| std::io::stdout().flush())
        .map_err(|e| format!("emit fixed readiness marker: {e}"))?;
    loop {
        thread::park_timeout(Duration::from_secs(60));
    }
}

fn parse_devino(s: &str) -> Result<(u64, u64), String> {
    let (dev, ino) = s.split_once(':').ok_or("invalid expected devino")?;
    if ino.contains(':') {
        return Err("invalid expected devino".into());
    }
    Ok((
        dev.parse().map_err(|_| "invalid expected device")?,
        ino.parse().map_err(|_| "invalid expected inode")?,
    ))
}
