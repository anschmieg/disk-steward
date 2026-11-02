use std::env;
use std::io::{self, Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

mod store;

/// Shutdown evidence can only be issued by this supervisor after wait and
/// reader join have both completed successfully.
pub(crate) struct StopReceipt {
    token: String,
    pid: u32,
}
impl StopReceipt {
    fn confirmed(token: &str, pid: u32) -> Self {
        Self {
            token: token.to_owned(),
            pid,
        }
    }
    pub(crate) fn token(&self) -> &str {
        &self.token
    }
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }
}

const OUTPUT_LIMIT: u64 = 64 * 1024;
const OUTER_DEADLINE: Duration = Duration::from_secs(1);
const FLOOD_BYTES: usize = 1024 * 1024;
const CHILD_HOLD: Duration = Duration::from_secs(10);
const READ_CHUNK: usize = 8192;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Case {
    Flood,
    Stall,
    Cancel,
}

impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::Flood => "flood",
            Self::Stall => "stall",
            Self::Cancel => "cancel",
        }
    }

    fn child_mode(self) -> &'static str {
        match self {
            Self::Cancel => "stall",
            Self::Flood => "flood",
            Self::Stall => "stall",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopReason {
    OutputLimit,
    Deadline,
    Canceled,
    ChildExited,
    ReaderError,
}

#[derive(Clone, Copy, Debug)]
enum ReaderEvent {
    Ready,
    Finished { bytes: u64, failed: bool },
}

#[derive(Debug)]
struct Report {
    case: Case,
    stop_reason: StopReason,
    bytes_read: u64,
    elapsed: Duration,
    child_exit: ExitStatus,
    reader_joined: bool,
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = if args.first().map(String::as_str) == Some("--child") {
        run_child(&args[1..])
    } else {
        run_parent(&args)
    };
    if let Err(error) = result {
        eprintln!("worker probe error: {error}");
        std::process::exit(1);
    }
}

fn run_parent(args: &[String]) -> Result<(), String> {
    match args {
        [] => {
            println!("Rust subprocess supervision probe (dry run; use --run to start fixed synthetic children)");
            println!("Cases: bounded stdout flood, one-second stall deadline, and explicit parent cancellation.");
            Ok(())
        }
        [flag] if flag == "--run-store" => {
            let store = store::OwnedStore::create()
                .map_err(|error| format!("create owned store: {error:?}"))?;
            let state = store
                .inspect_state()
                .map_err(|error| format!("inspect owned store: {error:?}"))?;
            println!(
                "store_state={state:?} token={} cleanup=not_authorized",
                store.token()
            );
            Ok(())
        }
        [flag] if flag == "--run-store-worker" => run_store_worker(),
        [flag] if flag == "--run-store-reinspect" => run_store_reinspect(),
        [flag] if flag == "--run-indexed-store-worker" => run_indexed_store_worker(),
        [flag, confirm] if flag == "--run-store-cleanup" && confirm == "--confirm-own-store-cleanup" => run_store_worker_with_cleanup(true),
        [flag] if flag == "--run-store-cleanup" => Err("refusing store cleanup without --confirm-own-store-cleanup".into()),
        [flag] if flag == "--run" => {
            for case in [Case::Flood, Case::Stall, Case::Cancel] {
                let report = supervise(case)?;
                let expected = match case {
                    Case::Flood => (StopReason::OutputLimit, OUTPUT_LIMIT + 1),
                    Case::Stall => (StopReason::Deadline, 1),
                    Case::Cancel => (StopReason::Canceled, 1),
                };
                if (report.stop_reason, report.bytes_read) != expected {
                    return Err(format!(
                        "case={} produced stop={:?} bytes={}, expected stop={:?} bytes={}",
                        report.case.name(),
                        report.stop_reason,
                        report.bytes_read,
                        expected.0,
                        expected.1
                    ));
                }
                println!(
                    "case={} stop={:?} bytes={} elapsed_ms={} child_exit={} reader_joined={}",
                    report.case.name(),
                    report.stop_reason,
                    report.bytes_read,
                    report.elapsed.as_millis(),
                    report.child_exit,
                    report.reader_joined
                );
            }
            Ok(())
        }
        _ => Err("accepted arguments: none, --run, --run-store, --run-store-worker, --run-store-reinspect, --run-indexed-store-worker, or --run-store-cleanup --confirm-own-store-cleanup".into()),
    }
}

fn run_indexed_store_worker() -> Result<(), String> {
    use verification_store_registry::{
        RegistryError, StoreState as RState, VerificationStoreRegistry,
    };
    const FIXTURE_ID: &str = "0123456789abcdef0123456789abcdef";
    let mut store = store::OwnedStore::create().map_err(|e| format!("create store: {e}"))?;
    let registry_path = store
        .control_registry_path()
        .map_err(|e| format!("validate generated store parent: {e}"))?;
    let (parent, root) = store
        .control_identity()
        .map_err(|e| format!("validate store identity: {e}"))?;
    let mut registry = VerificationStoreRegistry::open(&registry_path)
        .map_err(|e| format!("open private registry: {e}"))?;
    registry
        .register(FIXTURE_ID, parent.0, parent.1, root.0, root.1)
        .map_err(|e| format!("register store: {e}"))?;
    let exe = env::current_exe().map_err(|e| format!("locate own executable: {e}"))?;
    let mut child = Command::new(exe)
        .arg("--child")
        .arg("stall")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("start fixed child: {e}"))?;
    let pid = child.id();
    if let Err(e) = store.bind_worker(pid) {
        return match stop_and_reap(&mut child) {
            Ok(()) => Err(format!("bind worker: {e}")),
            Err(s) => Err(format!("bind worker: {e}; cleanup failed: {s}")),
        };
    }
    let stdout = match child.stdout.take() {
        Some(v) => v,
        None => {
            return match stop_and_reap(&mut child) {
                Ok(()) => Err("child stdout unavailable".into()),
                Err(e) => Err(format!("child stdout unavailable; cleanup failed: {e}")),
            }
        }
    };
    let (tx, rx) = mpsc::sync_channel(2);
    let reader = match thread::Builder::new()
        .name("indexed-store-stdout".into())
        .spawn(move || read_bounded(stdout, tx))
    {
        Ok(v) => v,
        Err(e) => {
            return match stop_and_reap(&mut child) {
                Ok(()) => Err(format!("start reader: {e}")),
                Err(s) => Err(format!("start reader: {e}; cleanup failed: {s}")),
            }
        }
    };
    if let Err(e) = await_readiness(&rx, Instant::now() + OUTER_DEADLINE) {
        return finish_after_error(child, reader, rx, e);
    }
    if let Err(e) = registry.transition(FIXTURE_ID, RState::Registered, RState::Active) {
        return finish_after_error(child, reader, rx, format!("activate store: {e}"));
    }
    if let Err(e) = stop_and_reap(&mut child) {
        let _ = reader.join();
        return Err(format!("child shutdown unconfirmed: {e}"));
    }
    if reader.join().is_err() {
        return Err("reader join failed; stop receipt withheld".into());
    }
    let status = child
        .try_wait()
        .map_err(|e| format!("confirm child exit: {e}"))?
        .ok_or("child exit missing")?;
    let (bytes, failed) = drain_reader_result(&rx)?;
    if failed || bytes != 1 {
        return Err("unexpected worker output; stop receipt withheld".into());
    }
    let receipt = StopReceipt::confirmed(store.token(), pid);
    store
        .record_stopped(&receipt)
        .map_err(|e| format!("record stopped: {e}"))?;
    registry
        .transition(FIXTURE_ID, RState::Active, RState::Stopped)
        .map_err(|e| format!("record registry stop: {e}"))?;
    let live_state = store
        .inspect_state()
        .map_err(|e| format!("inspect store: {e}"))?;
    drop(registry);
    drop(store);
    let mut reopened = VerificationStoreRegistry::open(&registry_path)
        .map_err(|e| format!("reopen private registry: {e}"))?;
    let record = reopened
        .get(FIXTURE_ID)
        .map_err(|e| format!("read reopened registry: {e}"))?
        .ok_or("registry row disappeared")?;
    if record.state != RState::NeedsReconciliation
        || record.prior_state != Some(RState::Stopped)
        || (
            record.parent_dev,
            record.parent_ino,
            record.root_dev,
            record.root_ino,
        ) != (parent.0, parent.1, root.0, root.1)
    {
        return Err("reopened registry identity/state mismatch".into());
    }
    match reopened.transition(FIXTURE_ID, RState::NeedsReconciliation, RState::Active) {
        Err(RegistryError::InvalidTransition) => {}
        Err(other) => {
            return Err(format!(
                "retry refusal was not the expected lifecycle gate: {other}"
            ))
        }
        Ok(_) => return Err("reopened reconciliation row unexpectedly became active".into()),
    }
    println!("worker_joined=true registered_stopped=true reopened_reconciliation=true prior_stopped=true store_state={live_state:?} child_exit={status} cleanup=not_authorized");
    Ok(())
}

fn run_store_worker() -> Result<(), String> {
    run_store_worker_with_cleanup(false)
}

fn run_store_worker_with_cleanup(cleanup: bool) -> Result<(), String> {
    let mut store = store::OwnedStore::create().map_err(|e| format!("create owned store: {e}"))?;
    let executable = env::current_exe().map_err(|e| format!("locate own executable: {e}"))?;
    let mut child = Command::new(executable)
        .arg("--child")
        .arg("stall")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("start fixed child: {e}"))?;
    let pid = child.id();
    if let Err(error) = store.bind_worker(pid) {
        return match stop_and_reap(&mut child) {
            Ok(()) => Err(format!("bind worker: {error}")),
            Err(stop) => Err(format!(
                "bind worker: {error}; child cleanup failed: {stop}"
            )),
        };
    }
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            return match stop_and_reap(&mut child) {
                Ok(()) => Err("child stdout pipe unavailable".into()),
                Err(error) => Err(format!(
                    "child stdout pipe unavailable; cleanup failed: {error}"
                )),
            }
        }
    };
    let (sender, receiver) = mpsc::sync_channel(2);
    let reader = match thread::Builder::new()
        .name("store-worker-stdout".into())
        .spawn(move || read_bounded(stdout, sender))
    {
        Ok(reader) => reader,
        Err(error) => {
            return match stop_and_reap(&mut child) {
                Ok(()) => Err(format!("start bounded stdout reader: {error}")),
                Err(stop) => Err(format!(
                    "start bounded stdout reader: {error}; cleanup failed: {stop}"
                )),
            }
        }
    };
    let deadline = Instant::now() + OUTER_DEADLINE;
    if let Err(error) = await_readiness(&receiver, deadline) {
        return finish_after_error(child, reader, receiver, error);
    }
    if let Err(error) = stop_and_reap(&mut child) {
        let _ = reader.join();
        return Err(format!("child shutdown unconfirmed: {error}"));
    }
    let joined = reader.join().is_ok();
    if !joined {
        return Err("stdout reader thread panicked; shutdown receipt withheld".into());
    }
    let status = child
        .try_wait()
        .map_err(|e| format!("confirm child exit: {e}"))?
        .ok_or_else(|| "child exit was not observable after wait".to_string())?;
    let (bytes, failed) = drain_reader_result(&receiver)?;
    if failed || bytes != 1 {
        return Err("unexpected store-worker reader result; shutdown receipt withheld".into());
    }
    let receipt = StopReceipt::confirmed(store.token(), pid);
    store
        .record_stopped(&receipt)
        .map_err(|e| format!("record stopped transition: {e}"))?;
    if cleanup {
        store
            .cleanup_after_stopped(&receipt)
            .map_err(|e| format!("cleanup stopped store: {e}"))?;
        println!("store_removed=true retained_parent_journal=cleanup.journal worker_pid={pid}");
        return Ok(());
    }
    let state = store
        .inspect_state()
        .map_err(|e| format!("inspect stopped store: {e}"))?;
    println!(
        "store_state={state:?} token={} worker_pid={} child_exit={} reader_joined=true bytes={} cleanup=not_authorized",
        store.token(),
        pid,
        status,
        bytes
    );
    Ok(())
}

fn run_child(args: &[String]) -> Result<(), String> {
    match args {
        [mode] if mode == "flood" => {
            let mut stdout = io::stdout().lock();
            stdout.write_all(b"R").map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
            let chunk = [b'F'; READ_CHUNK];
            let mut remaining = FLOOD_BYTES;
            while remaining > 0 {
                let count = remaining.min(chunk.len());
                stdout
                    .write_all(&chunk[..count])
                    .map_err(|e| e.to_string())?;
                remaining -= count;
            }
            stdout.flush().map_err(|e| e.to_string())?;
            thread::sleep(CHILD_HOLD);
            Ok(())
        }
        [mode] if mode == "stall" => {
            let mut stdout = io::stdout().lock();
            stdout.write_all(b"R").map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
            thread::sleep(CHILD_HOLD);
            Ok(())
        }
        [mode] if mode == "store-abandon" => run_store_abandon_child(),
        _ => Err("invalid private child mode".into()),
    }
}

fn run_store_abandon_child() -> Result<(), String> {
    let mut store = store::OwnedStore::create().map_err(|e| e.to_string())?;
    let pid = std::process::id();
    store.bind_worker(pid).map_err(|e| e.to_string())?;
    let record = store.abandon_record(pid).map_err(|e| e.to_string())?;
    let mut out = io::stdout().lock();
    out.write_all(record.as_bytes())
        .and_then(|_| out.flush())
        .map_err(|e| e.to_string())?;
    std::process::exit(86)
}

fn run_store_reinspect() -> Result<(), String> {
    let start = Instant::now();
    let deadline = start + OUTER_DEADLINE;
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg("--child")
        .arg("store-abandon")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let pid = child.id();
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let stopped = stop_and_reap(&mut child);
            return Err(format!("child stdout unavailable; stop/reap={stopped:?}"));
        }
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("abandoned-store-record".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(1025).read_to_end(&mut bytes);
            let _ = tx.send((bytes, result.is_err()));
        });
    let reader = match reader {
        Ok(reader) => reader,
        Err(e) => {
            let stopped = stop_and_reap(&mut child);
            return Err(format!("start record reader: {e}; stop/reap={stopped:?}"));
        }
    };
    let record = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    let capture_expired = Instant::now() >= deadline;
    let stopped = stop_and_reap(&mut child);
    let joined = reader.join().is_ok();
    if stopped.is_err() || !joined {
        return Err(format!(
            "creator shutdown unconfirmed: {stopped:?}; reader_joined={joined}"
        ));
    }
    let waited = child
        .try_wait()
        .map_err(|e| format!("confirm creator exit: {e}"))?
        .ok_or_else(|| "creator exit missing after wait".to_string())?;
    if capture_expired {
        return Err("creator record capture exceeded deadline; fixture retained".into());
    }
    let (bytes, failed) =
        record.map_err(|e| format!("creator record unavailable before deadline: {e}"))?;
    if failed || bytes.is_empty() || bytes.len() > 1024 {
        return Err("invalid or oversized child record".into());
    }
    let snapshot = parse_abandoned_record(&bytes)?;
    if snapshot.bound_pid != pid {
        return Err("child record PID mismatch".into());
    }
    // A complete validated record plus confirmed wait is sufficient even if the OS reports a signal.
    let state = store::OwnedStore::inspect_abandoned(&snapshot)
        .map_err(|e| format!("read-only store inspection refused: {e}"))?;
    println!("store_state={state:?} token={} creator_stopped=true cleanup=not_authorized child_exit={waited} reader_joined=true process_death_not_power_loss=true", snapshot.token);
    Ok(())
}

fn parse_abandoned_record(bytes: &[u8]) -> Result<store::AbandonedSnapshot, String> {
    if bytes.len() > 1024 {
        return Err("record exceeds limit".into());
    }
    let s = std::str::from_utf8(bytes).map_err(|_| "record is not ASCII")?;
    if !s.is_ascii() || !s.ends_with('\n') {
        return Err("malformed record".into());
    }
    let lines: Vec<&str> = s[..s.len() - 1].split('\n').collect();
    if lines.len() != 7 || lines[0] != "v1" {
        return Err("unsupported record shape or version".into());
    }
    let token = lines[1];
    if !store::valid_token(token) {
        return Err("invalid token".into());
    }
    fn number(s: &str) -> Result<u64, String> {
        if s.is_empty()
            || !s.bytes().all(|b| b.is_ascii_digit())
            || (s.len() > 1 && s.starts_with('0'))
        {
            return Err("noncanonical number".into());
        }
        s.parse().map_err(|_| "number overflow".into())
    }
    fn pair(s: &str) -> Result<(u64, u64), String> {
        let (a, b) = s.split_once(':').ok_or("bad identity")?;
        Ok((number(a)?, number(b)?))
    }
    let parent_id = pair(lines[2])?;
    let root_id = pair(lines[3])?;
    let journal_id = pair(lines[4])?;
    let bound_pid = u32::try_from(number(lines[5])?).map_err(|_| "bad PID")?;
    if bound_pid == 0 {
        return Err("bad PID".into());
    }
    // Extra line is not accepted; legacy shape reserves line 6 only if exact sentinel.
    if lines[6] != "end" {
        return Err("malformed record terminator".into());
    }
    Ok(store::AbandonedSnapshot {
        token: token.to_owned(),
        parent_id,
        root_id,
        journal_id,
        bound_pid,
        version: 1,
    })
}

fn supervise(case: Case) -> Result<Report, String> {
    let start = Instant::now();
    let deadline = start + OUTER_DEADLINE;
    let executable = env::current_exe().map_err(|e| format!("locate own executable: {e}"))?;
    let mut child = Command::new(executable)
        .arg("--child")
        .arg(case.child_mode())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("start fixed child: {e}"))?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let cleanup = stop_and_reap(&mut child);
            return match cleanup {
                Ok(()) => Err("child stdout pipe was not available".into()),
                Err(error) => Err(format!(
                    "child stdout pipe was not available; child cleanup failed: {error}"
                )),
            };
        }
    };
    let (sender, receiver) = mpsc::sync_channel(2);
    let reader = thread::Builder::new()
        .name(format!("worker-probe-stdout-{}", case.name()))
        .spawn(move || read_bounded(stdout, sender));
    let reader = match reader {
        Ok(reader) => reader,
        Err(error) => {
            let cleanup = stop_and_reap(&mut child);
            return match cleanup {
                Ok(()) => Err(format!("start bounded stdout reader: {error}")),
                Err(cleanup_error) => Err(format!(
                    "start bounded stdout reader: {error}; child cleanup failed: {cleanup_error}"
                )),
            };
        }
    };
    let supervision = if case == Case::Cancel {
        await_readiness(&receiver, deadline).map(|()| (StopReason::Canceled, None))
    } else {
        await_stop_reason(&mut child, &receiver, deadline)
    };
    let (stop_reason, completed_reader) = match supervision {
        Ok(result) => result,
        Err(error) => {
            return finish_after_error(child, reader, receiver, error);
        }
    };

    let mut cleanup_error = None;
    if let Err(error) = stop_and_reap(&mut child) {
        cleanup_error = Some(error);
    }
    let reader_joined = reader.join().is_ok();
    if !reader_joined {
        cleanup_error = Some("stdout reader thread panicked".to_string());
    }
    if let Some(error) = cleanup_error {
        return Err(format!("{error}; child cleanup was not fully confirmed"));
    }
    let (bytes_read, reader_failed) = completed_reader
        .or_else(|| drain_reader_result(&receiver).ok())
        .unwrap_or((OUTPUT_LIMIT.saturating_add(1), true));
    if reader_failed {
        return Err("stdout reader failed".into());
    }
    let child_exit = child
        .try_wait()
        .map_err(|e| format!("confirm child exit after wait: {e}"))?
        .ok_or_else(|| "child exit was not observable after wait".to_string())?;

    Ok(Report {
        case,
        stop_reason,
        bytes_read,
        elapsed: start.elapsed(),
        child_exit,
        reader_joined,
    })
}

fn await_readiness(receiver: &Receiver<ReaderEvent>, deadline: Instant) -> Result<(), String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("outer deadline expired before fixed child signaled readiness".into());
    }
    match receiver.recv_timeout(remaining) {
        Ok(ReaderEvent::Ready) => Ok(()),
        Ok(ReaderEvent::Finished { failed: true, .. }) => {
            Err("stdout reader failed before fixed child signaled readiness".into())
        }
        Ok(ReaderEvent::Finished { .. }) => {
            Err("stdout closed before fixed child signaled readiness".into())
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err("outer deadline expired before fixed child signaled readiness".into())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("stdout reader stopped without a result".into())
        }
    }
}

fn await_stop_reason(
    child: &mut Child,
    receiver: &Receiver<ReaderEvent>,
    deadline: Instant,
) -> Result<(StopReason, Option<(u64, bool)>), String> {
    let mut saw_ready = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return if saw_ready {
                Ok((StopReason::Deadline, None))
            } else {
                Err("outer deadline expired before fixed child signaled readiness".into())
            };
        }
        match receiver.recv_timeout(remaining) {
            Ok(ReaderEvent::Ready) => saw_ready = true,
            Ok(ReaderEvent::Finished {
                bytes: observed,
                failed,
            }) => {
                if failed {
                    return Ok((StopReason::ReaderError, Some((observed, failed))));
                }
                if observed > OUTPUT_LIMIT {
                    return Ok((StopReason::OutputLimit, Some((observed, failed))));
                }
                return if child
                    .try_wait()
                    .map_err(|e| format!("inspect child status: {e}"))?
                    .is_some()
                {
                    Ok((StopReason::ChildExited, Some((observed, failed))))
                } else {
                    Err("stdout closed while fixed child remained running".into())
                };
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return if saw_ready {
                    Ok((StopReason::Deadline, None))
                } else {
                    Err("outer deadline expired before fixed child signaled readiness".into())
                };
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("stdout reader stopped without a result".into());
            }
        }
    }
}

fn read_bounded<R: Read>(mut reader: R, sender: SyncSender<ReaderEvent>) {
    let mut bytes = 0_u64;
    let mut signaled_ready = false;
    let mut failed = false;
    let mut buffer = [0_u8; READ_CHUNK];
    while bytes <= OUTPUT_LIMIT {
        let remaining = OUTPUT_LIMIT.saturating_add(1).saturating_sub(bytes) as usize;
        let wanted = remaining.min(buffer.len());
        match reader.read(&mut buffer[..wanted]) {
            Ok(0) => break,
            Ok(count) => {
                bytes = bytes.saturating_add(count as u64);
                if !signaled_ready {
                    if sender.send(ReaderEvent::Ready).is_err() {
                        return;
                    }
                    signaled_ready = true;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    let _ = sender.send(ReaderEvent::Finished { bytes, failed });
}

fn drain_reader_result(receiver: &Receiver<ReaderEvent>) -> Result<(u64, bool), String> {
    receiver
        .try_iter()
        .find_map(|event| match event {
            ReaderEvent::Finished { bytes, failed } => Some((bytes, failed)),
            ReaderEvent::Ready => None,
        })
        .ok_or_else(|| "stdout reader exited without a result".into())
}

fn stop_and_reap(child: &mut Child) -> Result<(), String> {
    // A status-query failure must not bypass the stop/reap attempt. A kill
    // error may merely race natural exit; wait is the authority for reaping.
    let kill_error = if matches!(child.try_wait(), Ok(Some(_))) {
        None
    } else {
        child.kill().err()
    };
    child
        .wait()
        .map(|_| ())
        .map_err(|wait_error| match kill_error {
            Some(kill_error) => {
                format!("kill fixed child: {kill_error}; wait for fixed child: {wait_error}")
            }
            None => format!("wait for fixed child: {wait_error}"),
        })
}

fn finish_after_error<T>(
    mut child: Child,
    reader: JoinHandle<()>,
    receiver: Receiver<ReaderEvent>,
    error: String,
) -> Result<T, String> {
    let cleanup = stop_and_reap(&mut child);
    let _ = receiver.try_iter().count();
    let joined = reader.join().is_ok();
    match (cleanup, joined) {
        (Ok(()), true) => Err(error),
        (Err(cleanup_error), true) => Err(format!("{error}; cleanup failed: {cleanup_error}")),
        (Ok(()), false) => Err(format!("{error}; stdout reader thread panicked")),
        (Err(cleanup_error), false) => Err(format!(
            "{error}; cleanup failed: {cleanup_error}; stdout reader thread panicked"
        )),
    }
}

#[cfg(test)]
mod store_worker_tests {
    use super::{store::OwnedStore, StopReceipt};

    #[test]
    fn store_binding_is_single_store_and_exact_pid() {
        let mut first = OwnedStore::create().unwrap();
        let mut second = OwnedStore::create().unwrap();
        assert!(first
            .record_stopped(&StopReceipt::confirmed(first.token(), 41))
            .is_err());
        assert!(first.bind_worker(0).is_err());
        first.bind_worker(41).unwrap();
        second.bind_worker(41).unwrap();
        assert!(first.bind_worker(42).is_err());
        assert!(second
            .record_stopped(&StopReceipt::confirmed(first.token(), 41))
            .is_err());
        assert!(first
            .record_stopped(&StopReceipt::confirmed(first.token(), 42))
            .is_err());
        first
            .record_stopped(&StopReceipt::confirmed(first.token(), 41))
            .unwrap();
        assert!(first
            .record_stopped(&StopReceipt::confirmed(first.token(), 41))
            .is_err());
        second
            .record_stopped(&StopReceipt::confirmed(second.token(), 41))
            .unwrap();
    }
}

#[cfg(test)]
mod abandon_record_tests {
    use super::parse_abandoned_record;
    const VALID: &[u8] = b"v1\na1-b2-c3\n1:2\n3:4\n5:6\n7\nend\n";
    #[test]
    fn accepts_closed_record_shape() {
        assert_eq!(parse_abandoned_record(VALID).unwrap().bound_pid, 7);
    }
    #[test]
    fn rejects_traversal_overflow_oversize_and_future_version() {
        let valid = std::str::from_utf8(VALID).unwrap();
        for (original, replacement) in [
            ("a1-b2-c3", "../x"),
            ("a1-b2-c3", "a--c"),
            ("a1-b2-c3", "A1-b2-c3"),
            ("a1-b2-c3", "a1-b2"),
            ("1:2", "18446744073709551616:2"),
            ("1:2", "+1:2"),
            ("1:2", "01:2"),
            ("\n7\n", "\n4294967296\n"),
            ("\n7\n", "\n0\n"),
            ("v1\n", "v2\n"),
            ("end\n", "end\nextra\n"),
        ] {
            let bad = valid.replace(original, replacement);
            assert!(
                parse_abandoned_record(bad.as_bytes()).is_err(),
                "{replacement}"
            );
        }
        assert!(parse_abandoned_record(&vec![b'a'; 1025]).is_err());
    }
}
