#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
mod bounded_http;
use bounded_http::{AttemptBudget, BudgetedHttp};
use serde::{Deserialize, Serialize};
use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, BufRead, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

const BOUNDED_PROBE_DEADLINE: Duration = Duration::from_secs(60);
const BOUNDED_PROBE_STORE_BYTES: u64 = 32 * 1024 * 1024;
const BOUNDED_PROBE_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
const BOUNDED_PROBE_HEADER_BYTES: u64 = 64 * 1024;
const FREE_SPACE_RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const FREE_SPACE_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;
const REQUIRED_FREE_SPACE_BYTES: u64 = FREE_SPACE_RESERVE_BYTES + FREE_SPACE_HEADROOM_BYTES;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const OWNER_MARKER: &str = ".disk-steward-git-https-probe-owner";
const EMPTY_GIT_CONFIG: &str = ".gitconfig-empty";
const WORKER_GUARD_EXIT_CODE: i32 = 74;
const MAX_SECRET_BYTES: usize = 512;
const MAX_RECEIPT_BYTES: usize = 4096;
const MAX_RECEIPT_REFS: u32 = 100_000;
const MAX_RECEIPT_OBJECTS: u32 = 10_000_000;
static STORE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    InvalidEndpoint,
    WorkerFailed,
    Deadline,
    StoreBudget,
    StoreAccounting,
    FreeSpace,
    CredentialInput,
}

#[derive(Debug)]
struct Attempt {
    failure: Option<Failure>,
    elapsed: Duration,
    logical_store_bytes: u64,
    cleaned: bool,
    worker_receipt: Option<WorkerReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerReceipt {
    schema_version: u8,
    bare_fetch_completed: bool,
    response_body_exposed_bytes: u32,
    response_header_exposed_bytes: u32,
    object_count: Option<u32>,
    ref_count: Option<u32>,
}

fn main() {
    let mut args = env::args_os().skip(1);
    let result = match args.next().as_deref() {
        Some(arg) if arg == "--fetch-worker" => {
            worker_entry(args.next(), args.next(), args.next(), false)
        }
        Some(arg) if arg == "--private-fetch-worker" => {
            worker_entry(args.next(), args.next(), args.next(), true)
        }
        Some(arg) if arg == "--bounded-probe" => bounded_probe(args.next()),
        Some(arg) if arg == "--private-probe" => private_probe(args.next(), args.next()),
        None => {
            eprintln!(
                "Usage: git-https-worker-probe --bounded-probe https://github.com/OWNER/REPO[.git]"
            );
            return;
        }
        Some(_) => Err(Failure::InvalidEndpoint),
    };
    if let Err(failure) = result {
        eprintln!("verification failed: {}", failure.as_str());
        std::process::exit(1);
    }
}

impl Failure {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEndpoint => "unsupported endpoint",
            Self::WorkerFailed => "worker failed",
            Self::Deadline => "attempt deadline exceeded",
            Self::StoreBudget => "temporary store budget exceeded",
            Self::StoreAccounting => "temporary store accounting failed",
            Self::FreeSpace => "verification volume free-space reserve unavailable",
            Self::CredentialInput => "credential input invalid",
        }
    }
}

fn bounded_probe(input: Option<OsString>) -> Result<(), Failure> {
    let url = input
        .ok_or(Failure::InvalidEndpoint)?
        .to_string_lossy()
        .into_owned();
    validate_github_https(&url)?;
    let attempt = run_worker(
        WorkerKind::Fetch(url),
        BOUNDED_PROBE_DEADLINE,
        BOUNDED_PROBE_STORE_BYTES,
    )?;
    print_attempt(&attempt);
    match attempt.failure {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

fn private_probe(input: Option<OsString>, extra: Option<OsString>) -> Result<(), Failure> {
    if extra.is_some() {
        return Err(Failure::InvalidEndpoint);
    }
    let url = input
        .ok_or(Failure::InvalidEndpoint)?
        .to_string_lossy()
        .into_owned();
    validate_github_https(&url)?;
    let stdin = io::stdin();
    ensure_non_terminal_stdin(stdin.is_terminal())?;
    let secret = read_bounded_secret(stdin.lock())?;
    let attempt = run_worker(
        WorkerKind::PrivateFetch { url, secret },
        BOUNDED_PROBE_DEADLINE,
        BOUNDED_PROBE_STORE_BYTES,
    )?;
    print_attempt(&attempt);
    match attempt.failure {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

fn print_attempt(attempt: &Attempt) {
    let (body, headers, objects, refs) = attempt
        .worker_receipt
        .as_ref()
        .map(|receipt| {
            (
                receipt.response_body_exposed_bytes,
                receipt.response_header_exposed_bytes,
                receipt.object_count,
                receipt.ref_count,
            )
        })
        .unwrap_or_default();
    println!(
        "fetch elapsed_ms={} logical_store_bytes={} body_exposed_bytes={} header_exposed_bytes={} objects={} refs={} failure={:?} cleaned={}",
        attempt.elapsed.as_millis(),
        attempt.logical_store_bytes,
        body,
        headers,
        objects.map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
        refs.map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
        attempt.failure,
        attempt.cleaned
    );
}

fn validate_github_https(input: &str) -> Result<(), Failure> {
    let Some(rest) = input.strip_prefix("https://github.com/") else {
        return Err(Failure::InvalidEndpoint);
    };
    if input.bytes().any(|b| b.is_ascii_control())
        || ['?', '#', '@', ':', '%', '\\']
            .iter()
            .any(|ch| rest.contains(*ch))
    {
        return Err(Failure::InvalidEndpoint);
    }
    let path = rest.trim_end_matches(".git");
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 2
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
        || !parts.iter().all(|part| {
            part.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
    {
        return Err(Failure::InvalidEndpoint);
    }
    Ok(())
}

enum WorkerKind {
    Fetch(String),
    PrivateFetch {
        url: String,
        secret: Zeroizing<Vec<u8>>,
    },
}

fn read_bounded_secret(reader: impl BufRead) -> Result<Zeroizing<Vec<u8>>, Failure> {
    let mut secret = Zeroizing::new(Vec::with_capacity(MAX_SECRET_BYTES + 1));
    reader
        .take((MAX_SECRET_BYTES + 1) as u64)
        .read_until(b'\n', &mut secret)
        .map_err(|_| Failure::CredentialInput)?;
    if secret.last() != Some(&b'\n') {
        return Err(Failure::CredentialInput);
    }
    secret.pop();
    if secret.is_empty()
        || secret.len() > MAX_SECRET_BYTES
        || !secret.iter().all(|byte| (0x21..=0x7e).contains(byte))
    {
        return Err(Failure::CredentialInput);
    }
    Ok(secret)
}

fn ensure_non_terminal_stdin(is_terminal: bool) -> Result<(), Failure> {
    if is_terminal {
        Err(Failure::CredentialInput)
    } else {
        Ok(())
    }
}

fn secret_string(mut bytes: Zeroizing<Vec<u8>>) -> Result<Zeroizing<String>, Failure> {
    let owned = std::mem::take(bytes.as_mut());
    String::from_utf8(owned)
        .map(Zeroizing::new)
        .map_err(|_| Failure::CredentialInput)
}

#[allow(clippy::result_large_err)] // gix fixes this callback error type.
fn github_credential_callback(
    expected_url: String,
    secret: Arc<Zeroizing<String>>,
) -> impl FnMut(gix::credentials::helper::Action) -> gix::credentials::protocol::Result + 'static {
    let mut issued = false;
    move |action| match action {
        gix::credentials::helper::Action::Get(context) => {
            let url_matches = context.url.as_ref().is_some_and(|url| {
                let url_bytes: &[u8] = url.as_ref();
                url_bytes == expected_url.as_bytes()
            });
            if issued || !url_matches {
                return Ok(None);
            }
            issued = true;
            let next = context.clone().into();
            Ok(Some(gix::credentials::protocol::Outcome {
                identity: gix::sec::identity::Account {
                    username: "x-access-token".to_owned(),
                    password: secret.as_str().to_owned(),
                    oauth_refresh_token: None,
                },
                next,
            }))
        }
        gix::credentials::helper::Action::Store(_) | gix::credentials::helper::Action::Erase(_) => {
            Ok(None)
        }
    }
}

fn run_worker(kind: WorkerKind, deadline: Duration, byte_cap: u64) -> Result<Attempt, Failure> {
    preflight_free_space(&env::temp_dir(), available_free_bytes)?;
    let root = OwnedStore::create().map_err(|_| Failure::StoreAccounting)?;
    preflight_free_space(root.path(), available_free_bytes)?;
    let executable = env::current_exe().map_err(|_| Failure::WorkerFailed)?;
    let mut command = Command::new(executable);
    configure_worker_command(&mut command, &root);
    command.stdout(Stdio::piped());
    let secret_handoff = match kind {
        WorkerKind::Fetch(url) => {
            command
                .arg("--fetch-worker")
                .arg(root.path())
                .arg(root.receipt())
                .arg(url);
            None
        }
        WorkerKind::PrivateFetch { url, secret } => {
            command
                .arg("--private-fetch-worker")
                .arg(root.path())
                .arg(root.receipt())
                .arg(url)
                .stdin(Stdio::piped());
            Some(secret)
        }
    };
    let mut child = command.spawn().map_err(|_| Failure::WorkerFailed)?;
    let start = Instant::now();
    if let Some(secret) = secret_handoff {
        if handoff_secret(&mut child, secret.as_slice()).is_err() {
            return failed_handoff(child, root, start);
        }
    }
    supervise_with_space(child, root, deadline, byte_cap, available_free_bytes, true)
}

fn handoff_secret(child: &mut Child, secret: &[u8]) -> io::Result<()> {
    let mut input = child.stdin.take().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "private worker stdin unavailable",
        )
    })?;
    input.write_all(secret)?;
    input.write_all(b"\n")
}

fn failed_handoff(
    mut child: Child,
    mut store: OwnedStore,
    start: Instant,
) -> Result<Attempt, Failure> {
    let _ = child.kill();
    if child.wait().is_err() {
        store.retain();
        return Err(Failure::WorkerFailed);
    }
    let logical_store_bytes = store.logical_store_bytes().unwrap_or(0);
    let cleaned = store.cleanup();
    if !cleaned {
        store.retain();
    }
    Ok(Attempt {
        failure: Some(if cleaned {
            Failure::WorkerFailed
        } else {
            Failure::StoreAccounting
        }),
        elapsed: start.elapsed(),
        logical_store_bytes,
        cleaned,
        worker_receipt: None,
    })
}

fn preflight_free_space(
    path: &Path,
    mut observe: impl FnMut(&Path) -> Result<u64, Failure>,
) -> Result<(), Failure> {
    check_free_space(observe(path)?)
}

fn check_free_space(available: u64) -> Result<(), Failure> {
    if available < REQUIRED_FREE_SPACE_BYTES {
        Err(Failure::FreeSpace)
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn available_free_bytes(path: &Path) -> Result<u64, Failure> {
    let stats = rustix::fs::statvfs(path).map_err(|_| Failure::FreeSpace)?;
    let unit = if stats.f_frsize > 0 {
        stats.f_frsize
    } else {
        stats.f_bsize
    };
    stats
        .f_bavail
        .checked_mul(unit)
        .filter(|bytes| *bytes > 0)
        .ok_or(Failure::FreeSpace)
}

#[cfg(not(unix))]
fn available_free_bytes(_path: &Path) -> Result<u64, Failure> {
    Err(Failure::FreeSpace)
}

fn configure_worker_command(command: &mut Command, store: &OwnedStore) {
    command
        .env_clear()
        .current_dir(store.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("HOME", store.path())
        .env("TMPDIR", store.path())
        .env("PATH", "")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", store.path().join(EMPTY_GIT_CONFIG))
        .env("GIT_TERMINAL_PROMPT", "0");
}

#[cfg(test)]
fn supervise(
    child: Child,
    store: OwnedStore,
    deadline: Duration,
    byte_cap: u64,
) -> Result<Attempt, Failure> {
    supervise_with_space(
        child,
        store,
        deadline,
        byte_cap,
        available_free_bytes,
        false,
    )
}

fn supervise_with_space(
    mut child: Child,
    mut store: OwnedStore,
    deadline: Duration,
    byte_cap: u64,
    mut free_bytes: impl FnMut(&Path) -> Result<u64, Failure>,
    require_receipt: bool,
) -> Result<Attempt, Failure> {
    let start = Instant::now();
    let mut failure = None;
    let mut max_logical_bytes = 0;
    let status: ExitStatus;
    loop {
        match child.try_wait() {
            Ok(Some(done)) => {
                status = done;
                break;
            }
            Ok(None) => {}
            Err(_) => {
                failure = Some(Failure::WorkerFailed);
                let _ = child.kill();
                status = match child.wait() {
                    Ok(status) => status,
                    Err(_) => {
                        store.retain();
                        return Err(Failure::WorkerFailed);
                    }
                };
                break;
            }
        }
        let elapsed = start.elapsed();
        let observed = match store.logical_store_bytes() {
            Some(bytes) => bytes,
            None => {
                failure = Some(Failure::StoreAccounting);
                let _ = child.kill();
                status = match child.wait() {
                    Ok(status) => status,
                    Err(_) => {
                        store.retain();
                        return Err(Failure::WorkerFailed);
                    }
                };
                break;
            }
        };
        max_logical_bytes = max_logical_bytes.max(observed);
        let free_space = free_bytes(store.path()).and_then(check_free_space);
        if let Err(space_failure) = free_space {
            failure = Some(space_failure);
            let _ = child.kill();
            status = match child.wait() {
                Ok(status) => status,
                Err(_) => {
                    store.retain();
                    return Err(Failure::WorkerFailed);
                }
            };
            break;
        }
        failure = violation(elapsed, observed, deadline, byte_cap);
        if failure.is_some() {
            let _ = child.kill();
            status = match child.wait() {
                Ok(status) => status,
                Err(_) => {
                    store.retain();
                    return Err(Failure::WorkerFailed);
                }
            };
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }
    let elapsed = start.elapsed();
    match store.logical_store_bytes() {
        Some(bytes) => max_logical_bytes = max_logical_bytes.max(bytes),
        None => failure = Some(Failure::StoreAccounting),
    }
    if failure.is_none() {
        failure = violation(elapsed, max_logical_bytes, deadline, byte_cap);
    }
    if failure.is_none() {
        failure = free_bytes(store.path()).and_then(check_free_space).err();
    }
    let worker_receipt = if require_receipt {
        read_worker_receipt(child.stdout.take()).ok()
    } else {
        None
    };
    if require_receipt && worker_receipt.is_none() && failure.is_none() {
        failure = Some(Failure::WorkerFailed);
    }
    if !status.success() && failure.is_none() {
        failure = Some(Failure::WorkerFailed);
    }
    let cleaned = store.cleanup();
    if !cleaned {
        store.retain();
        failure = Some(Failure::StoreAccounting);
    }
    Ok(Attempt {
        failure,
        elapsed,
        logical_store_bytes: max_logical_bytes,
        cleaned,
        worker_receipt,
    })
}

fn read_worker_receipt(
    stdout: Option<std::process::ChildStdout>,
) -> Result<WorkerReceipt, Failure> {
    let stdout = stdout.ok_or(Failure::WorkerFailed)?;
    let mut bytes = Vec::with_capacity(MAX_RECEIPT_BYTES);
    stdout
        .take((MAX_RECEIPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::WorkerFailed)?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(Failure::WorkerFailed);
    }
    parse_worker_receipt(&bytes)
}

fn parse_worker_receipt(bytes: &[u8]) -> Result<WorkerReceipt, Failure> {
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(Failure::WorkerFailed);
    }
    let receipt: WorkerReceipt =
        serde_json::from_slice(bytes).map_err(|_| Failure::WorkerFailed)?;
    validate_worker_receipt(&receipt)?;
    Ok(receipt)
}

fn validate_worker_receipt(receipt: &WorkerReceipt) -> Result<(), Failure> {
    if receipt.schema_version != 1
        || !receipt.bare_fetch_completed
        || receipt.response_body_exposed_bytes > BOUNDED_PROBE_RESPONSE_BYTES as u32
        || receipt.response_header_exposed_bytes > BOUNDED_PROBE_HEADER_BYTES as u32
        || receipt
            .ref_count
            .is_none_or(|count| count > MAX_RECEIPT_REFS)
        || receipt
            .object_count
            .is_none_or(|count| count > MAX_RECEIPT_OBJECTS)
    {
        return Err(Failure::WorkerFailed);
    }
    Ok(())
}

fn violation(
    elapsed: Duration,
    logical_store_bytes: u64,
    deadline: Duration,
    cap: u64,
) -> Option<Failure> {
    if elapsed >= deadline {
        Some(Failure::Deadline)
    } else if logical_store_bytes > cap {
        Some(Failure::StoreBudget)
    } else {
        None
    }
}

struct OwnedStore {
    path: PathBuf,
    marker: PathBuf,
    receipt: String,
    identity: DirectoryIdentity,
    cleanup_on_drop: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DirectoryIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn directory_identity(path: &Path) -> io::Result<DirectoryIdentity> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owned store root changed type",
        ));
    }
    #[cfg(unix)]
    {
        Ok(DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "identity-safe store cleanup is unavailable",
        ))
    }
}

impl OwnedStore {
    fn create() -> io::Result<Self> {
        let base = env::temp_dir();
        for _ in 0..16 {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = base.join(format!(
                "disk-steward-git-https-{}-{nonce:x}-{:x}",
                std::process::id(),
                STORE_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    #[cfg(unix)]
                    if let Err(err) = fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                    {
                        let _ = fs::remove_dir(&path);
                        return Err(err);
                    }
                    let identity = match directory_identity(&path) {
                        Ok(identity) => identity,
                        Err(err) => {
                            let _ = fs::remove_dir(&path);
                            return Err(err);
                        }
                    };
                    let marker = path.join(OWNER_MARKER);
                    let mut random = [0u8; 16];
                    if let Err(err) = fs::File::open("/dev/urandom")
                        .and_then(|mut file| io::Read::read_exact(&mut file, &mut random))
                    {
                        let _ = fs::remove_dir(&path);
                        return Err(err);
                    }
                    let receipt = random
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    let marker_data =
                        format!("disk-steward-git-https-worker-probe-v1\n{receipt}\n");
                    let mut marker_options = fs::OpenOptions::new();
                    marker_options.write(true).create_new(true);
                    #[cfg(unix)]
                    marker_options.mode(0o600);
                    let marker_result = marker_options.open(&marker).and_then(|mut file| {
                        io::Write::write_all(&mut file, marker_data.as_bytes())
                    });
                    if let Err(err) = marker_result {
                        let _ = fs::remove_dir_all(&path);
                        return Err(err);
                    }
                    let empty_config = path.join(EMPTY_GIT_CONFIG);
                    let mut config_options = fs::OpenOptions::new();
                    config_options.write(true).create_new(true);
                    #[cfg(unix)]
                    config_options.mode(0o600);
                    if let Err(err) = config_options.open(empty_config) {
                        let _ = fs::remove_dir_all(&path);
                        return Err(err);
                    }
                    return Ok(Self {
                        path,
                        marker,
                        receipt,
                        identity,
                        cleanup_on_drop: true,
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "private store name collisions",
        ))
    }

    fn open_watcher(path: &Path, receipt: &str) -> io::Result<Self> {
        let marker = path.join(OWNER_MARKER);
        if !path.is_absolute() || !private_owner_marker(path, &marker)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "worker store must be an absolute private directory",
            ));
        }
        let identity = directory_identity(path)?;
        let expected = format!("disk-steward-git-https-worker-probe-v1\n{receipt}\n");
        if !fs::read(&marker).is_ok_and(|bytes| bytes == expected.as_bytes()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "worker store owner receipt is invalid",
            ));
        }
        let repository = path.join("objects.git");
        match fs::symlink_metadata(repository) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "worker object store path must be new",
                ));
            }
        }
        Ok(Self {
            path: path.to_owned(),
            marker,
            receipt: receipt.to_owned(),
            identity,
            cleanup_on_drop: false,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn receipt(&self) -> &str {
        &self.receipt
    }

    fn logical_store_bytes(&self) -> Option<u64> {
        fn walk(path: &Path) -> io::Result<u64> {
            let mut total = 0u64;
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let ty = entry.file_type()?;
                if ty.is_symlink() {
                    continue;
                }
                if ty.is_dir() {
                    total = total.saturating_add(walk(&entry.path())?);
                } else if ty.is_file() {
                    total = total.saturating_add(entry.metadata()?.len());
                }
            }
            Ok(total)
        }
        let identity = directory_identity(&self.path).ok()?;
        let expected = format!("disk-steward-git-https-worker-probe-v1\n{}\n", self.receipt);
        let marker_matches = private_owner_marker(&self.path, &self.marker).unwrap_or(false)
            && fs::read(&self.marker).is_ok_and(|bytes| bytes == expected.as_bytes());
        if identity != self.identity || !marker_matches {
            return None;
        }
        walk(&self.path).ok()
    }

    fn cleanup(&mut self) -> bool {
        let root_matches =
            directory_identity(&self.path).is_ok_and(|identity| identity == self.identity);
        if !root_matches {
            return false;
        }
        let marker_is_regular = private_owner_marker(&self.path, &self.marker).unwrap_or(false);
        let marker_matches = marker_is_regular
            && fs::read(&self.marker).is_ok_and(|bytes| {
                bytes
                    == format!("disk-steward-git-https-worker-probe-v1\n{}\n", self.receipt)
                        .as_bytes()
            });
        marker_matches && fs::remove_dir_all(&self.path).is_ok()
    }

    fn retain(&mut self) {
        self.cleanup_on_drop = false;
    }
}

#[cfg(unix)]
fn private_owner_marker(root: &Path, marker: &Path) -> io::Result<bool> {
    let root_metadata = fs::symlink_metadata(root)?;
    let marker_metadata = fs::symlink_metadata(marker)?;
    Ok(root_metadata.is_dir()
        && !root_metadata.file_type().is_symlink()
        && root_metadata.permissions().mode() & 0o077 == 0
        && marker_metadata.is_file()
        && !marker_metadata.file_type().is_symlink()
        && marker_metadata.permissions().mode() & 0o077 == 0
        && root_metadata.uid() == marker_metadata.uid())
}

#[cfg(not(unix))]
fn private_owner_marker(_root: &Path, _marker: &Path) -> io::Result<bool> {
    Ok(false)
}

struct WorkerGuard {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl WorkerGuard {
    fn start<F>(
        path: &Path,
        receipt: &str,
        deadline: Duration,
        byte_cap: u64,
        mut free_bytes: F,
    ) -> Result<Self, Failure>
    where
        F: FnMut(&Path) -> Result<u64, Failure> + Send + 'static,
    {
        let store =
            OwnedStore::open_watcher(path, receipt).map_err(|_| Failure::StoreAccounting)?;
        preflight_free_space(store.path(), &mut free_bytes)?;
        let start = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let join = thread::Builder::new()
            .name("git-https-worker-guard".into())
            .spawn(move || loop {
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }
                let failure = if start.elapsed() >= deadline {
                    Some(Failure::Deadline)
                } else {
                    match store.logical_store_bytes() {
                        Some(bytes) if bytes <= byte_cap => {
                            free_bytes(store.path()).and_then(check_free_space).err()
                        }
                        Some(_) => Some(Failure::StoreBudget),
                        None => Some(Failure::StoreAccounting),
                    }
                };
                if failure.is_some() {
                    // The parent still owns cleanup. Direct invocation retains the
                    // caller's directory because this exit skips all destructors.
                    std::process::exit(WORKER_GUARD_EXIT_CODE);
                }
                thread::sleep(POLL_INTERVAL);
            })
            .map_err(|_| Failure::WorkerFailed)?;
        Ok(Self {
            stop,
            join: Some(join),
        })
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for OwnedStore {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = self.cleanup();
        }
    }
}

fn worker_entry(
    store: Option<OsString>,
    receipt: Option<OsString>,
    url: Option<OsString>,
    private: bool,
) -> Result<(), Failure> {
    let receipt = worker(store, receipt, url, private)?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, &receipt).map_err(|_| Failure::WorkerFailed)?;
    output.write_all(b"\n").map_err(|_| Failure::WorkerFailed)?;
    output.flush().map_err(|_| Failure::WorkerFailed)
}

#[allow(clippy::result_large_err)] // gix's credential callback fixes this error type.
fn worker(
    store: Option<OsString>,
    receipt: Option<OsString>,
    url: Option<OsString>,
    private: bool,
) -> Result<WorkerReceipt, Failure> {
    let store = store.ok_or(Failure::InvalidEndpoint)?;
    let receipt = receipt
        .ok_or(Failure::InvalidEndpoint)?
        .to_string_lossy()
        .into_owned();
    let url = url
        .ok_or(Failure::InvalidEndpoint)?
        .to_string_lossy()
        .into_owned();
    validate_github_https(&url)?;
    let stdin = io::stdin();
    if private {
        ensure_non_terminal_stdin(stdin.is_terminal())?;
    }
    let store_root = PathBuf::from(store);
    // This child-local read-only guard makes the fixed limits mandatory even
    // when someone invokes the internal worker mode without its parent.
    let worker_guard = WorkerGuard::start(
        &store_root,
        &receipt,
        BOUNDED_PROBE_DEADLINE,
        BOUNDED_PROBE_STORE_BYTES,
        available_free_bytes,
    )?;
    let secret = if private {
        Some(secret_string(read_bounded_secret(stdin.lock())?)?)
    } else {
        None
    };
    let secret = secret.map(Arc::new);
    let attempt_budget = AttemptBudget::new(
        BOUNDED_PROBE_RESPONSE_BYTES,
        BOUNDED_PROBE_HEADER_BYTES,
        Instant::now() + BOUNDED_PROBE_DEADLINE,
    );
    let connection_budget = attempt_budget.clone();
    let authentication_url = url.clone();
    let repository_path = store_root.join("objects.git");
    let mut prepare = gix::prepare_clone_bare(url, &repository_path)
        .map_err(|_| Failure::WorkerFailed)?
        .with_in_memory_config_overrides([
            "credential.helper=",
            "http.followRedirects=false",
            "http.proxy=",
            "http.extraHeader=",
            "http.sslVerify=true",
            "gitoxide.http.proxy=",
            "gitoxide.https.proxy=",
            "gitoxide.http.allProxy=",
        ]);
    prepare = prepare.configure_connection(move |connection| {
        let url = connection
            .remote()
            .url(gix::remote::Direction::Fetch)
            .cloned()
            .ok_or_else(|| io::Error::other("fetch URL unavailable"))?;
        let http = BudgetedHttp::new(
            gix_transport::client::blocking_io::http::reqwest::Remote::default(),
            connection_budget.clone(),
        );
        let transport = gix_transport::client::blocking_io::http::Transport::new_http(
            http,
            url,
            gix::protocol::transport::Protocol::V2,
            false,
        );
        *connection.transport_mut() = Box::new(transport);
        if let Some(secret) = secret.clone() {
            connection.set_credentials(github_credential_callback(
                authentication_url.clone(),
                secret,
            ));
        } else {
            connection.set_credentials(|_| Ok(None));
        }
        Ok(())
    });
    let interrupted = AtomicBool::new(false);
    let (_repository, fetch_outcome) = prepare
        .fetch_only(gix::progress::Discard, &interrupted)
        .map_err(|_| Failure::WorkerFailed)?;
    let ref_count = u32::try_from(fetch_outcome.ref_map.remote_refs.len())
        .map_err(|_| Failure::WorkerFailed)?;
    let object_count = match fetch_outcome.status {
        gix::remote::fetch::Status::NoPackReceived { .. } => Some(0),
        gix::remote::fetch::Status::Change {
            write_pack_bundle, ..
        } => Some(write_pack_bundle.index.num_objects),
    };
    let receipt = WorkerReceipt {
        schema_version: 1,
        bare_fetch_completed: true,
        response_body_exposed_bytes: u32::try_from(attempt_budget.body_bytes_used())
            .map_err(|_| Failure::WorkerFailed)?,
        response_header_exposed_bytes: u32::try_from(attempt_budget.header_bytes_used())
            .map_err(|_| Failure::WorkerFailed)?,
        object_count,
        ref_count: Some(ref_count),
    };
    validate_worker_receipt(&receipt)?;
    worker_guard.finish();
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_validation_rejects_credentials_queries_and_other_hosts() {
        assert_eq!(
            validate_github_https("https://github.com/octo/repo.git"),
            Ok(())
        );
        for bad in [
            "https://user:secret@github.com/octo/repo.git",
            "https://github.com/octo/repo.git?token=secret",
            "https://example.com/octo/repo.git",
            "ssh://github.com/octo/repo.git",
            "https://github.com/octo/repo.git/extra",
        ] {
            assert_eq!(validate_github_https(bad), Err(Failure::InvalidEndpoint));
        }
        assert!(!Failure::WorkerFailed.as_str().contains("secret"));
    }

    #[test]
    fn secret_input_is_ascii_nonempty_and_limited_to_512_bytes() {
        assert_eq!(
            read_bounded_secret(io::Cursor::new(b"")),
            Err(Failure::CredentialInput)
        );
        assert_eq!(
            read_bounded_secret(io::Cursor::new(vec![b'a'; MAX_SECRET_BYTES + 1])),
            Err(Failure::CredentialInput)
        );
        assert_eq!(
            read_bounded_secret(io::Cursor::new(b"has space\n".to_vec())),
            Err(Failure::CredentialInput)
        );
        assert_eq!(
            read_bounded_secret(io::Cursor::new(b"bad\rline\n".to_vec())),
            Err(Failure::CredentialInput)
        );
        assert_eq!(
            read_bounded_secret(io::Cursor::new(vec![0xff, b'\n'])),
            Err(Failure::CredentialInput)
        );
        let mut maximum_input = vec![b'a'; MAX_SECRET_BYTES];
        maximum_input.push(b'\n');
        let maximum = read_bounded_secret(io::Cursor::new(maximum_input)).unwrap();
        assert_eq!(maximum.len(), MAX_SECRET_BYTES);
    }

    #[test]
    fn direct_private_worker_rejects_terminal_stdin_before_secret_read() {
        assert_eq!(
            ensure_non_terminal_stdin(true),
            Err(Failure::CredentialInput)
        );
        assert_eq!(ensure_non_terminal_stdin(false), Ok(()));
        // The direct worker runs this check before constructing its stdin
        // reader; the line parser still accepts valid pipe input.
        assert_eq!(
            read_bounded_secret(io::Cursor::new(b"synthetic-canary\n")),
            Ok(Zeroizing::new(b"synthetic-canary".to_vec()))
        );
    }

    #[test]
    fn github_credential_callback_is_exact_url_single_issue_and_never_stores() {
        const CANARY: &str = "synthetic-private-token-canary-41";
        let url = "https://github.com/example/private.git";
        let secret = Arc::new(Zeroizing::new(CANARY.to_owned()));
        let mut callback = github_credential_callback(url.to_owned(), secret);
        let outcome = callback(gix::credentials::helper::Action::get_for_url(url))
            .unwrap()
            .unwrap();
        assert_eq!(outcome.identity.username, "x-access-token");
        assert_eq!(outcome.identity.password, CANARY);
        assert!(callback(outcome.next.store()).unwrap().is_none());
        assert!(callback(gix::credentials::helper::Action::get_for_url(url))
            .unwrap()
            .is_none());

        let mut wrong_url =
            github_credential_callback(url.to_owned(), Arc::new(Zeroizing::new(CANARY.to_owned())));
        assert!(wrong_url(gix::credentials::helper::Action::get_for_url(
            "https://github.com/other/private.git"
        ))
        .unwrap()
        .is_none());

        let mut rejected =
            github_credential_callback(url.to_owned(), Arc::new(Zeroizing::new(CANARY.to_owned())));
        let outcome = rejected(gix::credentials::helper::Action::get_for_url(url))
            .unwrap()
            .unwrap();
        assert!(rejected(outcome.next.erase()).unwrap().is_none());
        assert!(rejected(gix::credentials::helper::Action::get_for_url(url))
            .unwrap()
            .is_none());
    }

    #[test]
    fn worker_receipt_is_bounded_strict_and_secret_free() {
        let valid = br#"{"schema_version":1,"bare_fetch_completed":true,"response_body_exposed_bytes":1200,"response_header_exposed_bytes":2048,"object_count":12,"ref_count":4}"#;
        let receipt = parse_worker_receipt(valid).unwrap();
        assert_eq!(receipt.response_body_exposed_bytes, 1200);
        assert_eq!(receipt.object_count, Some(12));
        assert_eq!(receipt.ref_count, Some(4));
        for invalid in [
            br#"{"schema_version":1,"bare_fetch_completed":true,"response_body_exposed_bytes":0,"response_header_exposed_bytes":0,"object_count":0,"ref_count":0,"token":"secret"}"#.as_slice(),
            br#"{"schema_version":1,"bare_fetch_completed":false,"response_body_exposed_bytes":0,"response_header_exposed_bytes":0,"object_count":0,"ref_count":0}"#.as_slice(),
            br#"{"schema_version":1,"bare_fetch_completed":true,"response_body_exposed_bytes":33554433,"response_header_exposed_bytes":0,"object_count":0,"ref_count":0}"#.as_slice(),
        ] {
            assert_eq!(parse_worker_receipt(invalid), Err(Failure::WorkerFailed));
        }
        assert_eq!(
            parse_worker_receipt(&vec![b' '; MAX_RECEIPT_BYTES + 1]),
            Err(Failure::WorkerFailed)
        );
    }

    #[test]
    fn parent_verifies_child_receipt_before_owned_store_cleanup() {
        let store = OwnedStore::create().unwrap();
        let path = store.path().to_owned();
        let json = r#"{"schema_version":1,"bare_fetch_completed":true,"response_body_exposed_bytes":100,"response_header_exposed_bytes":40,"object_count":7,"ref_count":3}"#;
        let child = Command::new("/usr/bin/printf")
            .arg("%s\\n")
            .arg(json)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let attempt = supervise_with_space(
            child,
            store,
            Duration::from_secs(2),
            1024 * 1024,
            |_| Ok(REQUIRED_FREE_SPACE_BYTES),
            true,
        )
        .unwrap();
        assert_eq!(attempt.failure, None);
        assert_eq!(attempt.worker_receipt.unwrap().ref_count, Some(3));
        assert!(attempt.cleaned);
        assert!(!path.exists());
    }

    #[test]
    fn malformed_child_receipt_fails_before_owned_store_cleanup() {
        let store = OwnedStore::create().unwrap();
        let path = store.path().to_owned();
        let json = r#"{"schema_version":1,"bare_fetch_completed":true,"response_body_exposed_bytes":100,"response_header_exposed_bytes":40,"object_count":7,"ref_count":3,"token":"canary"}"#;
        let child = Command::new("/usr/bin/printf")
            .arg("%s\\n")
            .arg(json)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let attempt = supervise_with_space(
            child,
            store,
            Duration::from_secs(2),
            1024 * 1024,
            |_| Ok(REQUIRED_FREE_SPACE_BYTES),
            true,
        )
        .unwrap();
        assert_eq!(attempt.failure, Some(Failure::WorkerFailed));
        assert!(attempt.worker_receipt.is_none());
        assert!(attempt.cleaned);
        assert!(!path.exists());
    }

    #[test]
    fn successful_exit_without_fetch_receipt_fails_closed() {
        let store = OwnedStore::create().unwrap();
        let path = store.path().to_owned();
        let child = Command::new("/usr/bin/true")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let attempt = supervise_with_space(
            child,
            store,
            Duration::from_secs(2),
            1024 * 1024,
            |_| Ok(REQUIRED_FREE_SPACE_BYTES),
            true,
        )
        .unwrap();
        assert_eq!(attempt.failure, Some(Failure::WorkerFailed));
        assert!(attempt.worker_receipt.is_none());
        assert!(attempt.cleaned);
        assert!(!path.exists());
    }

    #[test]
    fn private_canary_is_absent_from_child_output_and_store_files() {
        const CANARY: &[u8] = b"synthetic-private-token-canary-62";
        let mut store = OwnedStore::create().unwrap();
        let mut command = Command::new(env::current_exe().unwrap());
        configure_worker_command(&mut command, &store);
        let mut child = command
            .args([
                "--exact",
                "tests::private_credential_child_fixture",
                "--nocapture",
            ])
            .env("GIT_HTTPS_PROBE_TEST_CHILD", "canary")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(CANARY).unwrap();
        input.write_all(b"\n").unwrap();
        drop(input);
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let canary_b64 = base64_test_encode(CANARY);
        let basic_b64 = base64_test_encode(
            format!("x-access-token:{}", String::from_utf8_lossy(CANARY)).as_bytes(),
        );
        for output in [&output.stdout, &output.stderr] {
            assert!(!contains_bytes(output, CANARY));
            assert!(!contains_bytes(output, canary_b64.as_bytes()));
            assert!(!contains_bytes(
                output,
                format!("Basic {basic_b64}").as_bytes()
            ));
            assert!(!contains_bytes(output, basic_b64.as_bytes()));
        }
        let files = collect_files(store.path());
        assert!(!contains_bytes(&files, CANARY));
        assert!(!contains_bytes(&files, canary_b64.as_bytes()));
        assert!(!contains_bytes(
            &files,
            format!("Basic {basic_b64}").as_bytes()
        ));
        assert!(!contains_bytes(&files, basic_b64.as_bytes()));
        assert!(store.cleanup());
        store.retain();
    }

    #[test]
    fn failed_secret_handoff_waits_and_cleans_only_owned_store() {
        let store = OwnedStore::create().unwrap();
        let path = store.path().to_owned();
        let mut command = Command::new(env::current_exe().unwrap());
        configure_worker_command(&mut command, &store);
        let mut child = command
            .args(["--exact", "tests::child_fixture", "--nocapture"])
            .env("GIT_HTTPS_PROBE_TEST_CHILD", "fast-exit")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.wait();
        assert!(handoff_secret(&mut child, b"synthetic-canary").is_err());
        let attempt = failed_handoff(child, store, Instant::now()).unwrap();
        assert_eq!(attempt.failure, Some(Failure::WorkerFailed));
        assert!(attempt.cleaned);
        assert!(!path.exists());
    }

    #[test]
    fn closed_secret_pipe_is_rejected_before_private_worker_work() {
        let mut store = OwnedStore::create().unwrap();
        let mut command = Command::new(env::current_exe().unwrap());
        configure_worker_command(&mut command, &store);
        let mut child = command
            .args([
                "--exact",
                "tests::private_credential_child_fixture",
                "--nocapture",
            ])
            .env("GIT_HTTPS_PROBE_TEST_CHILD", "closed")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdin.take().unwrap());
        assert!(child.wait_with_output().unwrap().status.success());
        assert!(store.cleanup());
        store.retain();
    }

    #[test]
    fn private_credential_child_fixture() {
        let Ok(mode) = env::var("GIT_HTTPS_PROBE_TEST_CHILD") else {
            return;
        };
        if mode == "closed" {
            assert_eq!(
                read_bounded_secret(io::stdin().lock()).err(),
                Some(Failure::CredentialInput)
            );
            return;
        }
        if mode != "canary" {
            return;
        }
        const CANARY: &str = "synthetic-private-token-canary-62";
        let url = "https://github.com/example/private.git";
        let secret = secret_string(read_bounded_secret(io::stdin().lock()).unwrap()).unwrap();
        let mut callback = github_credential_callback(url.to_owned(), Arc::new(secret));
        let outcome = callback(gix::credentials::helper::Action::get_for_url(url))
            .unwrap()
            .unwrap();
        assert!(
            outcome.identity.password == CANARY,
            "synthetic credential mapping failed"
        );
        let _ = callback(outcome.next.erase()).unwrap();
        assert!(callback(gix::credentials::helper::Action::get_for_url(url))
            .unwrap()
            .is_none());
    }

    #[test]
    fn private_store_cleanup_requires_its_owner_receipt() {
        let mut store = OwnedStore::create().unwrap();
        assert!(store.path().is_absolute());
        assert!(store.logical_store_bytes().is_some());
        assert!(store.cleanup());
        assert!(!store.path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn worker_store_refuses_existing_object_store_symlink() {
        use std::os::unix::fs::symlink;

        let mut store = OwnedStore::create().unwrap();
        let sentinel_root = store.path().with_extension("worker-sentinel");
        fs::create_dir(&sentinel_root).unwrap();
        let sentinel = sentinel_root.join("caller-data.txt");
        fs::write(&sentinel, b"caller-owned data").unwrap();
        symlink(&sentinel_root, store.path().join("objects.git")).unwrap();
        assert!(OwnedStore::open_watcher(store.path(), store.receipt()).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"caller-owned data");
        fs::remove_file(store.path().join("objects.git")).unwrap();
        assert!(store.cleanup());
        store.retain();
        fs::remove_dir_all(sentinel_root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn worker_store_refuses_nonprivate_owner_marker() {
        use std::os::unix::fs::PermissionsExt;

        let mut store = OwnedStore::create().unwrap();
        let marker = store.path().join(OWNER_MARKER);
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(OwnedStore::open_watcher(store.path(), store.receipt()).is_err());
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(store.cleanup());
        store.retain();
    }

    #[test]
    fn free_space_threshold_fails_closed_before_allocation() {
        assert_eq!(
            check_free_space(REQUIRED_FREE_SPACE_BYTES - 1),
            Err(Failure::FreeSpace)
        );
        assert_eq!(check_free_space(REQUIRED_FREE_SPACE_BYTES), Ok(()));
    }

    #[test]
    fn injected_low_preflight_observation_fails_closed() {
        let fixture_path = Path::new("/synthetic-volume");
        let mut observed_path = None;
        let result = preflight_free_space(fixture_path, |path| {
            observed_path = Some(path.to_owned());
            Ok(REQUIRED_FREE_SPACE_BYTES - 1)
        });
        assert_eq!(result, Err(Failure::FreeSpace));
        assert_eq!(observed_path.as_deref(), Some(fixture_path));
    }

    #[test]
    fn free_space_drop_kills_joins_and_cleans_worker() {
        let store = OwnedStore::create().unwrap();
        let path = store.path().to_owned();
        let child = test_child(&store, "sleep");
        let mut samples = 0;
        let attempt = supervise_with_space(
            child,
            store,
            Duration::from_secs(2),
            1024 * 1024,
            |_| {
                samples += 1;
                if samples == 1 {
                    Ok(REQUIRED_FREE_SPACE_BYTES)
                } else {
                    Ok(REQUIRED_FREE_SPACE_BYTES - 1)
                }
            },
            false,
        )
        .unwrap();
        assert_eq!(attempt.failure, Some(Failure::FreeSpace));
        assert!(attempt.cleaned);
        assert!(!path.exists());
        assert!(attempt.elapsed < Duration::from_secs(2));
        assert!(samples >= 2);
    }

    #[test]
    fn free_space_observation_error_kills_joins_and_cleans_worker() {
        let store = OwnedStore::create().unwrap();
        let path = store.path().to_owned();
        let child = test_child(&store, "sleep");
        let attempt = supervise_with_space(
            child,
            store,
            Duration::from_secs(2),
            1024 * 1024,
            |_| Err(Failure::FreeSpace),
            false,
        )
        .unwrap();
        assert_eq!(attempt.failure, Some(Failure::FreeSpace));
        assert!(attempt.cleaned);
        assert!(!path.exists());
    }

    #[test]
    fn unparented_worker_guard_enforces_limits_and_retains_caller_store() {
        for mode in ["guard-deadline", "guard-low-space"] {
            let mut store = OwnedStore::create().unwrap();
            let sentinel = store.path().join("caller-data.txt");
            fs::write(&sentinel, b"caller-owned data").unwrap();
            let receipt = store.receipt().to_owned();
            if mode == "guard-deadline" {
                fs::write(
                    store.path().join(OWNER_MARKER),
                    "disk-steward-git-https-worker-probe-v1\nforged-direct-receipt\n",
                )
                .unwrap();
            }
            let mut command = Command::new(env::current_exe().unwrap());
            configure_worker_command(&mut command, &store);
            let mut child = command
                .args(["--exact", "tests::child_fixture", "--nocapture"])
                .env("GIT_HTTPS_PROBE_TEST_CHILD", mode)
                .spawn()
                .unwrap();
            let status = child.wait().unwrap();
            assert_eq!(status.code(), Some(WORKER_GUARD_EXIT_CODE), "{mode}");
            assert_eq!(fs::read(&sentinel).unwrap(), b"caller-owned data");
            assert!(store.path().exists());
            assert!(!store.path().join("objects.git").exists());
            if mode == "guard-deadline" {
                fs::write(
                    store.path().join(OWNER_MARKER),
                    format!("disk-steward-git-https-worker-probe-v1\n{receipt}\n"),
                )
                .unwrap();
            }
            assert!(store.cleanup());
            store.retain();
        }
    }

    #[test]
    fn worker_is_killed_joined_and_owned_store_cleaned_at_deadline() {
        let store = OwnedStore::create().unwrap();
        let child = test_child(&store, "deadline");
        let attempt = supervise(child, store, Duration::from_millis(100), 1024 * 1024).unwrap();
        assert_eq!(attempt.failure, Some(Failure::Deadline));
        assert!(attempt.cleaned);
        assert!(attempt.elapsed < Duration::from_secs(2));
    }

    #[test]
    fn child_fixture() {
        let Ok(mode) = env::var("GIT_HTTPS_PROBE_TEST_CHILD") else {
            return;
        };
        let store = env::current_dir().unwrap();
        if mode == "guard-deadline" || mode == "guard-low-space" {
            let marker_data = fs::read_to_string(store.join(OWNER_MARKER)).unwrap();
            let receipt = marker_data.lines().nth(1).unwrap().to_owned();
            let low_space = mode == "guard-low-space";
            let mut samples = 0;
            let _guard = WorkerGuard::start(
                &store,
                &receipt,
                Duration::from_millis(150),
                1024 * 1024,
                move |_| {
                    samples += 1;
                    if low_space && samples > 1 {
                        Ok(REQUIRED_FREE_SPACE_BYTES - 1)
                    } else {
                        Ok(REQUIRED_FREE_SPACE_BYTES)
                    }
                },
            )
            .unwrap();
            thread::sleep(Duration::from_secs(5));
            return;
        }
        if mode == "bytes" || mode == "fast-write" {
            fs::write(store.join("child-output"), vec![7u8; 256 * 1024]).unwrap();
        }
        if mode == "fast-write" {
            return;
        }
        if mode == "fast-exit" {
            return;
        }
        if mode == "environment" {
            assert!(env::var_os("PROBE_API_KEY_CANARY").is_none());
            assert!(env::var_os("HTTPS_PROXY").is_none());
            assert!(env::var_os("https_proxy").is_none());
            assert!(env::var_os("HTTP_PROXY").is_none());
            assert!(env::var_os("http_proxy").is_none());
            assert!(env::var_os("ALL_PROXY").is_none());
            assert!(env::var_os("all_proxy").is_none());
            assert_eq!(env::var("GIT_CONFIG_NOSYSTEM").as_deref(), Ok("1"));
            assert_eq!(env::var("GIT_TERMINAL_PROMPT").as_deref(), Ok("0"));
            assert_eq!(
                env::var_os("PATH").as_deref(),
                Some(std::ffi::OsStr::new(""))
            );
            for key in ["HOME", "TMPDIR"] {
                let value = env::var_os(key).unwrap();
                assert_eq!(
                    fs::canonicalize(value).unwrap(),
                    fs::canonicalize(&store).unwrap()
                );
            }
            let global_config = env::var_os("GIT_CONFIG_GLOBAL").unwrap();
            assert_eq!(
                fs::canonicalize(global_config).unwrap(),
                fs::canonicalize(store.join(EMPTY_GIT_CONFIG)).unwrap()
            );
            assert_eq!(fs::read(store.join(EMPTY_GIT_CONFIG)).unwrap(), b"");
            return;
        }
        thread::sleep(Duration::from_secs(5));
    }

    #[test]
    fn worker_store_growth_over_budget_stops_child_before_cleanup() {
        let store = OwnedStore::create().unwrap();
        let child = test_child(&store, "bytes");
        let attempt = supervise(child, store, Duration::from_secs(2), 64 * 1024).unwrap();
        assert_eq!(attempt.failure, Some(Failure::StoreBudget));
        assert!(attempt.logical_store_bytes > 64 * 1024);
        assert!(attempt.cleaned);
    }

    #[test]
    fn exited_worker_limits_are_checked_after_exit() {
        let store = OwnedStore::create().unwrap();
        let mut child = test_child(&store, "fast-write");
        let _ = child.wait();
        let attempt = supervise(child, store, Duration::from_secs(2), 64 * 1024).unwrap();
        assert_eq!(attempt.failure, Some(Failure::StoreBudget));
        assert!(attempt.cleaned);
    }

    #[test]
    fn exited_worker_deadline_is_checked_after_exit() {
        let store = OwnedStore::create().unwrap();
        let mut child = test_child(&store, "fast-exit");
        let _ = child.wait();
        let attempt = supervise(child, store, Duration::ZERO, 1024 * 1024).unwrap();
        assert_eq!(attempt.failure, Some(Failure::Deadline));
        assert!(attempt.cleaned);
    }

    #[test]
    fn worker_environment_discards_parent_secrets_and_proxy_settings() {
        let store = OwnedStore::create().unwrap();
        let mut command = Command::new(env::current_exe().unwrap());
        command.env("PROBE_API_KEY_CANARY", "synthetic-secret");
        command.env(
            "HTTPS_PROXY",
            "http://synthetic-user:synthetic-secret@proxy.invalid",
        );
        command.env(
            "https_proxy",
            "http://synthetic-user:synthetic-secret@proxy.invalid",
        );
        command.env("HTTP_PROXY", "http://synthetic-secret@proxy.invalid");
        command.env("http_proxy", "http://synthetic-secret@proxy.invalid");
        command.env("ALL_PROXY", "http://synthetic-secret@proxy.invalid");
        command.env("all_proxy", "http://synthetic-secret@proxy.invalid");
        configure_worker_command(&mut command, &store);
        let child = command
            .args(["--exact", "tests::child_fixture", "--nocapture"])
            .env("GIT_HTTPS_PROBE_TEST_CHILD", "environment")
            .spawn()
            .unwrap();
        let attempt = supervise(child, store, Duration::from_secs(2), 1024 * 1024).unwrap();
        assert_eq!(attempt.failure, None);
        assert!(attempt.cleaned);
    }

    #[cfg(unix)]
    #[test]
    fn store_replacement_symlink_is_retained_without_touching_sentinel() {
        use std::os::unix::fs::symlink;

        let mut store = OwnedStore::create().unwrap();
        let original = store.path().to_owned();
        let moved = original.with_extension("owned-moved");
        let sentinel_root = original.with_extension("sentinel");
        fs::create_dir(&sentinel_root).unwrap();
        let sentinel = sentinel_root.join("keep.txt");
        fs::write(&sentinel, b"unrelated fixture").unwrap();
        fs::rename(&original, &moved).unwrap();
        symlink(&sentinel_root, &original).unwrap();
        assert!(!store.cleanup());
        assert_eq!(fs::read(&sentinel).unwrap(), b"unrelated fixture");
        fs::remove_file(&original).unwrap();
        fs::rename(&moved, &original).unwrap();
        assert!(store.cleanup());
        fs::remove_dir_all(sentinel_root).unwrap();
        store.retain();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_failure_overrides_success_and_retains_replaced_store() {
        use std::os::unix::fs::symlink;

        let store = OwnedStore::create().unwrap();
        let original = store.path().to_owned();
        let moved = original.with_extension("owned-retained");
        let sentinel_root = original.with_extension("sentinel-retained");
        fs::create_dir(&sentinel_root).unwrap();
        let sentinel = sentinel_root.join("keep.txt");
        fs::write(&sentinel, b"unrelated fixture").unwrap();

        let mut child = test_child(&store, "fast-exit");
        let _ = child.wait();
        fs::rename(&original, &moved).unwrap();
        symlink(&sentinel_root, &original).unwrap();
        let attempt = supervise(child, store, Duration::from_secs(2), 1024 * 1024).unwrap();

        assert_eq!(attempt.failure, Some(Failure::StoreAccounting));
        assert!(!attempt.cleaned);
        assert!(moved.join(OWNER_MARKER).is_file());
        assert_eq!(fs::read(&sentinel).unwrap(), b"unrelated fixture");
        fs::remove_file(&original).unwrap();
        fs::remove_dir_all(&moved).unwrap();
        fs::remove_dir_all(sentinel_root).unwrap();
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty()
            && haystack
                .windows(needle.len())
                .any(|window| window == needle)
    }

    fn base64_test_encode(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut output = String::new();
        for chunk in input.chunks(3) {
            let first = chunk[0];
            let second = *chunk.get(1).unwrap_or(&0);
            let third = *chunk.get(2).unwrap_or(&0);
            let indexes = [
                first >> 2,
                ((first & 0x03) << 4) | (second >> 4),
                ((second & 0x0f) << 2) | (third >> 6),
                third & 0x3f,
            ];
            output.push(TABLE[indexes[0] as usize] as char);
            output.push(TABLE[indexes[1] as usize] as char);
            output.push(if chunk.len() > 1 {
                TABLE[indexes[2] as usize] as char
            } else {
                '='
            });
            output.push(if chunk.len() > 2 {
                TABLE[indexes[3] as usize] as char
            } else {
                '='
            });
        }
        output
    }

    fn collect_files(path: &Path) -> Vec<u8> {
        let mut contents = Vec::new();
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                contents.extend(collect_files(&entry.path()));
            } else if kind.is_file() {
                contents.extend(fs::read(entry.path()).unwrap());
            }
        }
        contents
    }

    fn test_child(store: &OwnedStore, mode: &str) -> Child {
        let mut command = Command::new(env::current_exe().unwrap());
        configure_worker_command(&mut command, store);
        command
            .args(["--exact", "tests::child_fixture", "--nocapture"])
            .env("GIT_HTTPS_PROBE_TEST_CHILD", mode)
            .spawn()
            .unwrap()
    }
}
