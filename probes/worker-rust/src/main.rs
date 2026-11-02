use std::env;
use std::io::{self, Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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
        _ => Err("accepted arguments: none or --run".into()),
    }
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
        _ => Err("invalid private child mode".into()),
    }
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
