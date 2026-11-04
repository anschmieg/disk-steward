use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: usize = 1024;
const READ_SIZE: usize = 64;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct CompletionReceipt {
    pub(super) bytes: usize,
    pub(super) reader_joined: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CompletionFailure {
    Spawn,
    Reader,
    Deadline,
    OutputLimit,
    ChildFailed,
    MissingMarker,
    Reap,
}

#[derive(Debug)]
struct ReaderDone {
    bytes: usize,
    matched: bool,
    failed: bool,
    overflow: bool,
}

fn read_output<R: Read>(mut output: R, marker: &'static [u8], sender: SyncSender<ReaderDone>) {
    let mut buffer = [0u8; OUTPUT_LIMIT + 1];
    let mut bytes = 0usize;
    let mut matched = false;
    let mut failed = false;
    let mut overflow = false;
    let mut interruptions = 0usize;

    loop {
        if bytes == OUTPUT_LIMIT {
            match output.read(&mut buffer[bytes..bytes + 1]) {
                Ok(0) => break,
                Ok(_) => {
                    overflow = true;
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                    interruptions += 1;
                    if interruptions <= 16 {
                        continue;
                    }
                    failed = true;
                    break;
                }
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }

        let end = (bytes + READ_SIZE).min(OUTPUT_LIMIT);
        match output.read(&mut buffer[bytes..end]) {
            Ok(0) => break,
            Ok(count) => {
                bytes += count;
                interruptions = 0;
                if !marker.is_empty() && !matched {
                    matched = buffer[..bytes]
                        .windows(marker.len())
                        .any(|window| window == marker);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                interruptions += 1;
                if interruptions > 16 {
                    failed = true;
                    break;
                }
            }
            Err(_) => {
                failed = true;
                break;
            }
        }
    }

    let _ = sender.send(ReaderDone {
        bytes,
        matched,
        failed,
        overflow,
    });
}

fn clean_child(
    child: &mut Child,
    reader: Option<JoinHandle<()>>,
) -> (Result<ExitStatus, ()>, bool) {
    let status = match child.try_wait() {
        Ok(Some(status)) => Ok(status),
        Ok(None) => {
            let _ = child.kill();
            child.wait().map_err(|_| ())
        }
        Err(_) => {
            let _ = child.kill();
            child.wait().map_err(|_| ())
        }
    };
    let joined = reader.map(|handle| handle.join().is_ok()).unwrap_or(true);
    (status, joined)
}

pub(super) fn supervise(
    exact_test: &'static str,
    marker: &'static [u8],
    timeout: Duration,
) -> Result<CompletionReceipt, CompletionFailure> {
    let deadline = Instant::now() + timeout;
    let executable = std::env::current_exe().map_err(|_| CompletionFailure::Spawn)?;
    let mut child = Command::new(executable)
        .args(["--ignored", "--exact", exact_test, "--nocapture"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| CompletionFailure::Spawn)?;

    let Some(stdout) = child.stdout.take() else {
        let _ = clean_child(&mut child, None);
        return Err(CompletionFailure::Reader);
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = match thread::Builder::new()
        .name("completion-output-reader".into())
        .spawn(move || read_output(stdout, marker, sender))
    {
        Ok(reader) => reader,
        Err(_) => {
            let _ = clean_child(&mut child, None);
            return Err(CompletionFailure::Reader);
        }
    };

    let mut status = None;
    let mut done = None;
    let mut deadline_expired = false;
    let mut output_overflow = false;
    let mut reader_failed = false;

    while status.is_none() || done.is_none() {
        if Instant::now() >= deadline {
            deadline_expired = true;
            break;
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(status_result) => status = status_result,
                Err(_) => break,
            }
        }
        if done.is_none() {
            match receiver.try_recv() {
                Ok(result) => {
                    output_overflow = result.overflow;
                    reader_failed = result.failed;
                    done = Some(result);
                }
                Err(TryRecvError::Disconnected) => {
                    reader_failed = true;
                    break;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if output_overflow || reader_failed {
            break;
        }
        if status.is_some() && done.is_some() {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if done.is_some() {
            // A finished reader disconnects its channel before the child may
            // exit. Keep waiting for the process without treating that normal
            // disconnect as another reader failure.
            thread::sleep(remaining.min(Duration::from_millis(20)));
            continue;
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(20))) {
            Ok(result) => {
                output_overflow = result.overflow;
                reader_failed = result.failed;
                done = Some(result);
            }
            Err(RecvTimeoutError::Disconnected) => {
                reader_failed = true;
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }

    if Instant::now() >= deadline {
        deadline_expired = true;
    }
    let (reaped, joined) = clean_child(&mut child, Some(reader));
    if output_overflow {
        return Err(CompletionFailure::OutputLimit);
    }
    if deadline_expired {
        return Err(CompletionFailure::Deadline);
    }
    if reader_failed || !joined {
        return Err(CompletionFailure::Reader);
    }
    let Some(done) = done else {
        return Err(CompletionFailure::Reader);
    };
    let Ok(exit_status) = reaped else {
        return Err(CompletionFailure::Reap);
    };
    if !exit_status.success() {
        return Err(CompletionFailure::ChildFailed);
    }
    if done.failed {
        return Err(CompletionFailure::Reader);
    }
    if done.overflow || done.bytes > OUTPUT_LIMIT {
        return Err(CompletionFailure::OutputLimit);
    }
    if !done.matched {
        return Err(CompletionFailure::MissingMarker);
    }
    Ok(CompletionReceipt {
        bytes: done.bytes,
        reader_joined: joined,
    })
}

#[test]
#[ignore]
fn completion_stall_child() {
    loop {
        thread::park();
    }
}

#[test]
#[ignore]
fn completion_flood_child() {
    let _ = std::io::stdout().write_all(&[b'x'; 4096]);
}

#[test]
#[ignore]
fn completion_missing_marker_child() {
    println!("completion fixture emitted unrelated output");
}

#[test]
#[ignore]
fn completion_failure_child() {
    panic!("completion fixture failure");
}

#[test]
fn supervision_stops_a_stalled_child() {
    assert_eq!(
        supervise(
            "tests::completion_supervision::completion_stall_child",
            b"unique completion marker",
            Duration::from_millis(200),
        ),
        Err(CompletionFailure::Deadline)
    );
}

#[test]
fn supervision_caps_child_output() {
    assert_eq!(
        supervise(
            "tests::completion_supervision::completion_flood_child",
            b"unique completion marker",
            Duration::from_secs(5),
        ),
        Err(CompletionFailure::OutputLimit)
    );
}

#[test]
fn supervision_requires_the_marker() {
    assert_eq!(
        supervise(
            "tests::completion_supervision::completion_missing_marker_child",
            b"unique completion marker",
            Duration::from_secs(5),
        ),
        Err(CompletionFailure::MissingMarker)
    );
}

#[test]
fn supervision_reports_child_failure() {
    assert_eq!(
        supervise(
            "tests::completion_supervision::completion_failure_child",
            b"unique completion marker",
            Duration::from_secs(5),
        ),
        Err(CompletionFailure::ChildFailed)
    );
}

#[test]
fn collector_accepts_exact_cap_and_observes_one_byte_overflow() {
    const MARKER: &[u8] = b"EXACT_COMPLETION_MARKER\n";
    let mut exact = vec![b'x'; OUTPUT_LIMIT - MARKER.len()];
    exact.extend_from_slice(MARKER);
    let (tx, rx) = mpsc::sync_channel(1);
    read_output(std::io::Cursor::new(&exact), MARKER, tx);
    let done = rx.recv().unwrap();
    assert_eq!(done.bytes, OUTPUT_LIMIT);
    assert!(done.matched && !done.failed && !done.overflow);

    exact.push(b'x');
    let (tx, rx) = mpsc::sync_channel(1);
    read_output(std::io::Cursor::new(&exact), MARKER, tx);
    let done = rx.recv().unwrap();
    assert!(done.matched && done.overflow && !done.failed);
}

#[test]
fn collector_refuses_a_reader_that_never_progresses() {
    struct Interrupted(usize);
    impl Read for Interrupted {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            self.0 += 1;
            Err(std::io::ErrorKind::Interrupted.into())
        }
    }
    let mut source = Interrupted(0);
    let (tx, rx) = mpsc::sync_channel(1);
    read_output(&mut source, b"marker", tx);
    let done = rx.recv().unwrap();
    assert!(done.failed && !done.matched && !done.overflow);
    assert_eq!(source.0, 17);
}
