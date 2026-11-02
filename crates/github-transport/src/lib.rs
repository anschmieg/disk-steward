//! A narrowly scoped gix GitHub HTTPS transport candidate.
//!
//! The constructor validates input without connecting. DNS uses the platform
//! resolver and cannot be interrupted here; callers need an outer worker
//! deadline. See the crate README for the current scope and limits.

#[cfg(not(test))]
use std::net::ToSocketAddrs;
use std::{
    any::Any,
    borrow::Cow,
    fmt,
    io::{self, BufRead, Cursor, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::{mpsc, Arc, Mutex},
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
const MAX_HEADERS: usize = 8192;
const IO_SLICE: Duration = Duration::from_millis(100);

/// Per-attempt limits. Values may only lower the documented defaults.
#[derive(Clone, Copy, Debug)]
pub struct AttemptLimits {
    ingress_bytes: u64,
    timeout: Duration,
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
    let config = Arc::new(client_config()?);
    #[cfg(test)]
    let http = RawHttp::new(repository.clone(), limits, config, None);
    #[cfg(not(test))]
    let http = RawHttp::new(repository.clone(), limits, config);
    assemble(repository, http, credential)
}

fn assemble(
    repository: RepositoryUrl,
    http: RawHttp,
    credential: Option<AppCredential>,
) -> io::Result<GitHubTransport> {
    let budget = http.budget.clone();
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
    Ok(GitHubTransport { inner, budget })
}

/// gix's blocking HTTP transport, ready for an explicit handshake.
pub struct GitHubTransport {
    inner: gix_transport::client::blocking_io::http::Transport<RawHttp>,
    budget: Budget,
}

impl GitHubTransport {
    /// Socket bytes observed beneath TLS, including incomplete attempts.
    pub fn observed_ingress_bytes(&self) -> io::Result<u64> {
        self.budget
            .used
            .lock()
            .map(|used| *used)
            .map_err(|_| safe_error(io::ErrorKind::Other, "transport unavailable"))
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
}
impl Budget {
    fn reserve_read(
        &self,
        stream: &mut TcpStream,
        deadline: Instant,
        out: &mut [u8],
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
        let count = read_until(stream, deadline, &mut out[..n])?;
        *used = used.saturating_add(count as u64);
        check_deadline(deadline)?;
        Ok(count)
    }
}

struct CountedSocket {
    stream: TcpStream,
    budget: Budget,
    deadline: Instant,
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
fn read_until(stream: &mut TcpStream, deadline: Instant, out: &mut [u8]) -> io::Result<usize> {
    loop {
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
            result => return result,
        }
    }
}
fn write_until(stream: &mut TcpStream, deadline: Instant, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
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
            Ok(n) => data = &data[n..],
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
            Err(_) => return Err(safe_error(io::ErrorKind::Other, "transport write failed")),
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
        write_until(&mut self.stream, self.deadline, b)?;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        check_deadline(self.deadline)?;
        self.stream
            .flush()
            .map_err(|_| safe_error(io::ErrorKind::Other, "transport write failed"))
    }
}

struct Body {
    stream: rustls::StreamOwned<rustls::ClientConnection, CountedSocket>,
    left: usize,
    byte: [u8; 1],
    ready: bool,
}
impl BufRead for Body {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        check_deadline(self.stream.sock.deadline)?;
        if !self.ready && self.left > 0 {
            let n = self
                .stream
                .read(&mut self.byte)
                .map_err(|_| safe_error(io::ErrorKind::Other, "response read failed"))?;
            if n == 0 {
                return Err(safe_error(
                    io::ErrorKind::UnexpectedEof,
                    "response truncated",
                ));
            }
            self.left -= n;
            self.ready = true;
        }
        if self.ready {
            Ok(&self.byte)
        } else {
            Ok(&[])
        }
    }
    fn consume(&mut self, n: usize) {
        if n > 0 {
            self.ready = false;
        }
    }
}
impl Read for Body {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let b = self.fill_buf()?;
        if b.is_empty() {
            return Ok(0);
        }
        out[0] = b[0];
        self.consume(1);
        Ok(1)
    }
}

struct RawHttp {
    repository: RepositoryUrl,
    limits: AttemptLimits,
    deadline: Instant,
    budget: Budget,
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
        #[cfg(test)] fixture: Option<(SocketAddr, Arc<str>)>,
    ) -> Self {
        Self {
            repository,
            limits,
            deadline: Instant::now() + limits.timeout,
            budget: Budget {
                max: limits.ingress_bytes,
                used: Arc::new(Mutex::new(0)),
            },
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
            Some((endpoint, Arc::from(server_name))),
        ))
    }
    fn connect_socket(&self) -> io::Result<CountedSocket> {
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
        let mut last = None;
        for addr in endpoint {
            check_deadline(self.deadline)?;
            match TcpStream::connect_timeout(
                &addr,
                self.deadline.saturating_duration_since(Instant::now()),
            ) {
                Ok(stream) => {
                    return Ok(CountedSocket {
                        stream,
                        budget: self.budget.clone(),
                        deadline: self.deadline,
                    })
                }
                Err(_) => last = Some(()),
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
    ) -> io::Result<(Cursor<Vec<u8>>, Body)> {
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
            .map_err(|_| safe_error(io::ErrorKind::Other, "request write failed"))?;
        if let Some(b) = body {
            tls.write_all(&b)
                .map_err(|_| safe_error(io::ErrorKind::Other, "request write failed"))?;
            tls.flush()
                .map_err(|_| safe_error(io::ErrorKind::Other, "request write failed"))?;
        }
        tls.flush()
            .map_err(|_| safe_error(io::ErrorKind::Other, "request write failed"))?;
        let mut raw = Vec::new();
        let mut byte = [0];
        while !raw.ends_with(b"\r\n\r\n") {
            if raw.len() >= MAX_HEADERS {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "response headers exceed limit",
                ));
            }
            tls.read_exact(&mut byte)
                .map_err(|_| safe_error(io::ErrorKind::Other, "response headers unavailable"))?;
            raw.push(byte[0]);
        }
        let parsed = parse_headers(&raw)?;
        if parsed.status == 401 {
            return Err(safe_error(
                io::ErrorKind::PermissionDenied,
                "GitHub authentication required",
            ));
        }
        if !(200..300).contains(&parsed.status) {
            return Err(safe_error(io::ErrorKind::Other, "HTTP response rejected"));
        }
        let length = parsed
            .length
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "missing response length"))?;
        let content_type = parsed
            .content_type
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "missing response type"))?;
        let h = format!(
            "Content-Type: {}\r\nContent-Length: {}\r\n\r\n",
            content_type, length
        )
        .into_bytes();
        Ok((
            Cursor::new(h),
            Body {
                stream: tls,
                left: length,
                byte: [0],
                ready: false,
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
}
impl Write for LazyPost {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
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
    body: Option<Body>,
    pending: Option<Arc<Mutex<PostState>>>,
}
struct PostState {
    receiver: mpsc::Receiver<Option<Vec<u8>>>,
    http: RawHttp,
    request_headers: Vec<String>,
    headers: Option<Cursor<Vec<u8>>>,
    body: Option<Body>,
    failed: Option<io::ErrorKind>,
}
impl PostState {
    fn init(&mut self) -> io::Result<()> {
        if self.headers.is_some() {
            return Ok(());
        }
        if let Some(kind) = self.failed {
            return Err(safe_error(kind, "POST failed"));
        }
        let data = match self
            .receiver
            .recv_timeout(self.http.deadline.saturating_duration_since(Instant::now()))
        {
            Ok(Some(data)) => data,
            Ok(None) => {
                self.failed = Some(io::ErrorKind::InvalidData);
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "POST body limit exceeded",
                ));
            }
            Err(_) => {
                self.failed = Some(io::ErrorKind::TimedOut);
                return Err(safe_error(io::ErrorKind::TimedOut, "POST unavailable"));
            }
        };
        let result = self.http.request(
            "POST",
            "/git-upload-pack",
            std::mem::take(&mut self.request_headers),
            Some(data),
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
            let (h, b) = self.request("GET", "/info/refs?service=git-upload-pack", hs, None)?;
            Ok(GetResponse {
                headers: Headers {
                    cursor: h,
                    pending: None,
                },
                body: Response {
                    body: Some(b),
                    pending: None,
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
            let (tx, rx) = mpsc::sync_channel(1);
            let state = Arc::new(Mutex::new(PostState {
                receiver: rx,
                http: RawHttp {
                    repository: self.repository.clone(),
                    limits: self.limits,
                    deadline: self.deadline,
                    budget: self.budget.clone(),
                    config: Arc::clone(&self.config),
                    #[cfg(test)]
                    fixture: self.fixture.clone(),
                },
                request_headers: hs,
                headers: None,
                body: None,
                failed: None,
            }));
            Ok(PostResponse {
                post_body: LazyPost {
                    sender: tx,
                    data: Vec::new(),
                    failed: false,
                },
                headers: Headers {
                    cursor: Cursor::new(Vec::new()),
                    pending: Some(Arc::clone(&state)),
                },
                body: Response {
                    body: None,
                    pending: Some(state),
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
    let (mut length, mut ctype) = (None, None);
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
            "transfer-encoding" | "content-encoding" | "location" => {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "response framing rejected",
                ))
            }
            _ => {}
        }
    }
    if code == 200 && (length.is_none() || ctype.is_none()) {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "incomplete response headers",
        ));
    }
    Ok(Parsed {
        status: code,
        length,
        content_type: ctype,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix_transport::bstr::ByteSlice as _;
    use gix_transport::client::blocking_io::Transport as _;
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::{
        net::{Ipv4Addr, TcpListener},
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
            let n = read_until(&mut self.stream, self.deadline, &mut out[..allowed])?;
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
        }
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
        let error = match http.request(
            "GET",
            "/info/refs?service=git-upload-pack",
            Vec::new(),
            None,
        ) {
            Ok(_) => panic!("401 must fail"),
            Err(error) => error,
        };
        assert!(server.join().is_ok());
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}
