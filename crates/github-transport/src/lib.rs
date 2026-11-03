//! A narrowly scoped gix GitHub HTTPS transport candidate.
//!
//! The constructor validates input without connecting. DNS uses the platform
//! resolver and cannot be interrupted here; callers need an outer worker
//! deadline. See the crate README for the current scope and limits.

mod checksum;
pub use checksum::{check_sha1_pack_checksum, PackChecksumReport};
mod pack;
mod refs;
pub use pack::PackTransferReport;
pub use refs::{AdvertisedRef, RefObservationReport};

#[cfg(not(test))]
use std::net::ToSocketAddrs;
use std::{
    any::Any,
    borrow::Cow,
    fmt,
    io::{self, BufRead, Cursor, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

use gix_error::{ErrorExt, ExnMessageResult, ExnResult};
use gix_transport::{
    bstr::BStr,
    client::{
        blocking_io::http::{GetResponse, Http, PostBodyDataKind, PostResponse},
        Account, TransportWithoutIO,
    },
    Service,
};
use rustls::pki_types::ServerName;

const HOST: &str = "github.com";
const MAX_POST: usize = 1024 * 1024;
const MAX_ADVERTISEMENT: u64 = 64 * 1024;
const MAX_HEADERS: usize = 8192;
const MAX_CONTROL_LINE: usize = 8192;
const MAX_TRAILER_BYTES: usize = 8192;
const MAX_TRAILER_COUNT: usize = 32;
const BODY_BUFFER: usize = 8192;
const IO_SLICE: Duration = Duration::from_millis(100);
const MAX_HTTP_RESPONSES: usize = 64;

/// Read-only counts for HTTP response framing attempts in one transport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HttpFramingSnapshot {
    pending: usize,
    completed: usize,
    failed: usize,
    abandoned: usize,
    tracking_exhausted: bool,
}

impl HttpFramingSnapshot {
    pub fn pending(&self) -> usize {
        self.pending
    }
    pub fn completed(&self) -> usize {
        self.completed
    }
    pub fn failed(&self) -> usize {
        self.failed
    }
    pub fn abandoned(&self) -> usize {
        self.abandoned
    }

    /// True only when at least one response started and every started response
    /// reached its HTTP framing terminator without failure or abandonment.
    pub fn all_responses_completed(&self) -> bool {
        self.completed > 0
            && self.pending == 0
            && self.failed == 0
            && self.abandoned == 0
            && !self.tracking_exhausted
    }
}

/// Result of explicitly consuming the remaining HTTP response framing.
/// This is transport metadata only; it does not validate Git payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpFinalizationReport {
    framing: HttpFramingSnapshot,
    observed_ingress_bytes: u64,
    drain_error_kind: Option<io::ErrorKind>,
}

impl HttpFinalizationReport {
    pub fn framing(&self) -> HttpFramingSnapshot {
        self.framing
    }
    pub fn observed_ingress_bytes(&self) -> u64 {
        self.observed_ingress_bytes
    }
    pub fn drain_error_kind(&self) -> Option<io::ErrorKind> {
        self.drain_error_kind
    }
    /// True only when a response existed, all framing completed, and the
    /// finalizer observed no cancellation, deadline, decoder, or drain error.
    pub fn is_complete(&self) -> bool {
        self.drain_error_kind.is_none() && self.framing.all_responses_completed()
    }
}

#[derive(Clone, Default)]
struct RetainedBodies(Arc<Mutex<RetainedState>>);

#[derive(Default)]
struct RetainedState {
    bodies: Vec<Body<NetworkBody>>,
    exhausted: bool,
}

impl RetainedBodies {
    fn retain(&self, body: Body<NetworkBody>) {
        if let Ok(mut state) = self.0.lock() {
            if state.bodies.len() < MAX_HTTP_RESPONSES {
                state.bodies.push(body);
            } else {
                state.exhausted = true;
            }
        }
        // On poison or capacity failure, unfinished bodies become Abandoned.
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FramingStatus {
    Pending,
    Completed,
    Failed,
    Abandoned,
}

#[derive(Default)]
struct LedgerState {
    responses: Vec<FramingStatus>,
    tracking_exhausted: bool,
}

#[derive(Clone, Default)]
struct ResponseLedger(Arc<Mutex<LedgerState>>);

impl ResponseLedger {
    fn reserve(&self) -> io::Result<ResponseGuard> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| safe_error(io::ErrorKind::Other, "transport unavailable"))?;
        if state.responses.len() >= MAX_HTTP_RESPONSES {
            state.tracking_exhausted = true;
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "HTTP response attempt limit reached",
            ));
        }
        let id = state.responses.len();
        state.responses.push(FramingStatus::Pending);
        Ok(ResponseGuard {
            ledger: self.clone(),
            id,
            body_owned: false,
            terminal: false,
        })
    }

    fn snapshot(&self) -> io::Result<HttpFramingSnapshot> {
        let state = self
            .0
            .lock()
            .map_err(|_| safe_error(io::ErrorKind::Other, "transport unavailable"))?;
        let mut result = HttpFramingSnapshot::default();
        for status in &state.responses {
            match status {
                FramingStatus::Pending => result.pending += 1,
                FramingStatus::Completed => result.completed += 1,
                FramingStatus::Failed => result.failed += 1,
                FramingStatus::Abandoned => result.abandoned += 1,
            }
        }
        result.tracking_exhausted = state.tracking_exhausted;
        Ok(result)
    }

    fn finish(&self, id: usize, status: FramingStatus) -> io::Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| safe_error(io::ErrorKind::Other, "transport unavailable"))?;
        let entry = state
            .responses
            .get_mut(id)
            .ok_or_else(|| safe_error(io::ErrorKind::Other, "transport unavailable"))?;
        if *entry == FramingStatus::Pending {
            *entry = status;
        }
        Ok(())
    }
}

struct ResponseGuard {
    ledger: ResponseLedger,
    id: usize,
    body_owned: bool,
    terminal: bool,
}

impl ResponseGuard {
    fn into_body(mut self) -> Self {
        self.body_owned = true;
        self
    }
    fn finish(&mut self, status: FramingStatus) -> io::Result<()> {
        if self.terminal {
            return Ok(());
        }
        self.ledger.finish(self.id, status)?;
        self.terminal = true;
        Ok(())
    }
    fn complete(&mut self) -> io::Result<()> {
        self.finish(FramingStatus::Completed)
    }
    fn fail(&mut self) -> io::Result<()> {
        self.finish(FramingStatus::Failed)
    }
}

impl Drop for ResponseGuard {
    fn drop(&mut self) {
        if !self.terminal {
            let status = if self.body_owned {
                FramingStatus::Abandoned
            } else {
                FramingStatus::Failed
            };
            let _ = self.ledger.finish(self.id, status);
            self.terminal = true;
        }
    }
}

/// Per-attempt limits. Values may only lower the documented defaults.
#[derive(Clone, Copy, Debug)]
pub struct AttemptLimits {
    ingress_bytes: u64,
    timeout: Duration,
}

/// Cloneable, one-way cancellation signal for one transport attempt.
#[derive(Clone)]
pub struct CancellationHandle(Arc<AtomicBool>);

impl CancellationHandle {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    /// Cancel the associated attempt. Cancellation cannot be reset.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub(crate) fn check(&self) -> io::Result<()> {
        if self.is_cancelled() {
            Err(safe_error(
                io::ErrorKind::ConnectionAborted,
                "attempt cancelled",
            ))
        } else {
            Ok(())
        }
    }
}

impl Default for AttemptLimits {
    fn default() -> Self {
        Self {
            ingress_bytes: 2 * 1024 * 1024 * 1024,
            timeout: Duration::from_secs(300),
        }
    }
}

impl AttemptLimits {
    pub fn new(ingress_bytes: u64, timeout: Duration) -> io::Result<Self> {
        if ingress_bytes == 0
            || ingress_bytes > AttemptLimits::default().ingress_bytes
            || timeout.is_zero()
            || timeout > AttemptLimits::default().timeout
        {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "invalid attempt limits",
            ));
        }
        Ok(Self {
            ingress_bytes,
            timeout,
        })
    }
}

/// An explicit app-supplied token. It intentionally has no `Debug` or `Clone` implementation.
pub struct AppCredential(String);

impl AppCredential {
    pub fn from_secret(secret: String) -> io::Result<Self> {
        if secret.is_empty() || secret.len() > 4096 || !secret.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "invalid credential",
            ));
        }
        Ok(Self(secret))
    }
}

/// A validated GitHub repository URL. Its accepted path is kept byte-for-byte.
#[derive(Clone, PartialEq, Eq)]
pub struct RepositoryUrl(String);

impl fmt::Debug for RepositoryUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RepositoryUrl([validated GitHub URL])")
    }
}

impl RepositoryUrl {
    pub fn parse(input: &str) -> io::Result<Self> {
        if input.len() > 512 || !input.is_ascii() {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "unsupported repository URL",
            ));
        }
        let prefix = "https://github.com/";
        let path = input
            .strip_prefix(prefix)
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidInput, "unsupported repository URL"))?;
        if path.is_empty()
            || path.contains(['?', '#', '%', '\\', '@', ':'])
            || input.chars().any(char::is_control)
        {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "unsupported repository URL",
            ));
        }
        let (owner, name) = path
            .split_once('/')
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidInput, "unsupported repository URL"))?;
        if name.contains('/')
            || owner.is_empty()
            || name.is_empty()
            || owner.len() > 100
            || name.len() > 100
            || [owner, name].iter().any(|part| {
                *part == "."
                    || *part == ".."
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            })
        {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "unsupported repository URL",
            ));
        }
        let repo = name.strip_suffix(".git").unwrap_or(name);
        if repo.is_empty() || repo.ends_with(".git") || repo == "." || repo == ".." {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "unsupported repository URL",
            ));
        }
        Ok(Self(input.to_owned()))
    }

    fn base(&self) -> &str {
        &self.0
    }
}

/// Construct an unconnected gix HTTP transport for a validated repository.
pub fn connect(
    repository: RepositoryUrl,
    limits: AttemptLimits,
    credential: Option<AppCredential>,
) -> io::Result<GitHubTransport> {
    connect_with_cancellation(repository, limits, credential, CancellationHandle::new())
}

fn connect_with_cancellation(
    repository: RepositoryUrl,
    limits: AttemptLimits,
    credential: Option<AppCredential>,
    cancellation: CancellationHandle,
) -> io::Result<GitHubTransport> {
    let config = Arc::new(client_config()?);
    #[cfg(test)]
    let http = RawHttp::new(repository.clone(), limits, config, cancellation, None);
    #[cfg(not(test))]
    let http = RawHttp::new(repository.clone(), limits, config, cancellation);
    assemble(repository, http, credential)
}

fn assemble(
    repository: RepositoryUrl,
    http: RawHttp,
    credential: Option<AppCredential>,
) -> io::Result<GitHubTransport> {
    let budget = http.budget.clone();
    let framing = http.framing.clone();
    let cancellation = http.cancellation.clone();
    let deadline = http.deadline;
    let retained = http.retained.clone();
    let url = gix_url::parse(repository.base())
        .map_err(|_| safe_error(io::ErrorKind::InvalidInput, "invalid repository URL"))?;
    let mut inner = gix_transport::client::blocking_io::http::Transport::new_http(
        http,
        url,
        gix_transport::Protocol::V2,
        false,
    );
    if let Some(secret) = credential {
        inner
            .set_identity(Account {
                username: "x-access-token".into(),
                password: secret.0,
                oauth_refresh_token: None,
            })
            .map_err(|_| {
                safe_error(
                    io::ErrorKind::PermissionDenied,
                    "credential rejected by transport policy",
                )
            })?;
    }
    Ok(GitHubTransport {
        inner,
        budget,
        framing,
        cancellation,
        deadline,
        retained,
    })
}

/// gix's blocking HTTP transport, ready for an explicit handshake.
pub struct GitHubTransport {
    inner: gix_transport::client::blocking_io::http::Transport<RawHttp>,
    budget: Budget,
    framing: ResponseLedger,
    cancellation: CancellationHandle,
    deadline: Instant,
    retained: RetainedBodies,
}

impl GitHubTransport {
    /// Observe references advertised by Git protocol v2 `ls-refs` and finalize
    /// HTTP framing in the same consuming operation. Returned refs are present
    /// only when both Git protocol parsing and HTTP framing completed. This is
    /// backend metadata only; it does not validate Git objects or imply remote
    /// retention. Prefer this API over the separate low-level stages.
    pub fn observe_refs(self) -> io::Result<RefObservationReport> {
        refs::observe_refs(self)
    }

    /// Download the bounded pack for all freshly advertised tips to a backend
    /// caller's sink. Completion proves transfer framing only, not object
    /// integrity or recoverability. The caller must enforce owned-store space
    /// admission and supervise blocking sink I/O in an outer worker.
    pub fn download_advertised_pack(
        self,
        sink: &mut impl io::Write,
    ) -> io::Result<PackTransferReport> {
        pack::download_advertised_pack(self, sink)
    }

    /// Low-level Git v2 `ls-refs` stage. Call `finalize_http_framing` afterward
    /// and trust the advertised refs only when both stages succeed.
    pub fn read_advertised_refs(&mut self) -> io::Result<Vec<AdvertisedRef>> {
        refs::read_advertised_refs(self)
    }
    /// Return a cloneable, attempt-scoped cancellation handle.
    pub fn cancellation_handle(&self) -> CancellationHandle {
        self.cancellation.clone()
    }
    /// Read-only response-framing counts. A poisoned ledger returns an error.
    pub fn http_framing_snapshot(&self) -> io::Result<HttpFramingSnapshot> {
        self.framing.snapshot()
    }

    /// Socket bytes observed beneath TLS, including incomplete attempts.
    pub fn observed_ingress_bytes(&self) -> io::Result<u64> {
        self.budget
            .used
            .lock()
            .map(|used| *used)
            .map_err(|_| safe_error(io::ErrorKind::Other, "transport unavailable"))
    }

    /// Consume the gix transport and explicitly drain retained HTTP bodies.
    /// This performs bounded network reads through the existing decoder,
    /// ingress budget, deadline, and cancellation signal.
    pub fn finalize_http_framing(self) -> io::Result<HttpFinalizationReport> {
        let Self {
            inner,
            budget,
            framing,
            cancellation,
            deadline,
            retained,
        } = self;
        drop(inner); // release gix readers before taking retained response tails
        let (mut bodies, queue_exhausted) = {
            let mut state = retained
                .0
                .lock()
                .map_err(|_| safe_error(io::ErrorKind::Other, "transport unavailable"))?;
            (std::mem::take(&mut state.bodies), state.exhausted)
        };
        let mut drain_error_kind = None;
        if queue_exhausted {
            drain_error_kind = Some(io::ErrorKind::InvalidData);
        } else if let Err(error) = cancellation.check().and_then(|_| check_deadline(deadline)) {
            drain_error_kind = Some(error.kind());
        }
        if drain_error_kind.is_none() {
            for body in &mut bodies {
                if let Err(error) = io::copy(body, &mut io::sink()) {
                    drain_error_kind = Some(error.kind());
                    break;
                }
            }
        }
        if drain_error_kind.is_none() {
            if let Err(error) = cancellation.check().and_then(|_| check_deadline(deadline)) {
                drain_error_kind = Some(error.kind());
            }
        }
        drop(bodies); // remaining tails become Abandoned, without implicit I/O
        let snapshot = framing.snapshot()?;
        let observed_ingress_bytes = budget
            .used
            .lock()
            .map(|used| *used)
            .map_err(|_| safe_error(io::ErrorKind::Other, "transport unavailable"))?;
        Ok(HttpFinalizationReport {
            framing: snapshot,
            observed_ingress_bytes,
            drain_error_kind,
        })
    }

    pub fn to_url(&self) -> Cow<'_, BStr> {
        self.inner.to_url()
    }
}

impl TransportWithoutIO for GitHubTransport {
    fn set_identity(&mut self, _: Account) -> Result<(), gix_transport::client::Error> {
        Err(gix_transport::client::Error::AuthenticationUnsupported)
    }
    fn to_url(&self) -> Cow<'_, BStr> {
        self.inner.to_url()
    }
    fn connection_persists_across_multiple_requests(&self) -> bool {
        false
    }
    fn configure(&mut self, config: &dyn Any) -> ExnResult {
        self.inner.configure(config)
    }
}

impl gix_transport::client::blocking_io::Transport for GitHubTransport {
    fn handshake<'a>(
        &mut self,
        service: Service,
        extra: &'a [(&'a str, Option<&'a str>)],
    ) -> Result<
        gix_transport::client::blocking_io::SetServiceResponse<'_>,
        gix_transport::client::Error,
    > {
        gix_transport::client::blocking_io::Transport::handshake(&mut self.inner, service, extra)
    }
    fn request(
        &mut self,
        mode: gix_transport::client::WriteMode,
        message: gix_transport::client::MessageKind,
        _trace: bool,
    ) -> Result<gix_transport::client::blocking_io::RequestWriter<'_>, gix_transport::client::Error>
    {
        gix_transport::client::blocking_io::Transport::request(
            &mut self.inner,
            mode,
            message,
            false,
        )
    }
}

fn safe_error(kind: io::ErrorKind, message: &'static str) -> io::Error {
    io::Error::new(kind, message)
}

fn request_io_error(
    cancellation: &CancellationHandle,
    error: io::Error,
    message: &'static str,
) -> io::Error {
    if cancellation.is_cancelled() {
        safe_error(io::ErrorKind::ConnectionAborted, "attempt cancelled")
    } else {
        safe_error(error.kind(), message)
    }
}

fn client_config() -> io::Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| safe_error(io::ErrorKind::Other, "TLS policy unavailable"))?
    .with_root_certificates(roots)
    .with_no_client_auth())
}

#[derive(Clone)]
struct Budget {
    max: u64,
    used: Arc<Mutex<u64>>,
    cancellation: CancellationHandle,
}
impl Budget {
    fn reserve_read(
        &self,
        stream: &mut TcpStream,
        deadline: Instant,
        out: &mut [u8],
    ) -> io::Result<usize> {
        self.reserve_read_with(deadline, out, |slice| {
            read_until(stream, deadline, &self.cancellation, slice)
        })
    }

    fn reserve_read_with(
        &self,
        deadline: Instant,
        out: &mut [u8],
        read: impl FnOnce(&mut [u8]) -> io::Result<usize>,
    ) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let mut used = self
            .used
            .lock()
            .map_err(|_| safe_error(io::ErrorKind::Other, "transport unavailable"))?;
        let left = self.max.saturating_sub(*used);
        if left == 0 {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "ingress limit reached",
            ));
        }
        let n = out.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        self.cancellation.check()?;
        check_deadline(deadline)?;
        let count = read(&mut out[..n])?;
        if count > n {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "invalid ingress read count",
            ));
        }
        *used = used.saturating_add(count as u64);
        self.cancellation.check()?;
        check_deadline(deadline)?;
        Ok(count)
    }
}

struct CountedSocket {
    stream: TcpStream,
    budget: Budget,
    deadline: Instant,
    cancellation: CancellationHandle,
}
fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(safe_error(
            io::ErrorKind::TimedOut,
            "attempt deadline exceeded",
        ))
    } else {
        Ok(())
    }
}
fn read_until(
    stream: &mut TcpStream,
    deadline: Instant,
    cancellation: &CancellationHandle,
    out: &mut [u8],
) -> io::Result<usize> {
    loop {
        cancellation.check()?;
        check_deadline(deadline)?;
        stream.set_read_timeout(Some(
            deadline
                .saturating_duration_since(Instant::now())
                .min(IO_SLICE),
        ))?;
        match stream.read(out) {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) && Instant::now() < deadline =>
            {
                continue
            }
            Ok(count) => return Ok(count),
            Err(error) => {
                cancellation.check()?;
                check_deadline(deadline)?;
                return Err(error);
            }
        }
    }
}
fn write_until(
    stream: &mut TcpStream,
    deadline: Instant,
    cancellation: &CancellationHandle,
    mut data: &[u8],
) -> io::Result<()> {
    while !data.is_empty() {
        cancellation.check()?;
        check_deadline(deadline)?;
        stream.set_write_timeout(Some(
            deadline
                .saturating_duration_since(Instant::now())
                .min(IO_SLICE),
        ))?;
        match stream.write(data) {
            Ok(0) => {
                return Err(safe_error(
                    io::ErrorKind::WriteZero,
                    "transport write failed",
                ))
            }
            Ok(n) => {
                cancellation.check()?;
                data = &data[n..]
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) && Instant::now() < deadline =>
            {
                continue
            }
            Err(error) => {
                cancellation.check()?;
                check_deadline(deadline)?;
                return Err(safe_error(error.kind(), "transport write failed"));
            }
        }
    }
    Ok(())
}
impl Read for CountedSocket {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.budget.reserve_read(&mut self.stream, self.deadline, b)
    }
}
impl Write for CountedSocket {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        write_until(&mut self.stream, self.deadline, &self.cancellation, b)?;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        check_deadline(self.deadline)?;
        self.cancellation.check()?;
        self.stream
            .flush()
            .map_err(|_| safe_error(io::ErrorKind::Other, "transport write failed"))?;
        self.cancellation.check()
    }
}

type NetworkBody = rustls::StreamOwned<rustls::ClientConnection, CountedSocket>;

struct Body<R> {
    stream: R,
    deadline: Instant,
    cancellation: CancellationHandle,
    framing: BodyFraming,
    decoded_remaining: Option<u64>,
    payload: [u8; BODY_BUFFER],
    payload_start: usize,
    payload_end: usize,
    failed: Option<io::ErrorKind>,
    evidence: Option<ResponseGuard>,
}
#[derive(Clone, Copy)]
enum BodyFraming {
    Fixed(usize),
    Chunked(ChunkState),
}
#[derive(Clone, Copy)]
enum ChunkState {
    Size,
    Data(u64),
    DataCrlf,
    Trailers { bytes: usize, count: usize },
    Done,
}
impl<R: Read> BufRead for Body<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if let Some(kind) = self.failed {
            return Err(body_error(kind));
        }
        if let Err(error) = self.cancellation.check() {
            return Err(self.fail(error));
        }
        if let Err(error) = check_deadline(self.deadline) {
            return Err(self.fail(error));
        }
        if self.payload_start < self.payload_end {
            return Ok(&self.payload[self.payload_start..self.payload_end]);
        }
        match self.advance() {
            Ok(true) => Ok(&self.payload[self.payload_start..self.payload_end]),
            Ok(false) => match self.complete() {
                Ok(()) => Ok(&[]),
                Err(error) => Err(self.fail(error)),
            },
            Err(error) => Err(self.fail(error)),
        }
    }
    fn consume(&mut self, n: usize) {
        self.payload_start = self.payload_start.saturating_add(n).min(self.payload_end);
    }
}
impl<R: Read> Body<R> {
    fn fail(&mut self, error: io::Error) -> io::Error {
        self.failed = Some(error.kind());
        if let Some(evidence) = &mut self.evidence {
            if let Err(ledger_error) = evidence.fail() {
                self.failed = Some(ledger_error.kind());
                return safe_error(ledger_error.kind(), "transport unavailable");
            }
        }
        error
    }

    fn complete(&mut self) -> io::Result<()> {
        if let Some(evidence) = &mut self.evidence {
            evidence.complete()?;
        }
        Ok(())
    }

    fn read_byte(&mut self) -> io::Result<u8> {
        self.cancellation.check()?;
        check_deadline(self.deadline)?;
        let mut byte = [0];
        let count = self
            .stream
            .read(&mut byte)
            .map_err(|error| safe_error(error.kind(), "response read failed"))?;
        check_deadline(self.deadline)?;
        self.cancellation.check()?;
        if count == 0 {
            return Err(safe_error(
                io::ErrorKind::UnexpectedEof,
                "response truncated",
            ));
        }
        Ok(byte[0])
    }

    fn read_line(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut line = Vec::new();
        loop {
            let byte = self.read_byte()?;
            if byte == b'\r' {
                if self.read_byte()? != b'\n' {
                    return Err(safe_error(
                        io::ErrorKind::InvalidData,
                        "invalid chunk line ending",
                    ));
                }
                return Ok(line);
            }
            if byte == b'\n' || line.len() >= limit {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "chunk control line exceeds limit",
                ));
            }
            if !(byte == b'\t' || (0x20..=0x7e).contains(&byte)) {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "invalid chunk control byte",
                ));
            }
            line.push(byte);
        }
    }

    fn advance(&mut self) -> io::Result<bool> {
        self.cancellation.check()?;
        check_deadline(self.deadline)?;
        loop {
            match self.framing {
                BodyFraming::Fixed(left) => {
                    if left == 0 {
                        return Ok(false);
                    }
                    let count = self.read_payload(left.min(BODY_BUFFER))?;
                    self.framing = BodyFraming::Fixed(left - count);
                    return Ok(true);
                }
                BodyFraming::Chunked(ChunkState::Size) => {
                    let line = self.read_line(MAX_CONTROL_LINE)?;
                    let size = parse_chunk_size(&line)?;
                    self.framing = BodyFraming::Chunked(if size == 0 {
                        ChunkState::Trailers { bytes: 0, count: 0 }
                    } else {
                        ChunkState::Data(size)
                    });
                }
                BodyFraming::Chunked(ChunkState::Data(left)) => {
                    if left == 0 {
                        self.framing = BodyFraming::Chunked(ChunkState::DataCrlf);
                        continue;
                    }
                    let count = self.read_payload(
                        usize::try_from(left.min(BODY_BUFFER as u64)).unwrap_or(BODY_BUFFER),
                    )?;
                    self.framing = BodyFraming::Chunked(ChunkState::Data(left - count as u64));
                    return Ok(true);
                }
                BodyFraming::Chunked(ChunkState::DataCrlf) => {
                    if self.read_byte()? != b'\r' || self.read_byte()? != b'\n' {
                        return Err(safe_error(
                            io::ErrorKind::InvalidData,
                            "invalid chunk data delimiter",
                        ));
                    }
                    self.framing = BodyFraming::Chunked(ChunkState::Size);
                }
                BodyFraming::Chunked(ChunkState::Trailers { bytes, count }) => {
                    let line = self.read_line(MAX_CONTROL_LINE)?;
                    let total = bytes.checked_add(line.len() + 2).ok_or_else(|| {
                        safe_error(io::ErrorKind::InvalidData, "trailers exceed limit")
                    })?;
                    if total > MAX_TRAILER_BYTES {
                        return Err(safe_error(
                            io::ErrorKind::InvalidData,
                            "trailers exceed limit",
                        ));
                    }
                    if line.is_empty() {
                        self.framing = BodyFraming::Chunked(ChunkState::Done);
                        continue;
                    }
                    let count = count + 1;
                    if count > MAX_TRAILER_COUNT {
                        return Err(safe_error(io::ErrorKind::InvalidData, "too many trailers"));
                    }
                    validate_trailer(&line)?;
                    self.framing = BodyFraming::Chunked(ChunkState::Trailers {
                        bytes: total,
                        count,
                    });
                }
                BodyFraming::Chunked(ChunkState::Done) => return Ok(false),
            }
        }
    }

    fn read_payload(&mut self, limit: usize) -> io::Result<usize> {
        self.cancellation.check()?;
        check_deadline(self.deadline)?;
        let limit = match self.decoded_remaining {
            Some(0) => {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "advertisement exceeds limit",
                ))
            }
            Some(left) => limit.min(usize::try_from(left).unwrap_or(usize::MAX)),
            None => limit,
        };
        let count = self
            .stream
            .read(&mut self.payload[..limit])
            .map_err(|error| safe_error(error.kind(), "response read failed"))?;
        if let Some(left) = &mut self.decoded_remaining {
            *left -= count as u64;
        }
        check_deadline(self.deadline)?;
        self.cancellation.check()?;
        if count == 0 {
            return Err(safe_error(
                io::ErrorKind::UnexpectedEof,
                "response truncated",
            ));
        }
        self.payload_start = 0;
        self.payload_end = count;
        Ok(count)
    }
}
fn body_error(kind: io::ErrorKind) -> io::Error {
    safe_error(kind, "response body failed")
}
impl<R: Read> Read for Body<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let b = self.fill_buf()?;
        if b.is_empty() {
            return Ok(0);
        }
        let count = out.len().min(b.len());
        out[..count].copy_from_slice(&b[..count]);
        self.consume(count);
        Ok(count)
    }
}

struct RawHttp {
    repository: RepositoryUrl,
    limits: AttemptLimits,
    deadline: Instant,
    budget: Budget,
    framing: ResponseLedger,
    retained: RetainedBodies,
    cancellation: CancellationHandle,
    config: Arc<rustls::ClientConfig>,
    #[cfg(test)]
    fixture: Option<(SocketAddr, Arc<str>)>,
}
impl fmt::Debug for RawHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RawHttp")
    }
}
impl RawHttp {
    fn new(
        repository: RepositoryUrl,
        limits: AttemptLimits,
        config: Arc<rustls::ClientConfig>,
        cancellation: CancellationHandle,
        #[cfg(test)] fixture: Option<(SocketAddr, Arc<str>)>,
    ) -> Self {
        Self {
            repository,
            limits,
            deadline: Instant::now() + limits.timeout,
            budget: Budget {
                max: limits.ingress_bytes,
                used: Arc::new(Mutex::new(0)),
                cancellation: cancellation.clone(),
            },
            framing: ResponseLedger::default(),
            retained: RetainedBodies::default(),
            cancellation,
            config,
            #[cfg(test)]
            fixture,
        }
    }
    #[cfg(test)]
    fn for_test(
        repository: RepositoryUrl,
        limits: AttemptLimits,
        config: Arc<rustls::ClientConfig>,
        endpoint: SocketAddr,
        server_name: &str,
    ) -> io::Result<Self> {
        if !endpoint.ip().is_loopback() || server_name.is_empty() || !server_name.is_ascii() {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "invalid test endpoint",
            ));
        }
        Ok(Self::new(
            repository,
            limits,
            config,
            CancellationHandle::new(),
            Some((endpoint, Arc::from(server_name))),
        ))
    }
    fn connect_socket(&self) -> io::Result<CountedSocket> {
        self.cancellation.check()?;
        check_deadline(self.deadline)?;
        #[cfg(test)]
        let endpoint = match self.fixture {
            Some((addr, _)) if addr.ip().is_loopback() => vec![addr],
            _ => {
                return Err(safe_error(
                    io::ErrorKind::PermissionDenied,
                    "network disabled in test mode",
                ))
            }
        };
        #[cfg(not(test))]
        let endpoint: Vec<SocketAddr> = (HOST, 443)
            .to_socket_addrs()
            .map_err(|_| safe_error(io::ErrorKind::Other, "name resolution failed"))?
            .collect();
        check_deadline(self.deadline)?;
        self.cancellation.check()?;
        let mut last = None;
        for addr in endpoint {
            self.cancellation.check()?;
            check_deadline(self.deadline)?;
            match TcpStream::connect_timeout(
                &addr,
                self.deadline.saturating_duration_since(Instant::now()),
            ) {
                Ok(stream) => {
                    self.cancellation.check()?;
                    return Ok(CountedSocket {
                        stream,
                        budget: self.budget.clone(),
                        deadline: self.deadline,
                        cancellation: self.cancellation.clone(),
                    });
                }
                Err(_) => {
                    self.cancellation.check()?;
                    last = Some(());
                }
            }
        }
        let _ = last;
        Err(safe_error(io::ErrorKind::Other, "connection failed"))
    }
    fn request(
        &mut self,
        method: &str,
        path: &str,
        headers: Vec<String>,
        body: Option<Vec<u8>>,
        evidence: ResponseGuard,
    ) -> io::Result<(Cursor<Vec<u8>>, Body<NetworkBody>)> {
        if path != "/info/refs?service=git-upload-pack" && path != "/git-upload-pack" {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "request path rejected",
            ));
        }
        let repository_path = self
            .repository
            .base()
            .strip_prefix("https://github.com/")
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidInput, "repository path rejected"))?;
        let wire_path = format!("/{repository_path}{path}");
        validate_request_headers(method, &headers)?;
        let socket = self.connect_socket()?;
        #[cfg(test)]
        let server_name = self
            .fixture
            .as_ref()
            .map(|(_, n)| n.as_ref())
            .unwrap_or(HOST);
        #[cfg(not(test))]
        let server_name = HOST;
        let name = ServerName::try_from(server_name.to_owned())
            .map_err(|_| safe_error(io::ErrorKind::InvalidInput, "TLS server name rejected"))?;
        let conn = rustls::ClientConnection::new(Arc::clone(&self.config), name)
            .map_err(|_| safe_error(io::ErrorKind::Other, "TLS setup failed"))?;
        let mut tls = rustls::StreamOwned::new(conn, socket);
        let mut req = format!("{method} {wire_path} HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\nAccept-Encoding: identity\r\n");
        let mut auth_seen = false;
        for h in headers {
            if h.contains(['\r', '\n', '\0']) {
                return Err(safe_error(
                    io::ErrorKind::InvalidInput,
                    "request header rejected",
                ));
            }
            let (name, _) = h.split_once(':').ok_or_else(|| {
                safe_error(io::ErrorKind::InvalidInput, "request header rejected")
            })?;
            match name.trim().to_ascii_lowercase().as_str() {
                "host" | "content-length" | "transfer-encoding" | "connection"
                | "accept-encoding" => {
                    return Err(safe_error(
                        io::ErrorKind::InvalidInput,
                        "request header rejected",
                    ))
                }
                "authorization" if auth_seen => {
                    return Err(safe_error(
                        io::ErrorKind::InvalidInput,
                        "request header rejected",
                    ))
                }
                "authorization" => auth_seen = true,
                _ => {}
            }
            req.push_str(&h);
            req.push_str("\r\n");
        }
        if let Some(ref b) = body {
            req.push_str(&format!("Content-Length: {}\r\n", b.len()));
        }
        req.push_str("\r\n");
        tls.write_all(req.as_bytes())
            .map_err(|error| request_io_error(&self.cancellation, error, "request write failed"))?;
        if let Some(b) = body {
            tls.write_all(&b).map_err(|error| {
                request_io_error(&self.cancellation, error, "request write failed")
            })?;
            tls.flush().map_err(|error| {
                request_io_error(&self.cancellation, error, "request write failed")
            })?;
        }
        tls.flush()
            .map_err(|error| request_io_error(&self.cancellation, error, "request write failed"))?;
        let mut raw = Vec::new();
        let mut byte = [0];
        while !raw.ends_with(b"\r\n\r\n") {
            if raw.len() >= MAX_HEADERS {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "response headers exceed limit",
                ));
            }
            tls.read_exact(&mut byte).map_err(|error| {
                request_io_error(&self.cancellation, error, "response headers unavailable")
            })?;
            raw.push(byte[0]);
        }
        let parsed = parse_headers(&raw)?;
        if matches!(parsed.status, 401 | 403) {
            return Err(safe_error(
                io::ErrorKind::PermissionDenied,
                "HTTP access denied",
            ));
        }
        if parsed.status != 200 {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "HTTP response rejected",
            ));
        }
        let length = parsed.length.unwrap_or(0);
        if method == "GET" && (length as u64) > MAX_ADVERTISEMENT {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "advertisement exceeds limit",
            ));
        }
        let content_type = parsed
            .content_type
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "missing response type"))?;
        let h = if parsed.chunked {
            format!("Content-Type: {}\r\n\r\n", content_type).into_bytes()
        } else {
            format!(
                "Content-Type: {}\r\nContent-Length: {}\r\n\r\n",
                content_type, length
            )
            .into_bytes()
        };
        Ok((
            Cursor::new(h),
            Body {
                stream: tls,
                deadline: self.deadline,
                cancellation: self.cancellation.clone(),
                decoded_remaining: (method == "GET").then_some(MAX_ADVERTISEMENT),
                framing: if parsed.chunked {
                    BodyFraming::Chunked(ChunkState::Size)
                } else {
                    BodyFraming::Fixed(length)
                },
                payload: [0; BODY_BUFFER],
                payload_start: 0,
                payload_end: 0,
                failed: None,
                evidence: Some(evidence.into_body()),
            },
        ))
    }
    fn validate(&self, url: &str, base: &str, post: bool) -> io::Result<()> {
        if base != self.repository.base() {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "request base rejected",
            ));
        }
        let tail = if post {
            "/git-upload-pack"
        } else {
            "/info/refs?service=git-upload-pack"
        };
        if url != format!("{}{tail}", self.repository.base()) {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "request URL rejected",
            ));
        }
        Ok(())
    }
}

struct LazyPost {
    sender: mpsc::SyncSender<Option<Vec<u8>>>,
    data: Vec<u8>,
    failed: bool,
    cancellation: CancellationHandle,
}
impl Write for LazyPost {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if let Err(error) = self.cancellation.check() {
            self.failed = true;
            return Err(error);
        }
        if self.failed || self.data.len().saturating_add(b.len()) > MAX_POST {
            self.failed = true;
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "POST body limit exceeded",
            ));
        }
        self.data.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if let Err(error) = self.cancellation.check() {
            self.failed = true;
            return Err(error);
        }
        if self.failed {
            Err(safe_error(io::ErrorKind::InvalidData, "POST body failed"))
        } else {
            Ok(())
        }
    }
}
impl Drop for LazyPost {
    fn drop(&mut self) {
        let _ = self.sender.try_send(if self.failed {
            None
        } else {
            Some(std::mem::take(&mut self.data))
        });
    }
}

// The response wrappers defer the POST until gix has dropped its writer.
struct Headers {
    cursor: Cursor<Vec<u8>>,
    pending: Option<Arc<Mutex<PostState>>>,
}
struct Response {
    body: Option<Body<NetworkBody>>,
    pending: Option<Arc<Mutex<PostState>>>,
    retained: RetainedBodies,
}
impl Drop for Response {
    fn drop(&mut self) {
        if let Some(body) = self.body.take() {
            self.retained.retain(body);
        }
    }
}
struct PostState {
    receiver: mpsc::Receiver<Option<Vec<u8>>>,
    http: RawHttp,
    request_headers: Vec<String>,
    headers: Option<Cursor<Vec<u8>>>,
    body: Option<Body<NetworkBody>>,
    failed: Option<io::ErrorKind>,
    evidence: Option<ResponseGuard>,
    retained: RetainedBodies,
}
impl PostState {
    fn fail_response(&mut self, kind: io::ErrorKind) -> io::Error {
        if let Some(first) = self.failed {
            return safe_error(first, "POST failed");
        }
        self.failed = Some(kind);
        if let Some(mut evidence) = self.evidence.take() {
            if let Err(error) = evidence.fail() {
                return safe_error(error.kind(), "transport unavailable");
            }
        }
        safe_error(kind, "POST failed")
    }

    fn init(&mut self) -> io::Result<()> {
        if let Some(kind) = self.failed {
            return Err(safe_error(kind, "POST failed"));
        }
        self.http
            .cancellation
            .check()
            .map_err(|e| self.fail_response(e.kind()))?;
        if self.headers.is_some() {
            return Ok(());
        }
        let data = loop {
            if let Err(error) = self.http.cancellation.check() {
                return Err(self.fail_response(error.kind()));
            }
            let left = self.http.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(self.fail_response(io::ErrorKind::TimedOut));
            }
            match self.receiver.recv_timeout(left.min(IO_SLICE)) {
                Ok(Some(data)) => break data,
                Ok(None) => return Err(self.fail_response(io::ErrorKind::InvalidData)),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.fail_response(io::ErrorKind::InvalidData))
                }
            }
        };
        if let Err(error) = self.http.cancellation.check() {
            return Err(self.fail_response(error.kind()));
        }
        let evidence = self
            .evidence
            .take()
            .ok_or_else(|| safe_error(io::ErrorKind::Other, "transport unavailable"))?;
        let result = self.http.request(
            "POST",
            "/git-upload-pack",
            std::mem::take(&mut self.request_headers),
            Some(data),
            evidence,
        );
        let (h, b) = match result {
            Ok(result) => result,
            Err(error) => {
                let kind = error.kind();
                self.failed = Some(kind);
                return Err(safe_error(kind, "POST failed"));
            }
        };
        self.headers = Some(h);
        self.body = Some(b);
        Ok(())
    }
}
impl Drop for PostState {
    fn drop(&mut self) {
        if let Some(body) = self.body.take() {
            self.retained.retain(body);
        } else if let Some(mut evidence) = self.evidence.take() {
            let _ = evidence.fail();
        }
    }
}
impl BufRead for Headers {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if let Some(p) = self.pending.clone() {
            let mut s = p
                .lock()
                .map_err(|_| safe_error(io::ErrorKind::Other, "transport unavailable"))?;
            s.init()?;
            self.cursor = s
                .headers
                .as_ref()
                .ok_or_else(|| safe_error(io::ErrorKind::Other, "response unavailable"))?
                .clone();
            self.pending = None;
        }
        self.cursor
            .fill_buf()
            .map_err(|_| safe_error(io::ErrorKind::Other, "response unavailable"))
    }
    fn consume(&mut self, n: usize) {
        if let Some(p) = &self.pending {
            if let Ok(mut s) = p.lock() {
                if let Some(h) = &mut s.headers {
                    h.consume(n)
                }
            }
        } else {
            self.cursor.consume(n)
        }
    }
}
impl Read for Headers {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let x = self.fill_buf()?;
        let n = x.len().min(b.len());
        b[..n].copy_from_slice(&x[..n]);
        self.consume(n);
        Ok(n)
    }
}
impl BufRead for Response {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if let Some(p) = self.pending.clone() {
            let mut s = p
                .lock()
                .map_err(|_| safe_error(io::ErrorKind::Other, "transport unavailable"))?;
            s.init()?;
            self.body = s.body.take();
            self.pending = None;
        }
        self.body
            .as_mut()
            .ok_or_else(|| safe_error(io::ErrorKind::Other, "response unavailable"))?
            .fill_buf()
    }
    fn consume(&mut self, n: usize) {
        if let Some(b) = &mut self.body {
            b.consume(n)
        }
    }
}
impl Read for Response {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let x = self.fill_buf()?;
        let n = x.len().min(b.len());
        b[..n].copy_from_slice(&x[..n]);
        self.consume(n);
        Ok(n)
    }
}

impl Http for RawHttp {
    type Headers = Headers;
    type ResponseBody = Response;
    type PostBody = LazyPost;
    fn get(
        &mut self,
        url: &str,
        base: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> ExnMessageResult<GetResponse<Self::Headers, Self::ResponseBody>> {
        let result: io::Result<_> = (|| {
            self.validate(url, base, false)?;
            let hs = collect_headers(headers)?;
            validate_request_headers("GET", &hs)?;
            let evidence = self.framing.reserve()?;
            let (h, b) = self.request(
                "GET",
                "/info/refs?service=git-upload-pack",
                hs,
                None,
                evidence,
            )?;
            Ok(GetResponse {
                headers: Headers {
                    cursor: h,
                    pending: None,
                },
                body: Response {
                    body: Some(b),
                    pending: None,
                    retained: self.retained.clone(),
                },
            })
        })();
        result.map_err(|error| {
            let kind = error.kind();
            io::Error::new(kind, "GitHub GET failed")
                .and_raise(gix_error::message("GitHub GET failed"))
        })
    }
    fn post(
        &mut self,
        url: &str,
        base: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        _kind: PostBodyDataKind,
    ) -> ExnMessageResult<PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>> {
        let result: io::Result<_> = (|| {
            self.validate(url, base, true)?;
            let hs = collect_headers(headers)?;
            validate_request_headers("POST", &hs)?;
            let evidence = self.framing.reserve()?;
            let (tx, rx) = mpsc::sync_channel(1);
            let state = Arc::new(Mutex::new(PostState {
                receiver: rx,
                http: RawHttp {
                    repository: self.repository.clone(),
                    limits: self.limits,
                    deadline: self.deadline,
                    budget: self.budget.clone(),
                    framing: self.framing.clone(),
                    retained: self.retained.clone(),
                    cancellation: self.cancellation.clone(),
                    config: Arc::clone(&self.config),
                    #[cfg(test)]
                    fixture: self.fixture.clone(),
                },
                request_headers: hs,
                evidence: Some(evidence),
                headers: None,
                body: None,
                failed: None,
                retained: self.retained.clone(),
            }));
            Ok(PostResponse {
                post_body: LazyPost {
                    sender: tx,
                    data: Vec::new(),
                    failed: false,
                    cancellation: self.cancellation.clone(),
                },
                headers: Headers {
                    cursor: Cursor::new(Vec::new()),
                    pending: Some(Arc::clone(&state)),
                },
                body: Response {
                    body: None,
                    pending: Some(state),
                    retained: self.retained.clone(),
                },
            })
        })();
        result.map_err(|error| {
            let kind = error.kind();
            io::Error::new(kind, "GitHub POST failed")
                .and_raise(gix_error::message("GitHub POST failed"))
        })
    }
    fn configure(&mut self, _: &dyn Any) -> ExnResult {
        Err(gix_error::message("unsupported transport configuration").raise_erased())
    }
}

struct Parsed {
    status: u16,
    length: Option<usize>,
    content_type: Option<String>,
    chunked: bool,
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn parse_chunk_size(line: &[u8]) -> io::Result<u64> {
    let mut cursor = 0;
    let mut size = 0u64;
    while cursor < line.len() && line[cursor].is_ascii_hexdigit() {
        let digit = match line[cursor] {
            b'0'..=b'9' => u64::from(line[cursor] - b'0'),
            b'a'..=b'f' => u64::from(line[cursor] - b'a' + 10),
            b'A'..=b'F' => u64::from(line[cursor] - b'A' + 10),
            _ => unreachable!(),
        };
        size = size
            .checked_mul(16)
            .and_then(|n| n.checked_add(digit))
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "chunk size overflow"))?;
        cursor += 1;
    }
    if cursor == 0 {
        return Err(safe_error(io::ErrorKind::InvalidData, "invalid chunk size"));
    }

    while cursor < line.len() {
        while matches!(line.get(cursor), Some(b' ' | b'\t')) {
            cursor += 1;
        }
        if cursor == line.len() {
            break;
        }
        if line[cursor] != b';' {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "invalid chunk extension",
            ));
        }
        cursor += 1;
        while matches!(line.get(cursor), Some(b' ' | b'\t')) {
            cursor += 1;
        }
        let name_start = cursor;
        while line.get(cursor).is_some_and(|b| is_token_byte(*b)) {
            cursor += 1;
        }
        if cursor == name_start {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "invalid chunk extension",
            ));
        }
        while matches!(line.get(cursor), Some(b' ' | b'\t')) {
            cursor += 1;
        }
        if line.get(cursor) == Some(&b'=') {
            cursor += 1;
            while matches!(line.get(cursor), Some(b' ' | b'\t')) {
                cursor += 1;
            }
            if line.get(cursor) == Some(&b'"') {
                cursor += 1;
                let mut closed = false;
                while let Some(&byte) = line.get(cursor) {
                    match byte {
                        b'"' => {
                            cursor += 1;
                            closed = true;
                            break;
                        }
                        b'\\' => {
                            cursor += 1;
                            if !line
                                .get(cursor)
                                .is_some_and(|b| matches!(*b, b'\t' | b' ' | 0x21..=0x7e))
                            {
                                return Err(safe_error(
                                    io::ErrorKind::InvalidData,
                                    "invalid quoted chunk extension",
                                ));
                            }
                            cursor += 1;
                        }
                        b'\t' | b' ' | b'!' | b'#'..=b'[' | b']'..=b'~' => cursor += 1,
                        _ => {
                            return Err(safe_error(
                                io::ErrorKind::InvalidData,
                                "invalid quoted chunk extension",
                            ))
                        }
                    }
                }
                if !closed {
                    return Err(safe_error(
                        io::ErrorKind::InvalidData,
                        "unterminated chunk extension",
                    ));
                }
            } else {
                let value_start = cursor;
                while line.get(cursor).is_some_and(|b| is_token_byte(*b)) {
                    cursor += 1;
                }
                if cursor == value_start {
                    return Err(safe_error(
                        io::ErrorKind::InvalidData,
                        "invalid chunk extension value",
                    ));
                }
            }
            while matches!(line.get(cursor), Some(b' ' | b'\t')) {
                cursor += 1;
            }
        }
        if cursor < line.len() && line[cursor] != b';' {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "invalid chunk extension",
            ));
        }
    }
    Ok(size)
}

fn validate_trailer(line: &[u8]) -> io::Result<()> {
    let colon = line
        .iter()
        .position(|b| *b == b':')
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "invalid trailer field"))?;
    let (name, value_with_colon) = line.split_at(colon);
    let value = &value_with_colon[1..];
    if name.is_empty()
        || !name.iter().copied().all(is_token_byte)
        || !value
            .iter()
            .all(|b| *b == b'\t' || (0x20..=0x7e).contains(b))
    {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "invalid trailer field",
        ));
    }
    let lower = name.iter().map(u8::to_ascii_lowercase).collect::<Vec<_>>();
    const FORBIDDEN: &[&[u8]] = &[
        b"authorization",
        b"proxy-authorization",
        b"www-authenticate",
        b"proxy-authenticate",
        b"cookie",
        b"set-cookie",
        b"content-length",
        b"transfer-encoding",
        b"trailer",
        b"host",
        b"destination",
        b"content-location",
        b"connection",
        b"keep-alive",
        b"proxy-connection",
        b"te",
        b"upgrade",
        b"content-encoding",
        b"content-type",
        b"content-range",
        b"location",
        b"via",
        b"expect",
        b"max-forwards",
        b"range",
        b"origin",
        b"referer",
    ];
    if FORBIDDEN.contains(&lower.as_slice()) {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "forbidden trailer field",
        ));
    }
    Ok(())
}
fn validate_request_headers(method: &str, headers: &[String]) -> io::Result<()> {
    let mut seen_auth = false;
    let mut seen_type = false;
    let mut seen_accept = false;
    let mut seen_protocol = false;
    for header in headers {
        if header.len() > 4096
            || !header.is_ascii()
            || header.bytes().any(|b| b.is_ascii_control() && b != b'\t')
        {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "request header rejected",
            ));
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidInput, "request header rejected"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "request header rejected",
            ));
        }
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "host" | "content-length" | "transfer-encoding" | "connection" | "accept-encoding" => {
                return Err(safe_error(
                    io::ErrorKind::InvalidInput,
                    "request header rejected",
                ))
            }
            "authorization" => {
                if seen_auth
                    || !value.starts_with("Basic ")
                    || !value[6..]
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
                {
                    return Err(safe_error(
                        io::ErrorKind::InvalidInput,
                        "request header rejected",
                    ));
                }
                seen_auth = true;
            }
            "content-type" => {
                if method != "POST" || seen_type || value != "application/x-git-upload-pack-request"
                {
                    return Err(safe_error(
                        io::ErrorKind::InvalidInput,
                        "request header rejected",
                    ));
                }
                seen_type = true;
            }
            "accept" => {
                if method != "POST"
                    || seen_accept
                    || value != "application/x-git-upload-pack-result"
                {
                    return Err(safe_error(
                        io::ErrorKind::InvalidInput,
                        "request header rejected",
                    ));
                }
                seen_accept = true;
            }
            "git-protocol" => {
                if seen_protocol {
                    return Err(safe_error(
                        io::ErrorKind::InvalidInput,
                        "request header rejected",
                    ));
                }
                seen_protocol = true;
            }
            "user-agent" => {}
            _ => {
                return Err(safe_error(
                    io::ErrorKind::InvalidInput,
                    "request header rejected",
                ))
            }
        }
    }
    if method == "POST" && (!seen_type || !seen_accept) {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "request headers incomplete",
        ));
    }
    Ok(())
}

fn collect_headers(headers: impl IntoIterator<Item = impl AsRef<str>>) -> io::Result<Vec<String>> {
    let mut result = Vec::new();
    let mut bytes = 0usize;
    for item in headers {
        let value = item.as_ref();
        if value.len() > 4096 || result.len() >= 32 {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "request headers exceed limit",
            ));
        }
        bytes = bytes.checked_add(value.len()).ok_or_else(|| {
            safe_error(io::ErrorKind::InvalidInput, "request headers exceed limit")
        })?;
        if bytes > 16 * 1024 {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "request headers exceed limit",
            ));
        }
        result.push(value.to_owned());
    }
    Ok(result)
}

fn parse_headers(raw: &[u8]) -> io::Result<Parsed> {
    if !raw.is_ascii() {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "invalid response headers",
        ));
    }
    let text = std::str::from_utf8(raw)
        .map_err(|_| safe_error(io::ErrorKind::InvalidData, "invalid response headers"))?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    if !status_line.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "invalid response status",
        ));
    }
    let (version, rest) = status_line
        .split_once(' ')
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "invalid response status"))?;
    let code_text = rest
        .get(..3)
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "invalid response status"))?;
    if version != "HTTP/1.1"
        || !code_text.bytes().all(|b| b.is_ascii_digit())
        || (rest.len() > 3 && !rest.as_bytes()[3].is_ascii_whitespace())
    {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "invalid response status",
        ));
    }
    let code = code_text
        .parse::<u16>()
        .map_err(|_| safe_error(io::ErrorKind::InvalidData, "invalid response status"))?;
    let (mut length, mut ctype, mut transfer_encoding) = (None, None, None);
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t'])
            || !line
                .bytes()
                .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
        {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "invalid response header",
            ));
        }
        let (k, v) = line
            .split_once(':')
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "invalid response header"))?;
        if k.is_empty()
            || !k
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "invalid response header",
            ));
        }
        let v = v.trim();
        match k.to_ascii_lowercase().as_str() {
            "content-length" if length.is_none() => {
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(safe_error(
                        io::ErrorKind::InvalidData,
                        "invalid response length",
                    ));
                }
                length = Some(v.parse::<usize>().map_err(|_| {
                    safe_error(io::ErrorKind::InvalidData, "invalid response length")
                })?)
            }
            "content-length" => {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "ambiguous response length",
                ))
            }
            "content-type" if ctype.is_none() => ctype = Some(v.to_owned()),
            "content-type" => {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "ambiguous response type",
                ))
            }
            "transfer-encoding" if transfer_encoding.is_none() => {
                if !v.eq_ignore_ascii_case("chunked") {
                    return Err(safe_error(
                        io::ErrorKind::InvalidData,
                        "unsupported transfer coding",
                    ));
                }
                transfer_encoding = Some(());
            }
            "transfer-encoding" => {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "ambiguous transfer coding",
                ))
            }
            "content-encoding" | "location" => {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "response framing rejected",
                ))
            }
            _ => {}
        }
    }
    if length.is_some() && transfer_encoding.is_some() {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "conflicting response framing",
        ));
    }
    if (200..300).contains(&code)
        && (ctype.is_none() || (length.is_none() == transfer_encoding.is_none()))
    {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "incomplete response headers",
        ));
    }
    Ok(Parsed {
        status: code,
        length,
        content_type: ctype,
        chunked: transfer_encoding.is_some(),
    })
}

#[cfg(test)]
mod tests {
    mod checksum_tests;
    mod failure_tests;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mod git_interop;
    mod pack_tests;

    use super::*;
    use gix_transport::bstr::ByteSlice as _;
    use gix_transport::client::blocking_io::Transport as _;
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::{
        net::{Ipv4Addr, TcpListener},
        process::{Command, Stdio},
        thread,
    };

    struct FixtureTls {
        server: Arc<rustls::ServerConfig>,
        trusted: Arc<rustls::ClientConfig>,
    }

    struct FixtureSocket {
        stream: TcpStream,
        deadline: Instant,
        used: u64,
        cap: u64,
    }
    impl FixtureSocket {
        fn timeout(&self) -> io::Result<Duration> {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                Err(safe_error(io::ErrorKind::TimedOut, "fixture deadline"))
            } else {
                Ok(left.min(IO_SLICE))
            }
        }
    }
    impl Read for FixtureSocket {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if out.is_empty() {
                return Ok(0);
            }
            self.stream.set_read_timeout(Some(self.timeout()?))?;
            let left = self.cap.saturating_sub(self.used);
            if left == 0 {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "fixture ingress cap",
                ));
            }
            let allowed = out.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            let n = read_until(
                &mut self.stream,
                self.deadline,
                &CancellationHandle::new(),
                &mut out[..allowed],
            )?;
            self.used = self.used.saturating_add(n as u64);
            self.timeout()?;
            Ok(n)
        }
    }
    impl Write for FixtureSocket {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.stream.set_write_timeout(Some(self.timeout()?))?;
            let n = self.stream.write(data)?;
            self.timeout()?;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.timeout()?;
            self.stream.flush()?;
            self.timeout()?;
            Ok(())
        }
    }
    type FixtureStream = rustls::StreamOwned<rustls::ServerConnection, FixtureSocket>;

    fn fixture_tls() -> FixtureTls {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let cert = generated.cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            generated.signing_key.serialize_der(),
        ));
        let server = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        let trusted = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        FixtureTls {
            server: Arc::new(server),
            trusted: Arc::new(trusted),
        }
    }

    fn listener() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        (listener, address)
    }

    fn accept_until(listener: &TcpListener) -> io::Result<TcpStream> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if Instant::now() >= deadline {
                return Err(safe_error(
                    io::ErrorKind::TimedOut,
                    "fixture accept deadline",
                ));
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    return Ok(stream);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn tls_stream(
        socket: TcpStream,
        config: Arc<rustls::ServerConfig>,
    ) -> io::Result<FixtureStream> {
        let sock = FixtureSocket {
            stream: socket,
            deadline: Instant::now() + Duration::from_secs(2),
            used: 0,
            cap: 16 * 1024 * 1024,
        };
        let conn = rustls::ServerConnection::new(config)
            .map_err(|_| safe_error(io::ErrorKind::Other, "fixture TLS setup"))?;
        Ok(rustls::StreamOwned::new(conn, sock))
    }

    fn read_request(
        stream: &mut FixtureStream,
    ) -> io::Result<(String, String, Vec<u8>, Option<bool>)> {
        let mut header = Vec::new();
        let mut byte = [0];
        while !header.ends_with(b"\r\n\r\n") {
            if header.len() >= MAX_HEADERS {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "fixture headers too large",
                ));
            }
            stream.read_exact(&mut byte)?;
            header.push(byte[0]);
        }
        let text = std::str::from_utf8(&header)
            .map_err(|_| safe_error(io::ErrorKind::InvalidData, "fixture headers invalid"))?;
        let method = text
            .split_ascii_whitespace()
            .next()
            .unwrap_or("")
            .to_owned();
        let target = text
            .split_ascii_whitespace()
            .nth(1)
            .unwrap_or("")
            .to_owned();
        let length = text
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if length > MAX_POST {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "fixture body too large",
            ));
        }
        let mut body = vec![0; length];
        stream.read_exact(&mut body)?;
        let authorization = text
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .collect::<Vec<_>>();
        let auth_matches = if authorization.is_empty() {
            None
        } else {
            Some(
                authorization.len() == 1
                    && authorization[0].1.trim()
                        == "Basic eC1hY2Nlc3MtdG9rZW46Zml4dHVyZS1pbnZhbGlk",
            )
        };
        Ok((method, target, body, auth_matches))
    }

    fn write_response(
        stream: &mut FixtureStream,
        content_type: &str,
        body: &[u8],
    ) -> io::Result<()> {
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
        stream.write_all(body)?;
        stream.flush()
    }

    fn write_chunked_response(
        stream: &mut FixtureStream,
        content_type: &str,
        chunks: &[&[u8]],
        trailers: &[&str],
    ) -> io::Result<()> {
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nTransfer-Encoding: ChUnKeD\r\nConnection: close\r\n\r\n")?;
        for (index, chunk) in chunks.iter().enumerate() {
            if index == 0 {
                write!(stream, "{:x};fixture=\"ignored;value\"\r\n", chunk.len())?;
            } else {
                write!(stream, "{:x};extension\r\n", chunk.len())?;
            }
            stream.write_all(chunk)?;
            stream.write_all(b"\r\n")?;
        }
        stream.write_all(b"0\r\n")?;
        for trailer in trailers {
            stream.write_all(trailer.as_bytes())?;
            stream.write_all(b"\r\n")?;
        }
        stream.write_all(b"\r\n")?;
        stream.flush()
    }

    fn transport_at(
        endpoint: SocketAddr,
        config: Arc<rustls::ClientConfig>,
        limits: AttemptLimits,
    ) -> GitHubTransport {
        let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
        let http =
            RawHttp::for_test(repository.clone(), limits, config, endpoint, "localhost").unwrap();
        assemble(repository, http, None).unwrap()
    }

    #[test]
    fn repository_policy_is_exact_and_preserves_suffix() {
        assert_eq!(
            RepositoryUrl::parse("https://github.com/a/b.git")
                .unwrap()
                .base(),
            "https://github.com/a/b.git"
        );
        for bad in [
            "http://github.com/a/b",
            "https://github.com:443/a/b",
            "https://user@github.com/a/b",
            "https://github.com/a/b?x",
            "https://github.com/a/b/c",
            "https://github.com/a/../b",
            "https://github.com/a/%62",
            "https://github.com/a/.git",
            "https://github.com/a/b?",
        ] {
            assert!(RepositoryUrl::parse(bad).is_err())
        }
        assert!(RepositoryUrl::parse("https://github.com/a/.github").is_ok());
    }

    #[test]
    fn response_framing_rejects_ambiguous_status_headers_and_redirects() {
        assert_eq!(
            parse_headers(b"HTTP/1.1 401 Unauthorized\r\n\r\n")
                .unwrap()
                .status,
            401
        );
        for raw in [
            &b"HTTP/1.1 +200 OK\r\nContent-Length: 0\r\nContent-Type: x\r\n\r\n"[..],
            b"HTTP/1.1 0200 OK\r\nContent-Length: 0\r\nContent-Type: x\r\n\r\n",
            b"HTTP/1.1 200 OK\x00\r\nContent-Length: 0\r\nContent-Type: x\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: +1\r\nContent-Type: x\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nContent-Length: 0\r\nContent-Type: x\r\n\r\n",
            b"HTTP/1.1 200 OK\r\n Content-Length: 0\r\nContent-Type: x\r\n\r\n",
            b"HTTP/1.1 302 Found\r\nLocation: https://elsewhere.invalid/\r\nContent-Length: 0\r\n\r\n",
        ] { assert!(parse_headers(raw).is_err()); }
    }

    fn chunk_body(bytes: &[u8], deadline: Instant) -> Body<Cursor<Vec<u8>>> {
        Body {
            stream: Cursor::new(bytes.to_vec()),
            deadline,
            cancellation: CancellationHandle::new(),
            decoded_remaining: None,
            framing: BodyFraming::Chunked(ChunkState::Size),
            payload: [0; BODY_BUFFER],
            payload_start: 0,
            payload_end: 0,
            failed: None,
            evidence: None,
        }
    }

    #[test]
    fn cancelled_cached_body_fails_stickily_and_marks_response_failed() {
        let ledger = ResponseLedger::default();
        let token = CancellationHandle::new();
        let mut body = Body {
            stream: Cursor::new(b"cached".to_vec()),
            deadline: Instant::now() + Duration::from_secs(1),
            cancellation: token.clone(),
            decoded_remaining: None,
            framing: BodyFraming::Fixed(6),
            payload: {
                let mut payload = [0; BODY_BUFFER];
                payload[..6].copy_from_slice(b"cached");
                payload
            },
            payload_start: 0,
            payload_end: 6,
            failed: None,
            evidence: Some(ledger.reserve().unwrap().into_body()),
        };
        token.cancel();
        assert_eq!(
            body.fill_buf().unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        assert_eq!(
            body.fill_buf().unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        assert_eq!(
            body.read_exact(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.failed(), 1);
        assert_eq!(snapshot.completed(), 0);
    }

    #[test]
    fn cancellation_before_connect_refuses_loopback_connection() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
        let token = CancellationHandle::new();
        token.cancel();
        let http = RawHttp::new(
            repository,
            AttemptLimits::default(),
            tls.trusted,
            token,
            Some((address, Arc::from("localhost"))),
        );
        assert!(
            matches!(http.connect_socket(), Err(error) if error.kind() == io::ErrorKind::ConnectionAborted)
        );
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn public_cancellation_is_one_way_and_attempt_scoped() {
        let first = connect(
            RepositoryUrl::parse("https://github.com/a/b").unwrap(),
            AttemptLimits::default(),
            None,
        )
        .unwrap();
        let second = connect(
            RepositoryUrl::parse("https://github.com/a/b").unwrap(),
            AttemptLimits::default(),
            None,
        )
        .unwrap();
        let handle = first.cancellation_handle();
        let clone = handle.clone();
        assert!(!clone.is_cancelled());
        handle.cancel();
        handle.cancel();
        assert!(clone.is_cancelled());
        assert!(first.cancellation_handle().is_cancelled());
        assert!(!second.cancellation_handle().is_cancelled());
    }

    #[test]
    fn cancellation_interrupts_a_synchronized_loopback_response_read() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
        let token = CancellationHandle::new();
        let mut http = RawHttp::new(
            repository,
            AttemptLimits::default(),
            tls.trusted.clone(),
            token.clone(),
            Some((address, Arc::from("localhost"))),
        );
        let ledger = http.framing.clone();
        let server_config = Arc::clone(&tls.server);
        let (ready_tx, ready_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, server_config).unwrap();
            let _ = read_request(&mut stream).unwrap();
            ready_tx.send(()).unwrap();
            // Keep the response absent until the client has observed cancellation.
            let mut byte = [0];
            let _ = stream.read(&mut byte);
        });
        let client = thread::spawn(move || {
            http.request(
                "GET",
                "/info/refs?service=git-upload-pack",
                Vec::new(),
                None,
                ledger.reserve().unwrap(),
            )
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        token.cancel();
        assert!(
            matches!(client.join().unwrap(), Err(error) if error.kind() == io::ErrorKind::ConnectionAborted)
        );
        server.join().unwrap();
    }

    fn tracked_chunk_body(bytes: &[u8], ledger: &ResponseLedger) -> Body<Cursor<Vec<u8>>> {
        Body {
            stream: Cursor::new(bytes.to_vec()),
            deadline: Instant::now() + Duration::from_secs(1),
            cancellation: CancellationHandle::new(),
            decoded_remaining: None,
            framing: BodyFraming::Chunked(ChunkState::Size),
            payload: [0; BODY_BUFFER],
            payload_start: 0,
            payload_end: 0,
            failed: None,
            evidence: Some(ledger.reserve().unwrap().into_body()),
        }
    }

    #[test]
    fn chunked_body_decodes_unknown_extensions_and_discards_valid_trailers() {
        let wire = b"2;foo=bar\r\na1\r\n3;quoted=\"x;y\"\r\nbcd\r\n0\r\nX-Checksum: ok\r\n\r\n";
        let mut body = chunk_body(wire, Instant::now() + Duration::from_secs(1));
        let mut decoded = Vec::new();
        body.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, b"a1bcd");
        assert_eq!(body.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn evidence_counts_complete_failed_abandoned_and_keeps_snapshots_immutable() {
        let ledger = ResponseLedger::default();
        let mut body = tracked_chunk_body(b"1\r\na\r\n0\r\n\r\n", &ledger);
        let pending = ledger.snapshot().unwrap();
        assert_eq!(pending.pending(), 1);
        assert!(!pending.all_responses_completed());
        body.read_to_end(&mut Vec::new()).unwrap();
        let completed = ledger.snapshot().unwrap();
        assert_eq!(completed.completed(), 1);
        assert!(completed.all_responses_completed());
        assert_eq!(pending.pending(), 1, "snapshot is a stable value");

        let mut failed_body = tracked_chunk_body(b"0\r\nAuthorization: secret\r\n\r\n", &ledger);
        assert!(failed_body.read_to_end(&mut Vec::new()).is_err());
        assert!(failed_body.read(&mut [0]).is_err());
        drop(failed_body);
        let failed = ledger.snapshot().unwrap();
        assert_eq!(failed.failed(), 1);
        assert_eq!(failed.completed(), 1);
        assert!(!failed.all_responses_completed());

        let abandoned_body = tracked_chunk_body(b"2\r\nab\r\n0\r\n\r\n", &ledger);
        drop(abandoned_body);
        let abandoned = ledger.snapshot().unwrap();
        assert_eq!(abandoned.abandoned(), 1);
        assert!(!abandoned.all_responses_completed());
    }

    #[test]
    fn dropping_tracked_body_performs_no_read_or_network_work() {
        struct NoRead;
        impl Read for NoRead {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("drop must not read")
            }
        }
        let ledger = ResponseLedger::default();
        let body = Body {
            stream: NoRead,
            deadline: Instant::now() + Duration::from_secs(1),
            cancellation: CancellationHandle::new(),
            decoded_remaining: None,
            framing: BodyFraming::Chunked(ChunkState::Size),
            payload: [0; BODY_BUFFER],
            payload_start: 0,
            payload_end: 0,
            failed: None,
            evidence: Some(ledger.reserve().unwrap().into_body()),
        };
        drop(body);
        assert_eq!(ledger.snapshot().unwrap().abandoned(), 1);
    }

    #[test]
    fn response_limit_refuses_sixty_fifth_and_invalidates_success_predicate() {
        let ledger = ResponseLedger::default();
        for _ in 0..MAX_HTTP_RESPONSES {
            let mut response = ledger.reserve().unwrap();
            response.complete().unwrap();
        }
        assert!(ledger.snapshot().unwrap().all_responses_completed());
        assert!(ledger.reserve().is_err());
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.completed(), MAX_HTTP_RESPONSES);
        assert!(!snapshot.all_responses_completed());
    }

    #[test]
    fn sixty_fifth_response_refused_before_loopback_connect() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let repository = RepositoryUrl::parse("https://github.com/example/repo").unwrap();
        let mut http = RawHttp::for_test(
            repository.clone(),
            AttemptLimits::default(),
            Arc::clone(&tls.trusted),
            address,
            "localhost",
        )
        .unwrap();
        for _ in 0..MAX_HTTP_RESPONSES {
            let mut guard = http.framing.reserve().unwrap();
            guard.complete().unwrap();
        }
        let url = format!("{}/info/refs?service=git-upload-pack", repository.base());
        assert!(http
            .get(&url, repository.base(), std::iter::empty::<&str>())
            .is_err());
        assert!(
            matches!(listener.accept(), Err(ref error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        let snapshot = http.framing.snapshot().unwrap();
        assert_eq!(snapshot.completed(), MAX_HTTP_RESPONSES);
        assert!(!snapshot.all_responses_completed());
    }

    #[test]
    fn poisoned_response_ledger_returns_error() {
        let ledger = ResponseLedger::default();
        let poisoned = ledger.clone();
        let _ = thread::spawn(move || {
            let _guard = poisoned.0.lock().unwrap();
            panic!("poison fixture");
        })
        .join();
        assert!(ledger.snapshot().is_err());
        assert!(ledger.reserve().is_err());
    }

    #[test]
    fn malformed_chunk_streams_fail_stickily() {
        let mut too_many_trailers = b"0\r\n".to_vec();
        for _ in 0..=MAX_TRAILER_COUNT {
            too_many_trailers.extend_from_slice(b"X: y\r\n");
        }
        too_many_trailers.extend_from_slice(b"\r\n");
        let mut too_large_trailer = b"0\r\nX: ".to_vec();
        too_large_trailer.extend(vec![b'x'; MAX_TRAILER_BYTES]);
        too_large_trailer.extend_from_slice(b"\r\n\r\n");
        let mut too_many_trailer_bytes = b"0\r\n".to_vec();
        for _ in 0..3 {
            too_many_trailer_bytes.extend_from_slice(b"X: ");
            too_many_trailer_bytes.extend(vec![b'x'; 3000]);
            too_many_trailer_bytes.extend_from_slice(b"\r\n");
        }
        too_many_trailer_bytes.extend_from_slice(b"\r\n");
        for bad in [
            &b"Z\r\n"[..],
            b"10000000000000000\r\n",
            b"3\r\nx",
            b"1\r\nxZ\n0\r\n\r\n",
            b"1\r\nx\n0\r\n\r\n",
            b"0\r\n folded: bad\r\n\r\n",
            b"0\r\nAuthorization: secret\r\n\r\n",
            b"1;=value\r\nx\r\n0\r\n\r\n",
            b"1;quoted=\"bad\x01\"\r\nx\r\n0\r\n\r\n",
            too_many_trailers.as_slice(),
            too_large_trailer.as_slice(),
            too_many_trailer_bytes.as_slice(),
        ] {
            let mut body = chunk_body(bad, Instant::now() + Duration::from_secs(1));
            assert!(body.read_to_end(&mut Vec::new()).is_err());
            let first_kind = body.failed.unwrap();
            assert_eq!(body.read(&mut [0]).unwrap_err().kind(), first_kind);
        }
    }

    #[test]
    fn transfer_encoding_parser_rejects_ambiguous_or_unsupported_framing() {
        for headers in [
            &b"Transfer-Encoding: chunked\r\nContent-Length: 1\r\n"[..],
            b"Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n",
            b"Transfer-Encoding: gzip, chunked\r\n",
            b"Transfer-Encoding: gzip\r\n",
        ] {
            let mut response = b"HTTP/1.1 200 OK\r\nContent-Type: x\r\n".to_vec();
            response.extend_from_slice(headers);
            response.extend_from_slice(b"\r\n");
            assert!(parse_headers(&response).is_err());
        }
        assert!(
            parse_headers(
                b"HTTP/1.1 200 OK\r\nContent-Type: x\r\nTransfer-Encoding: ChUnKeD\r\n\r\n"
            )
            .unwrap()
            .chunked
        );
    }

    struct SlowByte {
        sent: bool,
    }
    impl Read for SlowByte {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.sent || out.is_empty() {
                return Ok(0);
            }
            thread::sleep(Duration::from_millis(20));
            out[0] = b'0';
            self.sent = true;
            Ok(1)
        }
    }

    #[test]
    fn chunk_control_reads_enforce_sticky_attempt_deadline() {
        let mut body = Body {
            stream: SlowByte { sent: false },
            deadline: Instant::now() + Duration::from_millis(1),
            cancellation: CancellationHandle::new(),
            decoded_remaining: None,
            framing: BodyFraming::Chunked(ChunkState::Size),
            payload: [0; BODY_BUFFER],
            payload_start: 0,
            payload_end: 0,
            failed: None,
            evidence: None,
        };
        assert_eq!(
            body.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            body.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn cached_chunk_payload_still_checks_and_sticks_expired_deadline() {
        let mut body = chunk_body(
            b"3\r\nabc\r\n0\r\n\r\n",
            Instant::now() + Duration::from_secs(1),
        );
        assert_eq!(body.fill_buf().unwrap(), b"abc");
        body.consume(1);
        body.deadline = Instant::now() - Duration::from_millis(1);
        assert_eq!(body.fill_buf().unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(body.fill_buf().unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn outbound_headers_reject_control_values_and_config_cannot_disable_tls() {
        assert!(validate_request_headers("GET", &["User-Agent: a\x1bb".to_owned()]).is_err());
        assert!(validate_request_headers("GET", &["User-Agent: café".to_owned()]).is_err());
        let mut transport = connect(
            RepositoryUrl::parse("https://github.com/a/b").unwrap(),
            AttemptLimits::default(),
            None,
        )
        .unwrap();
        assert!(transport.configure(&false).is_err());
        assert!(transport
            .set_identity(Account {
                username: "fixture".into(),
                password: "fixture".into(),
                oauth_refresh_token: None
            })
            .is_err());
    }

    #[test]
    fn factory_validates_without_connecting_and_tests_cannot_fall_back_to_network() {
        let limits = AttemptLimits::new(1024 * 1024, Duration::from_secs(2)).unwrap();
        let transport = connect(
            RepositoryUrl::parse("https://github.com/a/b").unwrap(),
            limits,
            None,
        )
        .unwrap();
        assert_eq!(
            transport.to_url().as_ref().to_str().unwrap(),
            "https://github.com/a/b"
        );
        assert!(!transport
            .http_framing_snapshot()
            .unwrap()
            .all_responses_completed());
        let (listener, address) = listener();
        let mut http = RawHttp::for_test(
            RepositoryUrl::parse("https://github.com/a/b").unwrap(),
            limits,
            Arc::new(client_config().unwrap()),
            address,
            "localhost",
        )
        .unwrap();
        assert!(http
            .get(
                "https://evil.example/a/b/info/refs?service=git-upload-pack",
                "https://github.com/a/b",
                std::iter::empty::<&str>()
            )
            .is_err());
        assert!(http
            .get(
                "https://github.com/a/b/info/refs?service=git-upload-pack",
                "https://github.com/a/b",
                ["Host: attacker.invalid"],
            )
            .is_err());
        assert!(
            matches!(listener.accept(), Err(ref error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        assert!(http
            .get(
                "https://github.com/a/b/info/refs?service=git-upload-pack",
                "https://github.com/a/b",
                std::iter::empty::<&str>()
            )
            .is_err());
        let snapshot = http.framing.snapshot().unwrap();
        assert_eq!(snapshot.failed(), 1);
        assert_eq!(snapshot.pending(), 0);
    }

    #[test]
    fn local_trusted_tls_drives_actual_gix_get_and_post() {
        let tls = fixture_tls();
        let (listener1, address) = listener();
        let server_config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            let advertisement = b"000eversion 2\n000cagent=x\n0000";
            let result_body = b"0009\x01PACK0000";
            for (expected, ct, body) in [
                (
                    "GET",
                    "application/x-git-upload-pack-advertisement",
                    advertisement.as_slice(),
                ),
                (
                    "POST",
                    "application/x-git-upload-pack-result",
                    result_body.as_slice(),
                ),
            ] {
                let socket = accept_until(&listener1).unwrap();
                let mut stream = tls_stream(socket, Arc::clone(&server_config)).unwrap();
                let (method, target, request_body, auth_matches) =
                    read_request(&mut stream).unwrap();
                assert_eq!(method, expected);
                assert!(
                    auth_matches.is_none(),
                    "anonymous request must not carry the fixture credential"
                );
                assert_eq!(
                    target,
                    if expected == "GET" {
                        "/example/repo.git/info/refs?service=git-upload-pack"
                    } else {
                        "/example/repo.git/git-upload-pack"
                    }
                );
                if expected == "POST" {
                    assert!(request_body.ends_with(b"0000"));
                    assert!(!request_body.is_empty());
                }
                write_response(&mut stream, ct, body).unwrap();
            }
        });
        let mut transport = transport_at(
            address,
            Arc::clone(&tls.trusted),
            AttemptLimits::new(1024 * 1024, Duration::from_secs(3)).unwrap(),
        );
        let operation = (|| -> io::Result<()> {
            transport
                .handshake(Service::UploadPack, &[])
                .map_err(|_| safe_error(io::ErrorKind::Other, "fixture handshake failed"))?;
            let request = transport
                .request(
                    gix_transport::client::WriteMode::Binary,
                    gix_transport::client::MessageKind::Flush,
                    true,
                )
                .map_err(|_| safe_error(io::ErrorKind::Other, "fixture request failed"))?;
            let mut response = request
                .into_read()
                .map_err(|_| safe_error(io::ErrorKind::Other, "fixture response failed"))?;
            response
                .read_to_end(&mut Vec::new())
                .map_err(|_| safe_error(io::ErrorKind::Other, "fixture response failed"))?;
            Ok(())
        })();
        let server_result = server.join();
        assert!(server_result.is_ok(), "local fixture server thread failed");
        assert!(operation.is_ok(), "local TLS gix exchange failed");
    }

    #[test]
    fn local_tls_gix_get_and_post_decode_chunk_splits_extensions_and_discard_trailers() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            for (expected, content_type, chunks) in [
                (
                    "GET",
                    "application/x-git-upload-pack-advertisement",
                    vec![&b"000e"[..], &b"version 2\n000"[..], &b"cagent=x\n0000"[..]],
                ),
                (
                    "POST",
                    "application/x-git-upload-pack-result",
                    vec![&b"0009\x01"[..], &b"PACK"[..], &b"0000"[..]],
                ),
            ] {
                let socket = accept_until(&listener).unwrap();
                let mut stream = tls_stream(socket, Arc::clone(&config)).unwrap();
                let (method, target, request_body, _) = read_request(&mut stream).unwrap();
                assert_eq!(method, expected);
                assert_eq!(
                    target,
                    if expected == "GET" {
                        "/example/repo.git/info/refs?service=git-upload-pack"
                    } else {
                        "/example/repo.git/git-upload-pack"
                    }
                );
                if expected == "POST" {
                    assert!(request_body.ends_with(b"0000"));
                }
                write_chunked_response(
                    &mut stream,
                    content_type,
                    &chunks,
                    &["X-Fixture-Note: ignored"],
                )
                .unwrap();
            }
        });
        let mut transport = transport_at(
            address,
            Arc::clone(&tls.trusted),
            AttemptLimits::new(1024 * 1024, Duration::from_secs(3)).unwrap(),
        );
        let operation = (|| -> io::Result<Vec<u8>> {
            transport
                .handshake(Service::UploadPack, &[])
                .map_err(|_| safe_error(io::ErrorKind::Other, "fixture handshake failed"))?;
            let request = transport
                .request(
                    gix_transport::client::WriteMode::Binary,
                    gix_transport::client::MessageKind::Flush,
                    true,
                )
                .map_err(|_| safe_error(io::ErrorKind::Other, "fixture request failed"))?;
            let mut response = request
                .into_read()
                .map_err(|_| safe_error(io::ErrorKind::Other, "fixture response failed"))?;
            let mut bytes = Vec::new();
            response
                .read_to_end(&mut bytes)
                .map_err(|_| safe_error(io::ErrorKind::Other, "fixture response failed"))?;
            Ok(bytes)
        })();
        let server_result = server.join();
        assert!(server_result.is_ok(), "chunked TLS fixture server failed");
        // This gix reader preserves the sideband channel byte.
        assert_eq!(operation.unwrap(), b"\x01PACK");
        let report = transport.finalize_http_framing().unwrap();
        assert!(report.is_complete());
        assert_eq!(report.framing().completed(), 2);
    }

    #[test]
    fn full_http_body_read_marks_response_complete() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, config).unwrap();
            let (method, target, _, _) = read_request(&mut stream).unwrap();
            assert_eq!(method, "GET");
            assert_eq!(
                target,
                "/example/repo.git/info/refs?service=git-upload-pack"
            );
            write_response(
                &mut stream,
                "application/x-git-upload-pack-advertisement",
                b"full-body",
            )
            .unwrap();
        });
        let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
        let mut http = RawHttp::for_test(
            repository.clone(),
            AttemptLimits::default(),
            Arc::clone(&tls.trusted),
            address,
            "localhost",
        )
        .unwrap();
        let url = format!("{}/info/refs?service=git-upload-pack", repository.base());
        let mut response = http
            .get(&url, repository.base(), std::iter::empty::<&str>())
            .unwrap();
        assert_eq!(http.framing.snapshot().unwrap().pending(), 1);
        let mut payload = Vec::new();
        response.body.read_to_end(&mut payload).unwrap();
        assert_eq!(payload, b"full-body");
        let snapshot = http.framing.snapshot().unwrap();
        assert_eq!(snapshot.completed(), 1);
        assert!(snapshot.all_responses_completed());
        assert!(server.join().is_ok());
    }

    #[test]
    fn gix_packet_flush_does_not_complete_http_chunked_framing() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, config).unwrap();
            let (method, _, _, _) = read_request(&mut stream).unwrap();
            assert_eq!(method, "GET");
            let payload = b"000eversion 2\n000cagent=x\n0000";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n", payload.len()).unwrap();
            stream.write_all(payload).unwrap();
            stream.write_all(b"\r\n").unwrap();
            stream.flush().unwrap();
            // Deliberately omit the zero chunk and final trailer terminator.
        });
        let mut transport =
            transport_at(address, Arc::clone(&tls.trusted), AttemptLimits::default());
        let ledger = transport.framing.clone();
        let handshake_ok = transport.handshake(Service::UploadPack, &[]).is_ok();
        assert!(server.join().is_ok());
        let before_drop = ledger.snapshot().unwrap();
        assert!(
            handshake_ok,
            "the valid Git packet flush should finish gix handshake"
        );
        assert_eq!(before_drop.pending(), 1);
        assert!(!before_drop.all_responses_completed());
        drop(transport);
        let after_drop = ledger.snapshot().unwrap();
        assert_eq!(after_drop.abandoned(), 1);
        assert!(!after_drop.all_responses_completed());
    }

    #[test]
    fn explicit_finalization_drains_chunk_terminator_after_git_flush() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let config = Arc::clone(&tls.server);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, config).unwrap();
            let (method, _, _, _) = read_request(&mut stream).unwrap();
            assert_eq!(method, "GET");
            let payload = b"000eversion 2\n000cagent=x\n0000";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n", payload.len()).unwrap();
            stream.write_all(payload).unwrap();
            stream.write_all(b"\r\n").unwrap();
            stream.flush().unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            stream
                .write_all(b"0\r\nX-fixture: complete\r\n\r\n")
                .unwrap();
            stream.flush().unwrap();
        });
        let mut transport =
            transport_at(address, Arc::clone(&tls.trusted), AttemptLimits::default());
        assert!(transport.handshake(Service::UploadPack, &[]).is_ok());
        let before = transport.observed_ingress_bytes().unwrap();
        let mut accounting =
            verification_budget::BudgetController::new(verification_budget::Limits {
                headroom_bytes: 0,
            });
        let job = accounting.start(10 * verification_budget::GIB).unwrap();
        accounting.record_ingress_total(job, before).unwrap();
        accounting.record_ingress_total(job, before).unwrap();
        assert_eq!(
            accounting.snapshot(job).unwrap().usage.ingress_bytes,
            before
        );
        // Model cancellation before the worker's final observation arrives.
        // This is accounting evidence, not cancellation routing/worker-stop proof.
        accounting.cancel(job).unwrap();
        release_tx.send(()).unwrap();
        let report = transport.finalize_http_framing().unwrap();
        assert!(server.join().is_ok());
        assert_eq!(report.drain_error_kind(), None);
        assert!(report.is_complete());
        assert_eq!(report.framing().completed(), 1);
        assert_eq!(report.framing().pending(), 0);
        assert!(report.observed_ingress_bytes() > before);
        accounting
            .record_ingress_total(job, report.observed_ingress_bytes())
            .unwrap();
        accounting
            .record_ingress_total(job, report.observed_ingress_bytes())
            .unwrap();
        assert_eq!(
            accounting.snapshot(job).unwrap().usage.ingress_bytes,
            report.observed_ingress_bytes()
        );
        assert!(matches!(
            accounting.snapshot(job).unwrap().state,
            verification_budget::JobState::Canceled {
                worker_stopped: false,
                cleanup_confirmed: false
            }
        ));
        assert_eq!(
            accounting.confirm_cleanup(job),
            Err(verification_budget::BudgetError::Blocked(
                verification_budget::BlockReason::CleanupNotPending
            ))
        );
    }

    #[test]
    fn canceled_finalization_reports_incomplete_framing() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, config).unwrap();
            let (method, _, _, _) = read_request(&mut stream).unwrap();
            assert_eq!(method, "GET");
            let payload = b"000eversion 2\n000cagent=x\n0000";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n", payload.len()).unwrap();
            stream.write_all(payload).unwrap();
            stream.write_all(b"\r\n").unwrap();
            stream.flush().unwrap();
        });
        let mut transport =
            transport_at(address, Arc::clone(&tls.trusted), AttemptLimits::default());
        assert!(transport.handshake(Service::UploadPack, &[]).is_ok());
        let cancellation = transport.cancellation_handle();
        cancellation.cancel();
        let report = transport.finalize_http_framing().unwrap();
        assert!(server.join().is_ok());
        assert_eq!(
            report.drain_error_kind(),
            Some(io::ErrorKind::ConnectionAborted)
        );
        assert!(!report.is_complete());
        assert_eq!(report.framing().pending(), 0);
        assert_eq!(report.framing().abandoned(), 1);
    }

    fn transport_with_finalization_tail(
        tail: &'static [u8],
    ) -> (GitHubTransport, thread::JoinHandle<()>) {
        transport_with_finalization_tail_and_limits(tail.to_vec(), AttemptLimits::default())
    }

    fn transport_with_finalization_tail_and_limits(
        tail: Vec<u8>,
        limits: AttemptLimits,
    ) -> (GitHubTransport, thread::JoinHandle<()>) {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, tls.server).unwrap();
            read_request(&mut stream).unwrap();
            let payload = b"000eversion 2\n000cagent=x\n0000";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n", payload.len()).unwrap();
            stream.write_all(payload).unwrap();
            stream.write_all(b"\r\n").unwrap();
            // A budget-limited client may close before the entire tail is sent.
            let _ = stream.write_all(&tail).and_then(|_| stream.flush());
        });
        let mut transport = transport_at(address, tls.trusted, limits);
        assert!(transport.handshake(Service::UploadPack, &[]).is_ok());
        (transport, server)
    }

    #[test]
    fn finalization_rejects_missing_terminator_and_forbidden_trailer() {
        for tail in [
            b"".as_slice(),
            b"0\r\nAuthorization: fixture\r\n\r\n".as_slice(),
        ] {
            let (transport, server) = transport_with_finalization_tail(tail);
            let before = transport.observed_ingress_bytes().unwrap();
            let report = transport.finalize_http_framing().unwrap();
            server.join().unwrap();
            assert!(!report.is_complete());
            assert!(report.drain_error_kind().is_some());
            assert_eq!(report.framing().failed(), 1);
            assert_eq!(report.framing().pending(), 0);
            assert!(report.observed_ingress_bytes() >= before);
        }
    }

    #[test]
    fn finalization_remains_inside_raw_ingress_cap() {
        let mut tail = b"0\r\nX-fixture: ".to_vec();
        tail.extend(std::iter::repeat_n(b'a', 6000));
        tail.extend_from_slice(b"\r\n\r\n");
        let limits = AttemptLimits::new(4096, Duration::from_secs(5)).unwrap();
        let (transport, server) = transport_with_finalization_tail_and_limits(tail, limits);
        let report = transport.finalize_http_framing().unwrap();
        server.join().unwrap();
        assert!(!report.is_complete());
        assert_eq!(report.observed_ingress_bytes(), 4096);
        assert_eq!(report.drain_error_kind(), Some(io::ErrorKind::InvalidData));
        assert_eq!(report.framing().failed(), 1);
    }

    #[test]
    fn expired_finalizer_abandons_pending_tail() {
        let (mut transport, server) = transport_with_finalization_tail(b"0\r\n\r\n");
        transport.deadline = Instant::now();
        let report = transport.finalize_http_framing().unwrap();
        server.join().unwrap();
        assert_eq!(report.drain_error_kind(), Some(io::ErrorKind::TimedOut));
        assert!(!report.is_complete());
        assert_eq!(report.framing().abandoned(), 1);
    }

    fn unconnected_finalizer_fixture() -> GitHubTransport {
        connect(
            RepositoryUrl::parse("https://github.com/a/b").unwrap(),
            AttemptLimits::default(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn finalizer_zero_attempts_and_exhaustion_cannot_succeed() {
        let report = unconnected_finalizer_fixture()
            .finalize_http_framing()
            .unwrap();
        assert!(!report.is_complete());
        assert_eq!(report.observed_ingress_bytes(), 0);
        let transport = unconnected_finalizer_fixture();
        transport.framing.reserve().unwrap().complete().unwrap();
        transport.retained.0.lock().unwrap().exhausted = true;
        let report = transport.finalize_http_framing().unwrap();
        assert!(report.framing().all_responses_completed());
        assert_eq!(report.drain_error_kind(), Some(io::ErrorKind::InvalidData));
        assert!(!report.is_complete());
    }

    #[test]
    fn poisoned_finalizer_queue_returns_error() {
        let transport = unconnected_finalizer_fixture();
        let retained = transport.retained.clone();
        assert!(thread::spawn(move || {
            let _guard = retained.0.lock().unwrap();
            panic!("poison private queue fixture");
        })
        .join()
        .is_err());
        assert!(transport.finalize_http_framing().is_err());
    }

    #[test]
    fn finalization_does_not_send_uninitialized_post() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
        let mut http = RawHttp::for_test(
            repository.clone(),
            AttemptLimits::default(),
            tls.trusted,
            address,
            "localhost",
        )
        .unwrap();
        let response = http
            .post(
                &format!("{}/git-upload-pack", repository.base()),
                repository.base(),
                [
                    "Content-Type: application/x-git-upload-pack-request",
                    "Accept: application/x-git-upload-pack-result",
                ],
                PostBodyDataKind::BoundedAndFitsIntoMemory,
            )
            .unwrap();
        drop(response);
        let report = assemble(repository, http, None)
            .unwrap()
            .finalize_http_framing()
            .unwrap();
        assert!(!report.is_complete());
        assert_eq!(report.framing().failed(), 1);
        assert_eq!(report.observed_ingress_bytes(), 0);
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn raw_ingress_ceiling_counts_chunk_controls_and_payload() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, config).unwrap();
            let (method, _, _, _) = read_request(&mut stream).unwrap();
            assert_eq!(method, "GET");
            let payload = vec![b'x'; 16 * 1024];
            write_chunked_response(
                &mut stream,
                "application/x-git-upload-pack-advertisement",
                &[&payload],
                &[],
            )
            .unwrap();
        });
        let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
        let limit = 4096u64;
        let mut http = RawHttp::for_test(
            repository.clone(),
            AttemptLimits::new(limit, Duration::from_secs(3)).unwrap(),
            Arc::clone(&tls.trusted),
            address,
            "localhost",
        )
        .unwrap();
        let url = format!("{}/info/refs?service=git-upload-pack", repository.base());
        let mut response = http
            .get(&url, repository.base(), std::iter::empty::<&str>())
            .unwrap();
        assert!(response.body.read_to_end(&mut Vec::new()).is_err());
        assert_eq!(*http.budget.used.lock().unwrap(), limit);
        assert!(server.join().is_ok());
    }

    #[test]
    fn explicit_credential_uses_same_factory_assembly_over_local_tls() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            let mut observed = Vec::new();
            for (content_type, bytes) in [
                (
                    "application/x-git-upload-pack-advertisement",
                    b"000eversion 2\n000cagent=x\n0000".as_slice(),
                ),
                (
                    "application/x-git-upload-pack-result",
                    b"0009\x01PACK0000".as_slice(),
                ),
            ] {
                let socket = accept_until(&listener)?;
                let mut stream = tls_stream(socket, Arc::clone(&config))?;
                let (_, _, _, auth_matches) = read_request(&mut stream)?;
                observed.push(auth_matches);
                write_response(&mut stream, content_type, bytes)?;
            }
            Ok::<_, io::Error>(observed)
        });
        let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
        let http = RawHttp::for_test(
            repository.clone(),
            AttemptLimits::default(),
            tls.trusted,
            address,
            "localhost",
        )
        .unwrap();
        let credential = AppCredential::from_secret("fixture-invalid".to_owned()).unwrap();
        let mut transport = assemble(repository, http, Some(credential)).unwrap();
        let operation = (|| -> io::Result<()> {
            transport
                .handshake(Service::UploadPack, &[])
                .map_err(|_| safe_error(io::ErrorKind::Other, "fixture handshake"))?;
            let request = transport
                .request(
                    gix_transport::client::WriteMode::Binary,
                    gix_transport::client::MessageKind::Flush,
                    false,
                )
                .map_err(|_| safe_error(io::ErrorKind::Other, "fixture request"))?;
            request.into_read()?.read_to_end(&mut Vec::new())?;
            Ok(())
        })();
        let observed = server.join().unwrap().unwrap();
        assert!(operation.is_ok(), "credential fixture exchange failed");
        assert_eq!(
            observed,
            [Some(true), Some(true)],
            "credential header checks failed"
        );
    }

    #[test]
    fn poisoned_lazy_response_never_turns_into_eof_or_connects() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
        let mut http = RawHttp::for_test(
            repository.clone(),
            AttemptLimits::default(),
            tls.trusted,
            address,
            "localhost",
        )
        .unwrap();
        let PostResponse {
            mut post_body,
            mut headers,
            mut body,
        } = http
            .post(
                &format!("{}/git-upload-pack", repository.base()),
                repository.base(),
                [
                    "Content-Type: application/x-git-upload-pack-request",
                    "Accept: application/x-git-upload-pack-result",
                ],
                PostBodyDataKind::BoundedAndFitsIntoMemory,
            )
            .unwrap();
        assert!(post_body.write_all(&vec![0; MAX_POST + 1]).is_err());
        drop(post_body);
        for _ in 0..2 {
            assert_eq!(
                headers.fill_buf().unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                body.fill_buf().unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            http.cancellation.cancel();
        }
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn cancelled_lazy_post_wait_stops_before_connect() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
        let token = CancellationHandle::new();
        let mut http = RawHttp::new(
            repository.clone(),
            AttemptLimits::default(),
            tls.trusted,
            token.clone(),
            Some((address, Arc::from("localhost"))),
        );
        let PostResponse {
            post_body,
            mut headers,
            ..
        } = http
            .post(
                &format!("{}/git-upload-pack", repository.base()),
                repository.base(),
                [
                    "Content-Type: application/x-git-upload-pack-request",
                    "Accept: application/x-git-upload-pack-result",
                ],
                PostBodyDataKind::BoundedAndFitsIntoMemory,
            )
            .unwrap();
        drop(post_body);
        token.cancel();
        assert_eq!(
            headers.fill_buf().unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn response_ingress_limit_and_attempt_deadline_stop_local_tls() {
        let tls = fixture_tls();
        let (listener1, address) = listener();
        let config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            if let Ok(socket) = accept_until(&listener1) {
                if let Ok(mut stream) = tls_stream(socket, config) {
                    let mut byte = [0];
                    let _ = stream.read(&mut byte);
                }
            }
        });
        let mut capped = transport_at(
            address,
            Arc::clone(&tls.trusted),
            AttemptLimits::new(64, Duration::from_secs(2)).unwrap(),
        );
        let capped_result = capped.handshake(Service::UploadPack, &[]).is_err();
        assert!(server.join().is_ok());
        assert!(capped_result);
        assert_eq!(capped.observed_ingress_bytes().unwrap(), 64);

        let (listener2, address) = listener();
        let server = thread::spawn(move || {
            if let Ok(_socket) = accept_until(&listener2) {
                thread::sleep(Duration::from_millis(120));
            }
        });
        let mut timed = transport_at(
            address,
            Arc::clone(&tls.trusted),
            AttemptLimits::new(4096, Duration::from_millis(40)).unwrap(),
        );
        let started = Instant::now();
        let timeout_result = timed.handshake(Service::UploadPack, &[]).is_err();
        assert!(server.join().is_ok());
        assert!(timeout_result);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn oversized_post_is_poisoned_and_never_opens_a_second_connection() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, config).unwrap();
            let (method, target, _, _) = read_request(&mut stream).unwrap();
            assert_eq!(method, "GET");
            assert_eq!(
                target,
                "/example/repo.git/info/refs?service=git-upload-pack"
            );
            write_response(
                &mut stream,
                "application/x-git-upload-pack-advertisement",
                b"000eversion 2\n000cagent=x\n0000",
            )
            .unwrap();
        });
        let mut transport = transport_at(
            address,
            Arc::clone(&tls.trusted),
            AttemptLimits::new(1024 * 1024, Duration::from_secs(3)).unwrap(),
        );
        let handshake_ok = transport.handshake(Service::UploadPack, &[]).is_ok();
        assert!(server.join().is_ok());
        assert!(handshake_ok);
        let mut request = transport
            .request(
                gix_transport::client::WriteMode::Binary,
                gix_transport::client::MessageKind::Flush,
                false,
            )
            .unwrap();
        let oversize = vec![b'x'; MAX_POST + 1];
        assert!(request.write_all(&oversize).is_err());
        assert!(request.into_read().is_err());
    }

    #[test]
    fn untrusted_and_wrong_name_certificates_fail_tls_verification() {
        let tls = fixture_tls();
        let (listener1, address) = listener();
        let server_config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            let Ok(socket) = accept_until(&listener1) else {
                return;
            };
            let Ok(mut stream) = tls_stream(socket, server_config) else {
                return;
            };
            let mut byte = [0];
            let _ = stream.read(&mut byte);
        });
        let mut untrusted = transport_at(
            address,
            Arc::new(client_config().unwrap()),
            AttemptLimits::new(2048, Duration::from_secs(2)).unwrap(),
        );
        let untrusted_result = untrusted.handshake(Service::UploadPack, &[]).is_err();
        assert!(server.join().is_ok());
        assert!(untrusted_result);

        let (listener2, address) = listener();
        let server_config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            let Ok(socket) = accept_until(&listener2) else {
                return;
            };
            let Ok(mut stream) = tls_stream(socket, server_config) else {
                return;
            };
            let mut byte = [0];
            let _ = stream.read(&mut byte);
        });
        let repository = RepositoryUrl::parse("https://github.com/a/b").unwrap();
        let raw = RawHttp::for_test(
            repository.clone(),
            AttemptLimits::new(2048, Duration::from_secs(2)).unwrap(),
            Arc::clone(&tls.trusted),
            address,
            "wrong.local",
        )
        .unwrap();
        let mut inner = gix_transport::client::blocking_io::http::Transport::new_http(
            raw,
            gix_url::parse(repository.base()).unwrap(),
            gix_transport::Protocol::V2,
            false,
        );
        let wrong_name_result = inner.handshake(Service::UploadPack, &[]).is_err();
        assert!(server.join().is_ok());
        assert!(wrong_name_result);
    }

    #[test]
    fn post_auth_denial_stays_permission_denied_on_repeated_lazy_reads() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let config = Arc::clone(&tls.server);
        let server = thread::spawn(move || -> io::Result<()> {
            let socket = accept_until(&listener)?;
            let mut stream = tls_stream(socket, config)?;
            let (method, _, _, _) = read_request(&mut stream)?;
            if method != "POST" {
                return Err(safe_error(io::ErrorKind::InvalidData, "fixture method"));
            }
            stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n")?;
            stream.flush()
        });
        let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
        let mut http = RawHttp::for_test(
            repository.clone(),
            AttemptLimits::default(),
            tls.trusted,
            address,
            "localhost",
        )
        .unwrap();
        let PostResponse {
            mut post_body,
            mut headers,
            mut body,
        } = http
            .post(
                &format!("{}/git-upload-pack", repository.base()),
                repository.base(),
                [
                    "Content-Type: application/x-git-upload-pack-request",
                    "Accept: application/x-git-upload-pack-result",
                ],
                PostBodyDataKind::BoundedAndFitsIntoMemory,
            )
            .unwrap();
        post_body.write_all(b"0000").unwrap();
        drop(post_body);
        let first = headers.fill_buf().unwrap_err().kind();
        let server_result = server.join().unwrap();
        assert!(server_result.is_ok());
        assert_eq!(first, io::ErrorKind::PermissionDenied);
        assert_eq!(headers.fill_buf().unwrap_err().kind(), first);
        assert_eq!(body.fill_buf().unwrap_err().kind(), first);
    }

    #[test]
    fn auth_required_without_body_headers_keeps_permission_denied_kind() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let config = Arc::clone(&tls.server);
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, config).unwrap();
            let (method, target, _, _) = read_request(&mut stream).unwrap();
            assert_eq!(method, "GET");
            assert_eq!(
                target,
                "/example/repo.git/info/refs?service=git-upload-pack"
            );
            stream
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n")
                .unwrap();
            stream.flush().unwrap();
        });
        let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
        let mut http = RawHttp::for_test(
            repository,
            AttemptLimits::new(4096, Duration::from_secs(2)).unwrap(),
            Arc::clone(&tls.trusted),
            address,
            "localhost",
        )
        .unwrap();
        let evidence = http.framing.reserve().unwrap();
        let error = match http.request(
            "GET",
            "/info/refs?service=git-upload-pack",
            Vec::new(),
            None,
            evidence,
        ) {
            Ok(_) => panic!("401 must fail"),
            Err(error) => error,
        };
        assert!(server.join().is_ok());
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
    fn ref_packet(line: &[u8]) -> Vec<u8> {
        let mut packet = format!("{:04x}", line.len() + 4).into_bytes();
        packet.extend_from_slice(line);
        packet
    }

    fn ref_caps(lines: &[&[u8]]) -> Vec<u8> {
        let mut bytes = ref_packet(b"version 2\n");
        for line in lines {
            bytes.extend(ref_packet(line));
        }
        bytes.extend_from_slice(b"0000");
        bytes
    }

    fn ref_transport_fixture(
        caps: Vec<u8>,
        response: Option<Vec<u8>>,
    ) -> (GitHubTransport, thread::JoinHandle<()>) {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, Arc::clone(&tls.server)).unwrap();
            let (method, target, _, _) = read_request(&mut stream).unwrap();
            assert_eq!(method, "GET");
            assert_eq!(
                target,
                "/example/repo.git/info/refs?service=git-upload-pack"
            );
            write_response(
                &mut stream,
                "application/x-git-upload-pack-advertisement",
                &caps,
            )
            .unwrap();
            if let Some(response) = response {
                let socket = accept_until(&listener).unwrap();
                let mut stream = tls_stream(socket, tls.server).unwrap();
                let (method, target, body, _) = read_request(&mut stream).unwrap();
                assert_eq!(method, "POST");
                assert_eq!(target, "/example/repo.git/git-upload-pack");
                assert_eq!(
                    body,
                    b"0014command=ls-refs\n00010009peel\n000csymrefs\n0000"
                );
                write_response(
                    &mut stream,
                    "application/x-git-upload-pack-result",
                    &response,
                )
                .unwrap();
            }
        });
        let transport = transport_at(address, tls.trusted, AttemptLimits::default());
        (transport, server)
    }

    fn observe_ref_fixture(
        caps: Vec<u8>,
        response: Option<Vec<u8>>,
    ) -> (io::Result<Vec<AdvertisedRef>>, HttpFinalizationReport) {
        let (mut transport, server) = ref_transport_fixture(caps, response);
        let result = transport.read_advertised_refs();
        let finalized = transport.finalize_http_framing().unwrap();
        server.join().unwrap();
        (result, finalized)
    }

    #[test]
    fn actual_ls_refs_observes_metadata_and_finalizes_http_separately() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let mut response = Vec::new();
        for line in [
            format!("{oid} HEAD symref-target:refs/heads/main\n"),
            format!("{oid} refs/heads/main\n"),
            format!("{oid} refs/tags/v1 peeled:{oid}\n"),
        ] {
            response.extend(ref_packet(line.as_bytes()));
        }
        response.extend_from_slice(b"0000");
        let (result, finalized) = observe_ref_fixture(
            ref_caps(&[b"ls-refs\n", b"object-format=sha1\n"]),
            Some(response),
        );
        let refs = result.unwrap();
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].name(), b"HEAD");
        assert_eq!(refs[0].symref_target(), Some(b"refs/heads/main".as_slice()));
        assert_eq!(refs[2].peeled(), Some(refs[2].oid()));
        assert!(finalized.is_complete());
        assert_eq!(finalized.framing().completed(), 2);
    }

    #[test]
    fn combined_observation_exposes_refs_only_after_both_layers_complete() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let mut response = ref_packet(format!("{oid} refs/heads/main\n").as_bytes());
        response.extend_from_slice(b"0000");
        let (transport, server) = ref_transport_fixture(
            ref_caps(&[b"ls-refs\n", b"object-format=sha1\n"]),
            Some(response),
        );
        let report = transport.observe_refs().unwrap();
        server.join().unwrap();
        assert!(report.is_complete());
        assert_eq!(report.refs().unwrap().len(), 1);
        assert_eq!(report.refs().unwrap()[0].name(), b"refs/heads/main");
        assert!(report.finalization().is_complete());
        let mut accounting =
            verification_budget::BudgetController::new(verification_budget::Limits {
                headroom_bytes: 0,
            });
        let job = accounting.start(10 * verification_budget::GIB).unwrap();
        let total = report.finalization().observed_ingress_bytes();
        assert!(total > 0);
        accounting.record_ingress_total(job, total).unwrap();
        accounting.record_ingress_total(job, total).unwrap();
        assert_eq!(accounting.snapshot(job).unwrap().usage.ingress_bytes, total);
        assert_eq!(
            accounting.snapshot(job).unwrap().state,
            verification_budget::JobState::Running
        );
        assert!(accounting.confirm_cleanup(job).is_err());
    }

    #[test]
    fn combined_observation_keeps_protocol_failure_even_when_http_completes() {
        let line = b"0123456789abcdef0123456789abcdef01234567 refs/heads/main\n";
        let response = ref_packet(line);
        // A data packet without the Git flush terminator is a truncated response.
        let (transport, server) = ref_transport_fixture(ref_caps(&[b"ls-refs\n"]), Some(response));
        let report = transport.observe_refs().unwrap();
        server.join().unwrap();
        assert_eq!(
            report.operation_error_kind(),
            Some(io::ErrorKind::UnexpectedEof)
        );
        assert!(report.refs().is_none());
        assert!(report.finalization().is_complete());
        assert!(!report.is_complete());
    }

    #[test]
    fn combined_observation_discards_refs_when_chunked_http_tail_is_invalid() {
        for tail in [
            b"\r\n".as_slice(),
            b"\r\n0\r\nAuthorization: fixture\r\n\r\n".as_slice(),
        ] {
            let tls = fixture_tls();
            let (listener, address) = listener();
            let server = thread::spawn(move || {
                let socket = accept_until(&listener).unwrap();
                let mut stream = tls_stream(socket, Arc::clone(&tls.server)).unwrap();
                let (method, target, _, _) = read_request(&mut stream).unwrap();
                assert_eq!(method, "GET");
                assert_eq!(
                    target,
                    "/example/repo.git/info/refs?service=git-upload-pack"
                );
                write_response(
                    &mut stream,
                    "application/x-git-upload-pack-advertisement",
                    &ref_caps(&[b"ls-refs\n"]),
                )
                .unwrap();
                let socket = accept_until(&listener).unwrap();
                let mut stream = tls_stream(socket, tls.server).unwrap();
                let (method, target, _, _) = read_request(&mut stream).unwrap();
                assert_eq!(method, "POST");
                assert_eq!(target, "/example/repo.git/git-upload-pack");
                let mut body =
                    ref_packet(b"0123456789abcdef0123456789abcdef01234567 refs/heads/main\n");
                body.extend_from_slice(b"0000");
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-result\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
                stream.write_all(tail).unwrap();
                stream.flush().unwrap();
            });
            let transport = transport_at(address, tls.trusted, AttemptLimits::default());
            let report = transport.observe_refs().unwrap();
            server.join().unwrap();
            assert!(report.operation_error_kind().is_none());
            assert!(report.refs().is_none());
            assert!(!report.finalization().is_complete());
            assert!(!report.is_complete());
        }
    }

    #[test]
    fn combined_observation_distinguishes_valid_empty_refs_from_no_refs() {
        let (transport, server) =
            ref_transport_fixture(ref_caps(&[b"ls-refs\n"]), Some(b"0000".to_vec()));
        let report = transport.observe_refs().unwrap();
        server.join().unwrap();
        assert!(report.is_complete());
        assert_eq!(report.refs(), Some(&[][..]));
    }

    #[test]
    fn combined_observation_preserves_pre_cancel_without_ingress() {
        let transport = crate::connect(
            crate::RepositoryUrl::parse("https://github.com/a/b").unwrap(),
            crate::AttemptLimits::default(),
            None,
        )
        .unwrap();
        transport.cancellation_handle().cancel();
        let report = transport.observe_refs().unwrap();
        assert_eq!(
            report.operation_error_kind(),
            Some(io::ErrorKind::ConnectionAborted)
        );
        assert_eq!(report.finalization().observed_ingress_bytes(), 0);
        assert!(report.refs().is_none());
    }

    #[test]
    fn combined_observation_propagates_poisoned_finalization_state() {
        let transport = unconnected_finalizer_fixture();
        let retained = transport.retained.clone();
        assert!(thread::spawn(move || {
            let _guard = retained.0.lock().unwrap();
            panic!("poison private queue fixture");
        })
        .join()
        .is_err());
        assert_eq!(
            transport.observe_refs().unwrap_err().kind(),
            io::ErrorKind::Other
        );
    }

    #[test]
    fn ls_refs_rejects_unsupported_capabilities_before_post() {
        for caps in [
            ref_caps(&[]),
            ref_caps(&[b"ls-refs\n", b"object-format=sha256\n"]),
            ref_caps(&[b"ls-refs\n", b"ls-refs\n"]),
            ref_caps(&[
                b"ls-refs\n",
                b"object-format=sha1\n",
                b"object-format=sha1\n",
            ]),
        ] {
            let (result, _) = observe_ref_fixture(caps, None);
            assert!(result.is_err());
        }
    }

    #[test]
    fn ls_refs_rejects_duplicates_truncation_and_excess_results_without_partial_success() {
        let line = b"0123456789abcdef0123456789abcdef01234567 refs/heads/main\n";
        let mut duplicate = ref_packet(line);
        duplicate.extend(ref_packet(line));
        duplicate.extend_from_slice(b"0000");
        let mut excess = Vec::new();
        for n in 0..1025 {
            excess.extend(ref_packet(
                format!("0123456789abcdef0123456789abcdef01234567 refs/heads/b{n}\n").as_bytes(),
            ));
        }
        excess.extend_from_slice(b"0000");
        for (response, expected_error) in [
            (duplicate, Some("advertised reference limit or duplicate")),
            (ref_packet(line), None),
            (excess, Some("advertised reference limit or duplicate")),
            (b"0001".to_vec(), None),
        ] {
            let (result, finalized) =
                observe_ref_fixture(ref_caps(&[b"ls-refs\n"]), Some(response));
            let error = result.unwrap_err();
            if let Some(expected) = expected_error {
                assert_eq!(error.to_string(), expected);
            }
            // HTTP completion alone cannot establish Git protocol completion.
            assert!(finalized.is_complete());
        }
    }

    #[test]
    fn decoded_get_limit_allows_exact_termination_and_rejects_extra_data() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exact = chunk_body(b"3\r\nabc\r\n0\r\n\r\n", deadline);
        exact.decoded_remaining = Some(3);
        let mut bytes = Vec::new();
        exact.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"abc");
        let mut excess = chunk_body(b"4\r\nabcd\r\n0\r\n\r\n", deadline);
        excess.decoded_remaining = Some(3);
        assert!(excess.read_to_end(&mut Vec::new()).is_err());
        assert!(excess.read(&mut [0]).is_err());
    }

    #[test]
    fn oversized_fixed_advertisement_is_rejected_before_git_parsing() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, tls.server).unwrap();
            read_request(&mut stream).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: 65537\r\n\r\n").unwrap();
            stream.flush().unwrap();
        });
        let mut transport = transport_at(address, tls.trusted, AttemptLimits::default());
        assert!(transport.read_advertised_refs().is_err());
        assert!(!transport.finalize_http_framing().unwrap().is_complete());
        server.join().unwrap();
    }

    const OUTER_CHILD_READY: &[u8] = b"CANDIDATE_TLS_GET_VALIDATED\n";

    /// Exact child entry point for the outer-worker deadline proof.
    #[test]
    #[ignore = "invoked only by the exact candidate transport deadline parent"]
    fn candidate_tls_blocked_handshake_child() {
        let tls = fixture_tls();
        let (listener, address) = listener();
        let server_config = tls.server.clone();
        let server = thread::spawn(move || {
            let socket = accept_until(&listener).expect("accept candidate TLS connection");
            let sock = FixtureSocket {
                stream: socket,
                // Keep the fixture socket's own deadline beyond the parent's
                // outer deadline; the child must stay blocked in the candidate.
                deadline: Instant::now() + Duration::from_secs(35),
                used: 0,
                cap: 1024 * 1024,
            };
            let conn = rustls::ServerConnection::new(server_config)
                .expect("create private fixture TLS server");
            let mut stream = rustls::StreamOwned::new(conn, sock);
            let (method, target, _, _) =
                read_request(&mut stream).expect("read complete candidate request over TLS");
            assert_eq!(method, "GET", "candidate must issue GET");
            assert_eq!(
                target, "/example/repo.git/info/refs?service=git-upload-pack",
                "candidate must preserve the validated repository path"
            );
            println!("{}", std::str::from_utf8(OUTER_CHILD_READY).unwrap().trim());
            io::stdout().flush().expect("flush validated GET sentinel");
            // Do not write an HTTP response. The outer supervisor must stop
            // this process before its 30-second candidate deadline.
            thread::sleep(Duration::from_secs(35));
        });

        let repository = RepositoryUrl::parse("https://github.com/example/repo.git")
            .expect("validated fixture repository");
        let limits = AttemptLimits::new(1024 * 1024, Duration::from_secs(30))
            .expect("candidate's bounded inner limits");
        let raw = RawHttp::for_test(
            repository.clone(),
            limits,
            tls.trusted,
            address,
            "localhost",
        )
        .expect("private loopback test endpoint");
        let mut transport = assemble(repository, raw, None).expect("assemble candidate transport");
        let _result = transport.handshake(Service::UploadPack, &[]);
        // The server thread result is intentionally not joined: it is sleeping
        // without a response, and the parent terminates this child process.
        panic!("candidate handshake returned before outer stop; server={server:?}");
    }

    #[test]
    fn outer_deadline_stops_actual_candidate_tls_handshake() {
        assert_outer_deadline_stops_child(
            "tests::candidate_tls_blocked_handshake_child",
            OUTER_CHILD_READY,
        );
    }

    fn assert_outer_deadline_stops_child(exact_test: &'static str, ready_marker: &'static [u8]) {
        let executable = std::env::current_exe().expect("current test executable");
        let outer_deadline = Instant::now() + Duration::from_secs(3);
        let mut child = Command::new(executable)
            .arg("--ignored")
            .arg("--exact")
            .arg(exact_test)
            .arg("--nocapture")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn exact candidate child test");

        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let kill = child.kill();
                let wait = child.wait();
                panic!("child stdout pipe unavailable; kill={kill:?}; wait={wait:?}");
            }
        };
        #[derive(Debug)]
        enum OutputEvent {
            Ready,
            Finished { bytes: usize, failed: bool },
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        let reader = thread::Builder::new()
            .name("candidate-deadline-output".into())
            .spawn(move || {
                let mut stdout = stdout;
                let mut captured = [0u8; 1024];
                let mut used = 0usize;
                let mut matched = false;
                let mut failed = false;
                let mut byte = [0u8; 1];
                while used < captured.len() {
                    match stdout.read(&mut byte) {
                        Ok(0) => break,
                        Ok(_) => {
                            captured[used] = byte[0];
                            used += 1;
                            if !matched && captured[..used].ends_with(ready_marker) {
                                matched = true;
                                let _ = tx.send(OutputEvent::Ready);
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            failed = true;
                            break;
                        }
                    }
                }
                let _ = tx.send(OutputEvent::Finished {
                    bytes: used,
                    failed,
                });
            });

        let reader = match reader {
            Ok(reader) => reader,
            Err(error) => {
                let kill = child.kill();
                let wait = child.wait();
                panic!("spawn bounded output reader: {error}; kill={kill:?}; wait={wait:?}");
            }
        };

        let readiness = rx.recv_timeout(outer_deadline.saturating_duration_since(Instant::now()));
        let ready = matches!(&readiness, Ok(OutputEvent::Ready));
        let before_stop = child.try_wait();
        let remaining = outer_deadline.saturating_duration_since(Instant::now());
        let blocked_through_deadline = ready
            && matches!(&before_stop, Ok(None))
            && !remaining.is_zero()
            && matches!(
                rx.recv_timeout(remaining),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            )
            && matches!(child.try_wait(), Ok(None));

        // Cleanup always precedes evidence assertions so no assertion can
        // unwind while this exact child or its output reader remains live.
        let kill_result = child.kill();
        let wait_result = child.wait();
        let reader_result = reader.join();
        let finished = rx.try_iter().find_map(|event| match event {
            OutputEvent::Finished { bytes, failed } => Some((bytes, failed)),
            OutputEvent::Ready => None,
        });

        assert!(
            ready,
            "child did not validate and report the expected blocked stage: {readiness:?}"
        );
        assert!(
            matches!(before_stop, Ok(None)),
            "child exited before supervision: {before_stop:?}"
        );
        assert!(
            blocked_through_deadline,
            "candidate returned or exited before outer deadline"
        );
        assert!(
            kill_result.is_ok(),
            "killing exact child failed: {kill_result:?}"
        );
        assert!(
            matches!(&wait_result, Ok(status) if !status.success()),
            "child was not reaped with unsuccessful status: {wait_result:?}"
        );
        assert!(reader_result.is_ok(), "bounded output reader did not join");
        assert!(
            matches!(finished, Some((1..=1024, false))),
            "output reader did not finish cleanly within 1024 bytes: {finished:?}"
        );
    }
}

#[cfg(test)]
mod budget_read_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn budget(max: u64) -> Budget {
        Budget {
            max,
            used: Arc::new(Mutex::new(0)),
            cancellation: CancellationHandle::new(),
        }
    }

    fn used(budget: &Budget) -> u64 {
        *budget.used.lock().unwrap()
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[test]
    fn cancellation_after_positive_read_charges_bytes_and_shared_budget_refuses_more() {
        let budget = budget(8);
        let clone = budget.clone();
        let cancellation = budget.cancellation.clone();
        let mut out = [0; 8];
        let result = budget.reserve_read_with(deadline(), &mut out, |slice| {
            slice[..3].copy_from_slice(b"abc");
            cancellation.cancel();
            Ok(3)
        });

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
        assert_eq!(&out[..3], b"abc");
        assert_eq!(used(&budget), 3);

        let called = AtomicUsize::new(0);
        let result = clone.reserve_read_with(deadline(), &mut out, |_| {
            called.fetch_add(1, Ordering::Relaxed);
            Ok(1)
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
        assert_eq!(called.load(Ordering::Relaxed), 0);
        assert_eq!(used(&budget), 3);
    }

    #[test]
    fn callback_is_limited_to_remaining_headroom_and_exact_cap_refuses_next_read() {
        let budget = budget(5);
        *budget.used.lock().unwrap() = 2;
        let mut out = [0; 8];
        let result = budget
            .reserve_read_with(deadline(), &mut out, |slice| {
                assert_eq!(slice.len(), 3);
                slice.copy_from_slice(b"xyz");
                Ok(slice.len())
            })
            .unwrap();
        assert_eq!(result, 3);
        assert_eq!(used(&budget), 5);

        let called = AtomicUsize::new(0);
        let result = budget.reserve_read_with(deadline(), &mut out, |_| {
            called.fetch_add(1, Ordering::Relaxed);
            Ok(0)
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(called.load(Ordering::Relaxed), 0);
        assert_eq!(used(&budget), 5);
    }

    #[test]
    fn pre_cancelled_attempt_does_not_invoke_callback() {
        let budget = budget(4);
        budget.cancellation.cancel();
        let called = AtomicUsize::new(0);
        let result = budget.reserve_read_with(deadline(), &mut [0; 4], |_| {
            called.fetch_add(1, Ordering::Relaxed);
            Ok(1)
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
        assert_eq!(called.load(Ordering::Relaxed), 0);
        assert_eq!(used(&budget), 0);
    }

    #[test]
    fn callback_error_does_not_charge_bytes() {
        let budget = budget(4);
        let result = budget.reserve_read_with(deadline(), &mut [0; 4], |slice| {
            slice[..2].copy_from_slice(b"ab");
            Err(io::Error::new(io::ErrorKind::UnexpectedEof, "fixture read"))
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(used(&budget), 0);
    }

    #[test]
    fn clones_share_the_ingress_cap() {
        let budget = budget(4);
        let clone = budget.clone();
        let mut out = [0; 4];
        budget
            .reserve_read_with(deadline(), &mut out, |slice| Ok(slice.len()))
            .unwrap();

        let called = AtomicUsize::new(0);
        let result = clone.reserve_read_with(deadline(), &mut out, |_| {
            called.fetch_add(1, Ordering::Relaxed);
            Ok(1)
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(called.load(Ordering::Relaxed), 0);
        assert_eq!(used(&budget), 4);
    }

    #[test]
    fn impossible_callback_count_fails_closed_without_charging() {
        let budget = budget(4);
        let result = budget.reserve_read_with(deadline(), &mut [0; 4], |_| Ok(5));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(used(&budget), 0);
    }

    #[test]
    fn expired_deadline_does_not_invoke_callback() {
        let budget = budget(4);
        let called = AtomicUsize::new(0);
        let result = budget.reserve_read_with(Instant::now(), &mut [0; 4], |_| {
            called.fetch_add(1, Ordering::Relaxed);
            Ok(1)
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(called.load(Ordering::Relaxed), 0);
        assert_eq!(used(&budget), 0);
    }
}
