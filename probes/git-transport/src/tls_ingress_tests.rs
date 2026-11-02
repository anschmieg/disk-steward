//! Local rustls-backed gix transport proof. The byte counter sits on the raw
//! loopback socket, below rustls, and charges bytes returned to TLS.
use std::{
    any::Any,
    io::{self, BufRead, Cursor, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    sync::{
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use gix_error::{ErrorExt, ExnMessageResult, ExnResult};
use gix_transport::client::blocking_io::http::{GetResponse, Http, PostBodyDataKind, PostResponse};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};

const IO_SLICE_TIMEOUT: Duration = Duration::from_millis(100);
const HTTP_HEADER_LIMIT: usize = 8192;
const POST_BODY_LIMIT: usize = 8192;
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct WireBudget {
    limit: usize,
    used: Arc<Mutex<usize>>,
}

impl WireBudget {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            used: Arc::new(Mutex::new(0)),
        }
    }

    fn used(&self) -> usize {
        *self.used.lock().expect("TLS wire budget mutex")
    }
}

/// Raw socket adapter immediately below rustls. Counts bytes only when the
/// socket read returns them to rustls, before TLS can buffer or decrypt them.
struct CountedSocket {
    stream: TcpStream,
    budget: WireBudget,
    deadline: Instant,
}

impl CountedSocket {
    fn timeout(&self) -> io::Result<Duration> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "synthetic TLS attempt deadline expired",
            ))
        } else {
            Ok(remaining.min(IO_SLICE_TIMEOUT))
        }
    }
}

impl Read for CountedSocket {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let mut used = self
            .budget
            .used
            .lock()
            .map_err(|_| io::Error::other("TLS wire budget unavailable"))?;
        let remaining = self.budget.limit.saturating_sub(*used);
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "synthetic TLS ingress limit reached",
            ));
        }
        let allowed = output.len().min(remaining);
        loop {
            self.stream.set_read_timeout(Some(self.timeout()?))?;
            match self.stream.read(&mut output[..allowed]) {
                Ok(count) => {
                    *used = (*used).saturating_add(count);
                    drop(used);
                    // Charge a successful late read, then fail closed before rustls sees it.
                    self.timeout()?;
                    return Ok(count);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) && Instant::now() < self.deadline =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Write for CountedSocket {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        loop {
            self.stream.set_write_timeout(Some(self.timeout()?))?;
            match self.stream.write(input) {
                Ok(count) => {
                    self.timeout()?;
                    return Ok(count);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) && Instant::now() < self.deadline =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.timeout()?;
        self.stream.flush()?;
        self.timeout()?;
        Ok(())
    }
}

struct TlsBody {
    stream: rustls::StreamOwned<rustls::ClientConnection, CountedSocket>,
    remaining: usize,
    byte: [u8; 1],
    available: bool,
}

impl BufRead for TlsBody {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.stream.sock.timeout()?;
        if !self.available && self.remaining > 0 {
            let count = self.stream.read(&mut self.byte)?;
            self.stream.sock.timeout()?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "synthetic TLS response body truncated",
                ));
            }
            self.remaining -= count;
            self.available = true;
        }
        if self.available {
            Ok(&self.byte)
        } else {
            Ok(&[])
        }
    }

    fn consume(&mut self, amount: usize) {
        if amount > 0 {
            self.available = false;
        }
    }
}

impl Read for TlsBody {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let bytes = self.fill_buf()?;
        if bytes.is_empty() {
            return Ok(0);
        }
        output[0] = bytes[0];
        self.consume(1);
        Ok(1)
    }
}

enum PostPayload {
    Body(Vec<u8>),
    TooLarge,
}

struct TlsPostBody {
    sender: SyncSender<PostPayload>,
    buffered: Vec<u8>,
    failed: bool,
}

impl Write for TlsPostBody {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if self.failed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "synthetic TLS POST body already failed",
            ));
        }
        if self.buffered.len().saturating_add(input.len()) > POST_BODY_LIMIT {
            self.failed = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "synthetic TLS POST body limit exceeded",
            ));
        }
        self.buffered.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.failed {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "synthetic TLS POST body failed",
            ))
        } else {
            Ok(())
        }
    }
}

impl Drop for TlsPostBody {
    fn drop(&mut self) {
        let payload = if self.failed {
            PostPayload::TooLarge
        } else {
            PostPayload::Body(std::mem::take(&mut self.buffered))
        };
        // The bounded one-slot channel is empty until this unique writer drops.
        // try_send keeps Drop nonblocking on every path.
        let _ = self.sender.try_send(payload);
    }
}

struct InitializedPost {
    headers: Vec<u8>,
    body: Option<TlsBody>,
}

struct TlsPostState {
    receiver: Receiver<PostPayload>,
    requester: TlsHttp,
    request_headers: Vec<String>,
    initialized: Option<InitializedPost>,
    failed: bool,
}

impl TlsPostState {
    fn initialize(&mut self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other("synthetic TLS POST initialization failed"));
        }
        if self.initialized.is_some() {
            return Ok(());
        }
        let remaining = self
            .requester
            .deadline
            .saturating_duration_since(Instant::now());
        let payload = match self.receiver.recv_timeout(remaining) {
            Ok(PostPayload::Body(payload)) => payload,
            Ok(PostPayload::TooLarge) => {
                self.failed = true;
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "synthetic TLS POST body limit exceeded",
                ));
            }
            Err(_) => {
                self.failed = true;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "synthetic TLS POST body unavailable before deadline",
                ));
            }
        };
        let request_headers = self.request_headers.clone();
        match self.requester.request_with_body(
            "POST",
            "/git-upload-pack",
            request_headers.iter(),
            Some(&payload),
        ) {
            Ok((headers, body)) => {
                self.initialized = Some(InitializedPost {
                    headers: headers.into_inner(),
                    body: Some(body),
                });
                Ok(())
            }
            Err(_) => {
                self.failed = true;
                Err(io::Error::other("synthetic TLS POST transport failed"))
            }
        }
    }

    fn response_headers(&mut self) -> io::Result<Vec<u8>> {
        self.initialize()?;
        self.initialized
            .as_ref()
            .map(|response| response.headers.clone())
            .ok_or_else(|| io::Error::other("synthetic TLS POST headers unavailable"))
    }

    fn take_response_body(&mut self) -> io::Result<TlsBody> {
        self.initialize()?;
        self.initialized
            .as_mut()
            .and_then(|response| response.body.take())
            .ok_or_else(|| io::Error::other("synthetic TLS POST body unavailable"))
    }
}

struct TlsPostHeaders {
    state: Arc<Mutex<TlsPostState>>,
    cursor: Option<Cursor<Vec<u8>>>,
}

enum TlsResponseHeaders {
    Immediate(Cursor<Vec<u8>>),
    Post(TlsPostHeaders),
}

impl BufRead for TlsResponseHeaders {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        match self {
            Self::Immediate(cursor) => cursor.fill_buf(),
            Self::Post(headers) => headers.fill_buf(),
        }
    }

    fn consume(&mut self, amount: usize) {
        match self {
            Self::Immediate(cursor) => cursor.consume(amount),
            Self::Post(headers) => headers.consume(amount),
        }
    }
}

impl Read for TlsResponseHeaders {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let bytes = self.fill_buf()?;
        if bytes.is_empty() {
            return Ok(0);
        }
        output[0] = bytes[0];
        self.consume(1);
        Ok(1)
    }
}

impl BufRead for TlsPostHeaders {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.cursor.is_none() {
            let headers = self
                .state
                .lock()
                .map_err(|_| io::Error::other("synthetic TLS POST state unavailable"))?
                .response_headers()?;
            self.cursor = Some(Cursor::new(headers));
        }
        self.cursor
            .as_mut()
            .ok_or_else(|| io::Error::other("synthetic TLS POST headers unavailable"))?
            .fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        if let Some(cursor) = &mut self.cursor {
            cursor.consume(amount);
        }
    }
}

impl Read for TlsPostHeaders {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let bytes = self.fill_buf()?;
        if bytes.is_empty() {
            return Ok(0);
        }
        output[0] = bytes[0];
        self.consume(1);
        Ok(1)
    }
}

struct TlsPostResponseBody {
    state: Arc<Mutex<TlsPostState>>,
    body: Option<TlsBody>,
}

enum TlsResponseBody {
    Immediate(TlsBody),
    Post(TlsPostResponseBody),
}

impl BufRead for TlsResponseBody {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        match self {
            Self::Immediate(body) => body.fill_buf(),
            Self::Post(body) => body.fill_buf(),
        }
    }

    fn consume(&mut self, amount: usize) {
        match self {
            Self::Immediate(body) => body.consume(amount),
            Self::Post(body) => body.consume(amount),
        }
    }
}

impl Read for TlsResponseBody {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let bytes = self.fill_buf()?;
        if bytes.is_empty() {
            return Ok(0);
        }
        output[0] = bytes[0];
        self.consume(1);
        Ok(1)
    }
}

impl BufRead for TlsPostResponseBody {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.body.is_none() {
            self.body = Some(
                self.state
                    .lock()
                    .map_err(|_| io::Error::other("synthetic TLS POST state unavailable"))?
                    .take_response_body()?,
            );
        }
        self.body
            .as_mut()
            .ok_or_else(|| io::Error::other("synthetic TLS POST body unavailable"))?
            .fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        if let Some(body) = &mut self.body {
            body.consume(amount);
        }
    }
}

impl Read for TlsPostResponseBody {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let bytes = self.fill_buf()?;
        if bytes.is_empty() {
            return Ok(0);
        }
        output[0] = bytes[0];
        self.consume(1);
        Ok(1)
    }
}

struct TlsHttp {
    endpoint: SocketAddr,
    server_name: &'static str,
    config: Arc<rustls::ClientConfig>,
    budget: WireBudget,
    deadline: Instant,
}

impl TlsHttp {
    fn new(
        endpoint: SocketAddr,
        server_name: &'static str,
        config: Arc<rustls::ClientConfig>,
        limit: usize,
        timeout: Duration,
    ) -> Self {
        assert_eq!(endpoint.ip(), std::net::IpAddr::V4(Ipv4Addr::LOCALHOST));
        Self {
            endpoint,
            server_name,
            config,
            budget: WireBudget::new(limit),
            deadline: Instant::now() + timeout,
        }
    }

    fn request(
        &mut self,
        method: &str,
        path: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> io::Result<(Cursor<Vec<u8>>, TlsBody)> {
        self.request_with_body(method, path, headers, None)
    }

    fn request_with_body(
        &mut self,
        method: &str,
        path: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        body: Option<&[u8]>,
    ) -> io::Result<(Cursor<Vec<u8>>, TlsBody)> {
        let timeout = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(IO_SLICE_TIMEOUT);
        if timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "synthetic TLS attempt deadline expired",
            ));
        }
        let stream = TcpStream::connect_timeout(&self.endpoint, timeout)?;
        let socket = CountedSocket {
            stream,
            budget: self.budget.clone(),
            deadline: self.deadline,
        };
        let name = ServerName::try_from(self.server_name.to_owned())
            .map_err(|_| io::Error::other("invalid synthetic TLS server name"))?;
        let connection = rustls::ClientConnection::new(Arc::clone(&self.config), name)
            .map_err(|_| io::Error::other("synthetic TLS client setup failed"))?;
        let mut stream = rustls::StreamOwned::new(connection, socket);

        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            self.server_name
        );
        for header in headers {
            let header = header.as_ref();
            if header.contains(['\r', '\n']) {
                return Err(io::Error::other("invalid synthetic request header"));
            }
            request.push_str(header);
            request.push_str("\r\n");
        }
        if let Some(body) = body {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes())?;
        if let Some(body) = body {
            stream.write_all(body)?;
            stream.flush()?;
        }

        let mut response_headers = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            if Instant::now() >= self.deadline || response_headers.len() >= HTTP_HEADER_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "synthetic TLS response header bound reached",
                ));
            }
            stream.read_exact(&mut byte)?;
            response_headers.push(byte[0]);
            if response_headers.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        stream.sock.timeout()?;
        let parsed = parse_tls_headers(&response_headers)?;
        if !(200..300).contains(&parsed.status)
            || parsed.transfer_encoding
            || parsed.content_encoding.is_some()
        {
            return Err(io::Error::other("synthetic TLS HTTP response rejected"));
        }
        let content_type = parsed
            .content_type
            .ok_or_else(|| io::Error::other("synthetic TLS response lacks content type"))?;
        let headers = format!(
            "Content-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
            parsed.content_length
        )
        .into_bytes();
        Ok((
            Cursor::new(headers),
            TlsBody {
                stream,
                remaining: parsed.content_length,
                byte: [0],
                available: false,
            },
        ))
    }
}

impl Http for TlsHttp {
    type Headers = TlsResponseHeaders;
    type ResponseBody = TlsResponseBody;
    type PostBody = TlsPostBody;

    fn get(
        &mut self,
        _url: &str,
        _base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> ExnMessageResult<GetResponse<Self::Headers, Self::ResponseBody>> {
        let (headers, body) = self
            .request("GET", "/info/refs?service=git-upload-pack", headers)
            .map_err(tls_error)?;
        Ok(GetResponse {
            headers: TlsResponseHeaders::Immediate(headers),
            body: TlsResponseBody::Immediate(body),
        })
    }

    fn post(
        &mut self,
        _url: &str,
        _base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        _body: PostBodyDataKind,
    ) -> ExnMessageResult<PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>> {
        let request_headers = headers
            .into_iter()
            .map(|header| header.as_ref().to_owned())
            .collect::<Vec<_>>();
        let (sender, receiver) = sync_channel(1);
        let state = Arc::new(Mutex::new(TlsPostState {
            receiver,
            requester: TlsHttp {
                endpoint: self.endpoint,
                server_name: self.server_name,
                config: Arc::clone(&self.config),
                budget: self.budget.clone(),
                deadline: self.deadline,
            },
            request_headers,
            initialized: None,
            failed: false,
        }));
        Ok(PostResponse {
            post_body: TlsPostBody {
                sender,
                buffered: Vec::new(),
                failed: false,
            },
            headers: TlsResponseHeaders::Post(TlsPostHeaders {
                state: Arc::clone(&state),
                cursor: None,
            }),
            body: TlsResponseBody::Post(TlsPostResponseBody { state, body: None }),
        })
    }

    fn configure(&mut self, _config: &dyn Any) -> ExnResult {
        Ok(())
    }
}

fn tls_error(_: io::Error) -> gix_error::Exn<gix_error::Message> {
    io::Error::other("synthetic TLS transport failed")
        .and_raise(gix_error::message("synthetic TLS transport failure"))
}

struct ParsedTlsHeaders {
    status: u16,
    content_length: usize,
    content_type: Option<String>,
    content_encoding: Option<String>,
    transfer_encoding: bool,
}

fn parse_tls_headers(bytes: &[u8]) -> io::Result<ParsedTlsHeaders> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| io::Error::other("invalid TLS HTTP headers"))?;
    let mut lines = text.split("\r\n");
    let mut status_parts = lines.next().unwrap_or_default().split_ascii_whitespace();
    if status_parts.next() != Some("HTTP/1.1") {
        return Err(io::Error::other("invalid TLS HTTP status line"));
    }
    let status = status_parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| io::Error::other("invalid TLS HTTP status"))?;
    let mut content_length = None;
    let mut content_type = None;
    let mut content_encoding = None;
    let mut transfer_encoding = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("invalid TLS HTTP header"))?;
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "content-length" if content_length.is_none() => {
                content_length = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| io::Error::other("invalid TLS HTTP content length"))?,
                );
            }
            "content-length" => return Err(io::Error::other("duplicate TLS content length")),
            "content-type" if content_type.is_none() => content_type = Some(value.to_owned()),
            "content-type" => return Err(io::Error::other("duplicate TLS content type")),
            "content-encoding" => content_encoding = Some(value.to_owned()),
            "transfer-encoding" => transfer_encoding = true,
            _ => {}
        }
    }
    Ok(ParsedTlsHeaders {
        status,
        content_length: content_length
            .ok_or_else(|| io::Error::other("missing TLS HTTP content length"))?,
        content_type,
        content_encoding,
        transfer_encoding,
    })
}

struct TlsFixture {
    server_config: Arc<rustls::ServerConfig>,
    trusted_client_config: Arc<rustls::ClientConfig>,
}

fn tls_fixture() -> TlsFixture {
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .expect("generate in-memory localhost certificate");
    let certificate = generated.cert.der().clone();
    let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        generated.signing_key.serialize_der(),
    ));
    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], private_key)
        .expect("construct local TLS server config");
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(certificate.clone())
        .expect("trust generated fixture certificate");
    let trusted_client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsFixture {
        server_config: Arc::new(server_config),
        trusted_client_config: Arc::new(trusted_client_config),
    }
}

fn loopback_listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind TLS fixture loopback");
    listener
        .set_nonblocking(true)
        .expect("set bounded TLS fixture accept");
    let address = listener.local_addr().expect("get TLS fixture address");
    (listener, address)
}

fn server_accept(listener: &TcpListener, deadline: Instant) -> io::Result<TcpStream> {
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TLS fixture accept deadline expired",
            ));
        }
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error),
        }
    }
}

#[derive(Default)]
struct ServerObservation {
    completed: usize,
    post_body_matches: Option<bool>,
    post_body_nonempty: Option<bool>,
}

fn request_content_length(headers: &[u8]) -> io::Result<usize> {
    let text = std::str::from_utf8(headers)
        .map_err(|_| io::Error::other("invalid synthetic request headers"))?;
    let mut length = None;
    for line in text.split("\r\n").skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                if length.is_some() {
                    return Err(io::Error::other("duplicate synthetic content length"));
                }
                length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| io::Error::other("invalid synthetic content length"))?,
                );
            }
        }
    }
    let length = length.unwrap_or(0);
    if length > POST_BODY_LIMIT {
        return Err(io::Error::other(
            "synthetic request body exceeds fixture bound",
        ));
    }
    Ok(length)
}

fn serve_gix_responses(
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
    requests: usize,
    expected_post_body: Option<Vec<u8>>,
) -> thread::JoinHandle<ServerObservation> {
    thread::spawn(move || {
        let deadline = Instant::now() + FIXTURE_TIMEOUT;
        let mut observation = ServerObservation::default();
        for index in 0..requests {
            let Ok(socket) = server_accept(&listener, deadline) else {
                return observation;
            };
            let socket = CountedSocket {
                stream: socket,
                budget: WireBudget::new(usize::MAX),
                deadline,
            };
            let connection = match rustls::ServerConnection::new(Arc::clone(&config)) {
                Ok(connection) => connection,
                Err(_) => return observation,
            };
            let mut stream = rustls::StreamOwned::new(connection, socket);
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                if Instant::now() >= deadline || request.len() >= HTTP_HEADER_LIMIT {
                    return observation;
                }
                match stream.read(&mut byte) {
                    Ok(0) => return observation,
                    Ok(_) => {
                        request.push(byte[0]);
                        if request.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) && Instant::now() < deadline =>
                    {
                        continue;
                    }
                    Err(_) => return observation,
                }
            }
            let body_length = match request_content_length(&request) {
                Ok(length) => length,
                Err(_) => return observation,
            };
            let mut request_body = vec![0u8; body_length];
            if stream.read_exact(&mut request_body).is_err() {
                return observation;
            }
            let expected_path: &[u8] = if index == 0 {
                b"GET /info/refs?service=git-upload-pack HTTP/1.1\r\n"
            } else {
                b"POST /git-upload-pack HTTP/1.1\r\n"
            };
            if !request.starts_with(expected_path) {
                return observation;
            }
            if index == 1 {
                observation.post_body_nonempty = Some(!request_body.is_empty());
                observation.post_body_matches = expected_post_body
                    .as_ref()
                    .map(|expected| expected.as_slice() == request_body.as_slice());
                if observation.post_body_nonempty != Some(true)
                    || observation.post_body_matches != Some(true)
                {
                    return observation;
                }
            }
            let body = if index == 0 {
                crate::http::v2_advertisement()
            } else {
                crate::http::synthetic_pack_response()
            };
            let kind = if index == 0 {
                "advertisement"
            } else {
                "result"
            };
            let reply = format!(
                "HTTP/1.1 200 Synthetic\r\nContent-Type: application/x-git-upload-pack-{kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            if write_tls_until(&mut stream, reply.as_bytes(), deadline).is_err()
                || write_tls_until(&mut stream, &body, deadline).is_err()
            {
                return observation;
            }
            observation.completed += 1;
        }
        observation
    })
}

fn write_tls_until(
    stream: &mut rustls::StreamOwned<rustls::ServerConnection, CountedSocket>,
    mut bytes: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TLS fixture write deadline expired",
            ));
        }
        let written = stream.write(bytes)?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "TLS fixture accepted no response bytes",
            ));
        }
        bytes = &bytes[written..];
    }
    stream.flush()?;
    Ok(())
}

fn run_gix_transfer(
    address: SocketAddr,
    client_config: Arc<rustls::ClientConfig>,
    limit: usize,
    server_name: &'static str,
) -> (bool, usize) {
    use gix_transport::{
        client::blocking_io::Transport as _,
        client::{MessageKind, WriteMode},
        Protocol, Service,
    };

    let http = TlsHttp::new(address, server_name, client_config, limit, FIXTURE_TIMEOUT);
    let budget = http.budget.clone();
    let url = format!("https://{server_name}/synthetic.git");
    let Ok(url) = gix_url::parse(&url) else {
        return (false, budget.used());
    };
    let mut transport = gix_transport::client::blocking_io::http::Transport::new_http(
        http,
        url,
        Protocol::V2,
        false,
    );
    if transport.handshake(Service::UploadPack, &[]).is_err() {
        return (false, budget.used());
    }
    let Ok(mut request) = transport.request(WriteMode::Binary, MessageKind::Flush, false) else {
        return (false, budget.used());
    };
    if request.write_all(b"synthetic-command\n").is_err() {
        return (false, budget.used());
    }
    let Ok(mut body) = request.into_read() else {
        return (false, budget.used());
    };
    let success = body.read_to_end(&mut Vec::new()).is_ok();
    (success, budget.used())
}

fn serve_tls_handshake(
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let deadline = Instant::now() + FIXTURE_TIMEOUT;
        let Ok(socket) = server_accept(&listener, deadline) else {
            return;
        };
        let mut socket = CountedSocket {
            stream: socket,
            budget: WireBudget::new(usize::MAX),
            deadline,
        };
        let Ok(mut connection) = rustls::ServerConnection::new(config) else {
            return;
        };
        while connection.is_handshaking() && Instant::now() < deadline {
            if connection.complete_io(&mut socket).is_err() {
                return;
            }
        }
    })
}

fn client_certificate_verification_error(
    address: SocketAddr,
    client_config: Arc<rustls::ClientConfig>,
    server_name: &'static str,
) -> bool {
    let Ok(stream) = TcpStream::connect_timeout(&address, FIXTURE_TIMEOUT.min(IO_SLICE_TIMEOUT))
    else {
        return false;
    };
    let mut socket = CountedSocket {
        stream,
        budget: WireBudget::new(64 * 1024),
        deadline: Instant::now() + FIXTURE_TIMEOUT,
    };
    let Ok(server_name) = ServerName::try_from(server_name.to_owned()) else {
        return false;
    };
    let Ok(mut connection) = rustls::ClientConnection::new(client_config, server_name) else {
        return false;
    };
    connection.complete_io(&mut socket).is_err_and(|error| {
        error
            .get_ref()
            .and_then(|source| source.downcast_ref::<rustls::Error>())
            .is_some_and(|source| matches!(source, rustls::Error::InvalidCertificate(_)))
    })
}

#[test]
fn actual_gix_get_and_post_are_counted_below_rustls() {
    let fixture = tls_fixture();
    let (listener, address) = loopback_listener();
    let mut expected_post_body = crate::http::packetline(b"synthetic-command\n");
    expected_post_body.extend_from_slice(b"0000");
    let server = serve_gix_responses(
        listener,
        Arc::clone(&fixture.server_config),
        2,
        Some(expected_post_body),
    );
    let advertisement = crate::http::v2_advertisement();
    let pack = crate::http::synthetic_pack_response();
    let plaintext_len = advertisement.len() + pack.len();
    let (success, ingress) = run_gix_transfer(
        address,
        Arc::clone(&fixture.trusted_client_config),
        64 * 1024,
        "localhost",
    );
    let served = server.join().expect("bounded TLS fixture server joined");
    assert!(success, "trusted synthetic gix transfer should complete");
    assert_eq!(served.completed, 2, "TLS fixture should serve GET and POST");
    assert_eq!(
        served.post_body_matches,
        Some(true),
        "POST packet body mismatch"
    );
    assert_eq!(
        served.post_body_nonempty,
        Some(true),
        "POST body should be nonempty"
    );
    assert!(
        ingress > plaintext_len,
        "raw TLS ingress should include TLS records as well as response payloads"
    );
}

#[test]
fn oversized_gix_post_body_is_rejected_before_second_connection() {
    use gix_transport::{
        client::blocking_io::Transport as _,
        client::{MessageKind, WriteMode},
        Protocol, Service,
    };

    let fixture = tls_fixture();
    let (listener, address) = loopback_listener();
    let mut expected_post_body = crate::http::packetline(b"synthetic-command\n");
    expected_post_body.extend_from_slice(b"0000");
    let server = serve_gix_responses(
        listener,
        Arc::clone(&fixture.server_config),
        2,
        Some(expected_post_body),
    );
    let http = TlsHttp::new(
        address,
        "localhost",
        Arc::clone(&fixture.trusted_client_config),
        64 * 1024,
        FIXTURE_TIMEOUT,
    );
    let budget = http.budget.clone();
    let url = gix_url::parse("https://localhost/synthetic.git").expect("fixture URL");
    let mut transport = gix_transport::client::blocking_io::http::Transport::new_http(
        http,
        url,
        Protocol::V2,
        false,
    );
    let handshake_ok = transport.handshake(Service::UploadPack, &[]).is_ok();
    let mut post_rejected = false;
    if handshake_ok {
        if let Ok(mut request) = transport.request(WriteMode::Binary, MessageKind::Flush, false) {
            let oversized_write_rejected =
                request.write_all(&vec![b'x'; POST_BODY_LIMIT + 1]).is_err();
            let into_read_rejected = request.into_read().is_err();
            post_rejected = oversized_write_rejected && into_read_rejected;
        }
    }
    let ingress = budget.used();
    drop(transport);
    let observation = server
        .join()
        .expect("bounded oversize fixture server joined");
    assert!(handshake_ok, "synthetic TLS GET handshake should succeed");
    assert!(post_rejected, "oversized request body should fail closed");
    assert_eq!(
        observation.completed, 1,
        "only the initial GET may reach the loopback server"
    );
    assert_eq!(
        observation.post_body_matches, None,
        "oversized POST reached server"
    );
    assert!(ingress > 0, "GET response bytes should remain accounted");
}

#[test]
fn ignored_oversize_write_poison_reaches_lazy_headers_without_connecting() {
    let (listener, address) = loopback_listener();
    let fixture = tls_fixture();
    let mut http = TlsHttp::new(
        address,
        "localhost",
        fixture.trusted_client_config,
        4096,
        FIXTURE_TIMEOUT,
    );
    let PostResponse {
        mut post_body,
        mut headers,
        body,
    } = match http.post(
        "https://localhost/synthetic.git/git-upload-pack",
        "https://localhost/synthetic.git",
        std::iter::empty::<&str>(),
        PostBodyDataKind::Unbounded,
    ) {
        Ok(response) => response,
        Err(_) => panic!("synthetic POST setup should succeed"),
    };
    assert!(post_body
        .write_all(&vec![b'x'; POST_BODY_LIMIT + 1])
        .is_err());
    assert!(
        post_body.buffered.is_empty(),
        "oversize input must not enter the buffer"
    );
    assert!(
        post_body.write_all(b"partial").is_err(),
        "poison must persist"
    );
    drop(post_body);
    assert!(
        headers.fill_buf().is_err(),
        "ignored upload error must reach the response"
    );
    assert!(
        headers.fill_buf().is_err(),
        "failed initialization must not retry"
    );
    assert!(matches!(listener.accept(), Err(ref error)
        if error.kind() == io::ErrorKind::WouldBlock));
    drop(body);
}

#[test]
fn dropping_empty_post_body_is_nonblocking_and_starts_no_connection() {
    let (listener, address) = loopback_listener();
    let fixture = tls_fixture();
    let mut http = TlsHttp::new(
        address,
        "localhost",
        fixture.trusted_client_config,
        4096,
        FIXTURE_TIMEOUT,
    );
    let start = Instant::now();
    let response = http.post(
        "https://localhost/synthetic.git/git-upload-pack",
        "https://localhost/synthetic.git",
        std::iter::empty::<&str>(),
        PostBodyDataKind::Unbounded,
    );
    let response = match response {
        Ok(response) => response,
        Err(_) => panic!("synthetic POST setup should succeed"),
    };
    let PostResponse {
        post_body,
        headers,
        body,
    } = response;
    drop(post_body);
    drop(headers);
    drop(body);
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "dropping fixture writer blocked"
    );
    assert!(matches!(
        listener.accept(),
        Err(ref error) if error.kind() == io::ErrorKind::WouldBlock
    ));
}

#[test]
fn held_post_writer_times_out_before_lazy_headers_open_a_connection() {
    let (listener, address) = loopback_listener();
    let fixture = tls_fixture();
    let mut http = TlsHttp::new(
        address,
        "localhost",
        fixture.trusted_client_config,
        4096,
        Duration::from_millis(120),
    );
    let response = http.post(
        "https://localhost/synthetic.git/git-upload-pack",
        "https://localhost/synthetic.git",
        std::iter::empty::<&str>(),
        PostBodyDataKind::Unbounded,
    );
    let PostResponse {
        post_body,
        mut headers,
        body,
    } = match response {
        Ok(response) => response,
        Err(_) => panic!("synthetic POST setup should succeed"),
    };
    let started = Instant::now();
    let header_result = headers.fill_buf();
    let elapsed = started.elapsed();
    assert!(
        header_result.is_err(),
        "lazy header read should honor deadline"
    );
    assert!(
        elapsed >= Duration::from_millis(110) && elapsed < Duration::from_secs(1),
        "lazy header receive should be bounded by its attempt deadline"
    );
    assert!(matches!(
        listener.accept(),
        Err(ref error) if error.kind() == io::ErrorKind::WouldBlock
    ));
    drop(post_body);
    drop(headers);
    drop(body);
}

#[test]
fn tls_ingress_budget_stops_at_limit_without_lookahead() {
    let fixture = tls_fixture();
    let (listener, address) = loopback_listener();
    let server = serve_gix_responses(listener, Arc::clone(&fixture.server_config), 1, None);
    let limit = 256;
    let (success, ingress) = run_gix_transfer(
        address,
        Arc::clone(&fixture.trusted_client_config),
        limit,
        "localhost",
    );
    let _served = server.join().expect("bounded TLS fixture server joined");
    assert!(!success, "TLS transfer crossing the byte cap must fail");
    assert_eq!(ingress, limit, "the raw socket reader must stop at the cap");
}

#[test]
fn rustls_rejects_untrusted_certificate_and_wrong_hostname() {
    let fixture = tls_fixture();
    let (listener, address) = loopback_listener();
    let server = serve_gix_responses(listener, Arc::clone(&fixture.server_config), 1, None);
    let empty_roots = rustls::RootCertStore::empty();
    let untrusted_config = Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(empty_roots)
            .with_no_client_auth(),
    );
    let (untrusted_success, _) =
        run_gix_transfer(address, untrusted_config, 64 * 1024, "localhost");
    let _ = server.join().expect("bounded untrusted TLS fixture joined");
    assert!(
        !untrusted_success,
        "untrusted fixture certificate must fail"
    );

    let (listener, address) = loopback_listener();
    let server = serve_gix_responses(listener, Arc::clone(&fixture.server_config), 1, None);
    let (wrong_name_success, _) = run_gix_transfer(
        address,
        Arc::clone(&fixture.trusted_client_config),
        64 * 1024,
        "wrong.localhost",
    );
    let _ = server
        .join()
        .expect("bounded wrong-name TLS fixture joined");
    assert!(
        !wrong_name_success,
        "wrong TLS hostname must fail verification"
    );

    let (listener, address) = loopback_listener();
    let server = serve_tls_handshake(listener, Arc::clone(&fixture.server_config));
    let untrusted_rejection = client_certificate_verification_error(
        address,
        Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
        ),
        "localhost",
    );
    server
        .join()
        .expect("bounded untrusted TLS handshake joined");
    assert!(
        untrusted_rejection,
        "untrusted certificate must produce a rustls certificate validation error"
    );

    let (listener, address) = loopback_listener();
    let server = serve_tls_handshake(listener, Arc::clone(&fixture.server_config));
    let wrong_name_rejection = client_certificate_verification_error(
        address,
        Arc::clone(&fixture.trusted_client_config),
        "wrong.localhost",
    );
    server
        .join()
        .expect("bounded wrong-name TLS handshake joined");
    assert!(
        wrong_name_rejection,
        "wrong hostname must produce a rustls certificate validation error"
    );
}

#[test]
fn stalled_tls_peer_obeys_raw_socket_attempt_deadline() {
    let fixture = tls_fixture();
    let (listener, address) = loopback_listener();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + FIXTURE_TIMEOUT;
        if let Ok(_socket) = server_accept(&listener, deadline) {
            thread::sleep(Duration::from_millis(300));
        }
    });
    let started = Instant::now();
    let mut http = TlsHttp::new(
        address,
        "localhost",
        Arc::clone(&fixture.trusted_client_config),
        64 * 1024,
        Duration::from_millis(120),
    );
    let result = http.get(
        "https://localhost/synthetic.git/info/refs?service=git-upload-pack",
        "https://localhost/synthetic.git",
        std::iter::empty::<&str>(),
    );
    let elapsed = started.elapsed();
    let server_joined = server.join().is_ok();
    assert!(result.is_err(), "stalled TLS peer must fail");
    assert!(server_joined, "bounded stalled peer fixture must join");
    assert!(
        elapsed >= Duration::from_millis(110) && elapsed < Duration::from_secs(1),
        "raw socket deadline should bound the stalled TLS handshake"
    );
}
