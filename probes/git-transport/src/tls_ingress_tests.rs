//! Local rustls-backed gix transport proof. The byte counter sits on the raw
//! loopback socket, below rustls, and charges bytes returned to TLS.
use std::{
    any::Any,
    io::{self, BufRead, Cursor, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use gix_error::{ErrorExt, ExnMessageResult, ExnResult};
use gix_transport::client::blocking_io::http::{GetResponse, Http, PostBodyDataKind, PostResponse};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};

const IO_SLICE_TIMEOUT: Duration = Duration::from_millis(100);
const HTTP_HEADER_LIMIT: usize = 8192;
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
        request.push_str("\r\n");
        stream.write_all(request.as_bytes())?;

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
    type Headers = Cursor<Vec<u8>>;
    type ResponseBody = TlsBody;
    type PostBody = io::Sink;

    fn get(
        &mut self,
        _url: &str,
        _base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> ExnMessageResult<GetResponse<Self::Headers, Self::ResponseBody>> {
        let (headers, body) = self
            .request("GET", "/info/refs?service=git-upload-pack", headers)
            .map_err(tls_error)?;
        Ok(GetResponse { headers, body })
    }

    fn post(
        &mut self,
        _url: &str,
        _base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        _body: PostBodyDataKind,
    ) -> ExnMessageResult<PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>> {
        let (headers, body) = self
            .request("POST", "/git-upload-pack", headers)
            .map_err(tls_error)?;
        Ok(PostResponse {
            post_body: io::sink(),
            headers,
            body,
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

fn serve_gix_responses(
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
    requests: usize,
) -> thread::JoinHandle<usize> {
    thread::spawn(move || {
        let deadline = Instant::now() + FIXTURE_TIMEOUT;
        let mut completed = 0;
        for index in 0..requests {
            let Ok(socket) = server_accept(&listener, deadline) else {
                return completed;
            };
            let socket = CountedSocket {
                stream: socket,
                budget: WireBudget::new(usize::MAX),
                deadline,
            };
            let connection = match rustls::ServerConnection::new(Arc::clone(&config)) {
                Ok(connection) => connection,
                Err(_) => return completed,
            };
            let mut stream = rustls::StreamOwned::new(connection, socket);
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                if Instant::now() >= deadline || request.len() >= HTTP_HEADER_LIMIT {
                    return completed;
                }
                match stream.read(&mut byte) {
                    Ok(0) => return completed,
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
                    Err(_) => return completed,
                }
            }
            let expected_path: &[u8] = if index == 0 {
                b"GET /info/refs?service=git-upload-pack HTTP/1.1\r\n"
            } else {
                b"POST /git-upload-pack HTTP/1.1\r\n"
            };
            if !request.starts_with(expected_path) {
                return completed;
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
                return completed;
            }
            completed += 1;
        }
        completed
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
    let Ok(request) = transport.request(WriteMode::Binary, MessageKind::Flush, false) else {
        return (false, budget.used());
    };
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
    let server = serve_gix_responses(listener, Arc::clone(&fixture.server_config), 2);
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
    assert_eq!(served, 2, "TLS fixture should serve GET and POST");
    assert!(
        ingress > plaintext_len,
        "raw TLS ingress should include TLS records as well as response payloads"
    );
}

#[test]
fn tls_ingress_budget_stops_at_limit_without_lookahead() {
    let fixture = tls_fixture();
    let (listener, address) = loopback_listener();
    let server = serve_gix_responses(listener, Arc::clone(&fixture.server_config), 1);
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
    let server = serve_gix_responses(listener, Arc::clone(&fixture.server_config), 1);
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
    let server = serve_gix_responses(listener, Arc::clone(&fixture.server_config), 1);
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
