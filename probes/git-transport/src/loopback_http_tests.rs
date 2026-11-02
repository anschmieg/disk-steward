//! Test-only HTTP/1.1 socket fixture. It binds only IPv4 loopback and has no
//! URL resolver, TLS, credential helper, redirect handling, or production API.
use std::{
    any::Any,
    io::{self, BufRead, Cursor, Read, Write},
    net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use gix_error::{ErrorExt, ExnMessageResult, ExnResult};
use gix_transport::client::blocking_io::http::{GetResponse, Http, PostBodyDataKind, PostResponse};

const HEADER_LIMIT: usize = 8192;
const IO_SLICE_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone)]
struct SharedBudget {
    limit: usize,
    used: Arc<Mutex<usize>>,
}

impl SharedBudget {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            used: Arc::new(Mutex::new(0)),
        }
    }

    fn used(&self) -> usize {
        *self.used.lock().expect("budget mutex")
    }
}

struct SocketBody {
    stream: TcpStream,
    remaining_body: usize,
    budget: SharedBudget,
    deadline: Instant,
    byte: [u8; 1],
    available: bool,
}

impl SocketBody {
    fn read_one(&mut self) -> io::Result<()> {
        if self.remaining_body == 0 {
            return Ok(());
        }
        if self.available {
            return Ok(());
        }
        let count = read_counted(
            &mut self.stream,
            &self.budget,
            self.deadline,
            &mut self.byte,
        )?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated HTTP response body",
            ));
        }
        self.remaining_body -= count;
        self.available = true;
        Ok(())
    }
}

impl Read for SocketBody {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || (self.remaining_body == 0 && !self.available) {
            return Ok(0);
        }
        let mut read = 0;
        while read < output.len() && (self.remaining_body > 0 || self.available) {
            let bytes = self.fill_buf()?;
            if bytes.is_empty() {
                break;
            }
            output[read] = bytes[0];
            self.consume(1);
            read += 1;
        }
        Ok(read)
    }
}

impl io::BufRead for SocketBody {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if !self.available {
            self.read_one()?;
        }
        if self.available {
            Ok(&self.byte)
        } else {
            Ok(&[])
        }
    }

    fn consume(&mut self, amount: usize) {
        if amount > 0 && self.available {
            self.available = false;
        }
    }
}

/// Reads one byte at a time so headers and bodies share the same exact budget,
/// without an uncounted `BufReader` below the accounting boundary.
struct SocketHttp {
    endpoint: SocketAddr,
    budget: SharedBudget,
    deadline: Instant,
}

impl SocketHttp {
    fn new(endpoint: SocketAddr, byte_limit: usize, timeout: Duration) -> Self {
        assert_eq!(endpoint.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        Self {
            endpoint,
            budget: SharedBudget::new(byte_limit),
            deadline: Instant::now() + timeout,
        }
    }

    fn connect(&self) -> io::Result<TcpStream> {
        let timeout = remaining_timeout(self.deadline)?.min(IO_SLICE_TIMEOUT);
        let stream = TcpStream::connect_timeout(&self.endpoint, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(stream)
    }

    fn request(
        &mut self,
        method: &str,
        path: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> ExnMessageResult<(Cursor<Vec<u8>>, SocketBody)> {
        let mut stream = self
            .connect()
            .map_err(|_| socket_failure("connect failed"))?;
        let mut request =
            format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
        for header in headers {
            let header = header.as_ref();
            if header.contains(['\r', '\n']) {
                return Err(socket_failure("invalid request header"));
            }
            request.push_str(header);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        write_all_until(&mut stream, request.as_bytes(), self.deadline)
            .map_err(|_| socket_failure("request write failed"))?;
        check_deadline(self.deadline).map_err(|_| socket_failure("attempt deadline exceeded"))?;

        let mut header_bytes = Vec::new();
        let mut line = Vec::new();
        loop {
            line.clear();
            loop {
                let next = read_counted_byte(&mut stream, &self.budget, self.deadline)
                    .map_err(|_| socket_failure("response header read failed"))?;
                line.push(next);
                if line.len() > HEADER_LIMIT || header_bytes.len() + line.len() > HEADER_LIMIT {
                    return Err(socket_failure("response headers exceed limit"));
                }
                if line.ends_with(b"\r\n") {
                    break;
                }
            }
            header_bytes.extend_from_slice(&line);
            if header_bytes.len() >= 4 && header_bytes.ends_with(b"\r\n\r\n") {
                break;
            }
        }

        let parsed =
            parse_headers(&header_bytes).map_err(|_| socket_failure("invalid response headers"))?;
        match parsed.status {
            200..=299 => {}
            401 => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "authentication required",
                )
                .and_raise(gix_error::message("synthetic HTTP authentication required")))
            }
            300..=399 => return Err(socket_failure("redirect rejected")),
            _ => return Err(socket_failure("HTTP status rejected")),
        }
        if parsed
            .content_encoding
            .as_deref()
            .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
        {
            return Err(socket_failure("unsupported content encoding"));
        }
        if parsed.transfer_encoding {
            return Err(socket_failure("transfer encoding is unsupported"));
        }
        if parsed.content_type.is_none() {
            return Err(socket_failure("missing content type"));
        }
        let synthetic_headers = format!(
            "Content-Type: {}\r\nContent-Length: {}\r\n\r\n",
            parsed.content_type.as_deref().unwrap_or_default(),
            parsed.content_length
        )
        .into_bytes();
        let body = SocketBody {
            stream,
            remaining_body: parsed.content_length,
            budget: self.budget.clone(),
            deadline: self.deadline,
            byte: [0],
            available: false,
        };
        Ok((Cursor::new(synthetic_headers), body))
    }
}

impl Http for SocketHttp {
    type Headers = Cursor<Vec<u8>>;
    type ResponseBody = SocketBody;
    type PostBody = io::Sink;

    fn get(
        &mut self,
        _url: &str,
        _base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> ExnMessageResult<GetResponse<Self::Headers, Self::ResponseBody>> {
        let (headers, body) = self.request("GET", "/info/refs?service=git-upload-pack", headers)?;
        Ok(GetResponse { headers, body })
    }

    fn post(
        &mut self,
        _url: &str,
        _base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        _body: PostBodyDataKind,
    ) -> ExnMessageResult<PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>> {
        let (headers, body) = self.request("POST", "/git-upload-pack", headers)?;
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

struct ParsedHeaders {
    status: u16,
    content_length: usize,
    content_type: Option<String>,
    content_encoding: Option<String>,
    transfer_encoding: bool,
}

fn parse_headers(bytes: &[u8]) -> io::Result<ParsedHeaders> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| io::Error::other("invalid HTTP header bytes"))?;
    let mut lines = text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| io::Error::other("missing status"))?;
    let mut status_parts = status_line.split_ascii_whitespace();
    if status_parts.next() != Some("HTTP/1.1") {
        return Err(io::Error::other("unsupported status line"));
    }
    let status = status_parts
        .next()
        .and_then(|part| part.parse::<u16>().ok())
        .ok_or_else(|| io::Error::other("invalid status"))?;
    let (mut content_length, mut content_type, mut content_encoding, mut transfer_encoding) =
        (None, None, None, false);
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("malformed header"))?;
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => {
                if content_length.is_some() {
                    return Err(io::Error::other("duplicate content length"));
                }
                content_length = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| io::Error::other("invalid content length"))?,
                );
            }
            "content-type" => {
                if content_type.is_some() {
                    return Err(io::Error::other("duplicate content type"));
                }
                content_type = Some(value.to_owned());
            }
            "content-encoding" => {
                if content_encoding.is_some() {
                    return Err(io::Error::other("duplicate content encoding"));
                }
                content_encoding = Some(value.to_owned());
            }
            "transfer-encoding" => transfer_encoding = true,
            _ => {}
        }
    }
    Ok(ParsedHeaders {
        status,
        content_length: content_length.ok_or_else(|| io::Error::other("missing content length"))?,
        content_type,
        content_encoding,
        transfer_encoding,
    })
}

fn read_counted(
    stream: &mut TcpStream,
    budget: &SharedBudget,
    deadline: Instant,
    output: &mut [u8],
) -> io::Result<usize> {
    read_counted_with(budget, deadline, output, |limited_output| {
        let timeout = remaining_timeout(deadline)?.min(IO_SLICE_TIMEOUT);
        stream.set_read_timeout(Some(timeout))?;
        stream.read(limited_output)
    })
}

fn read_counted_with<F>(
    budget: &SharedBudget,
    deadline: Instant,
    output: &mut [u8],
    mut read: F,
) -> io::Result<usize>
where
    F: FnMut(&mut [u8]) -> io::Result<usize>,
{
    if output.is_empty() {
        return Ok(0);
    }
    check_deadline(deadline)?;
    let mut used = budget.used.lock().expect("budget mutex");
    let remaining = budget.limit.saturating_sub(*used);
    if remaining == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "aggregate HTTP response byte limit exceeded",
        ));
    }
    let permitted = output.len().min(remaining);
    let count = read(&mut output[..permitted])?;
    if count > permitted {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "reader returned more bytes than requested",
        ));
    }
    *used += count;
    // Charge successful late reads before reporting a deadline failure.
    check_deadline(deadline)?;
    Ok(count)
}

fn read_counted_byte(
    stream: &mut TcpStream,
    budget: &SharedBudget,
    deadline: Instant,
) -> io::Result<u8> {
    let mut byte = [0u8; 1];
    let count = read_counted(stream, budget, deadline, &mut byte)?;
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated HTTP headers",
        ));
    }
    Ok(byte[0])
}

fn write_all_until(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        let timeout = remaining_timeout(deadline)?.min(IO_SLICE_TIMEOUT);
        stream.set_write_timeout(Some(timeout))?;
        let written = stream.write(bytes)?;
        check_deadline(deadline)?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "socket accepted no request bytes",
            ));
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

fn remaining_timeout(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "attempt deadline exceeded",
        ))
    } else {
        Ok(remaining)
    }
}

fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "attempt deadline exceeded",
        ))
    } else {
        Ok(())
    }
}

fn socket_failure(message: &'static str) -> gix_error::Exn<gix_error::Message> {
    io::Error::other(message).and_raise(gix_error::message("synthetic loopback HTTP failure"))
}

fn response(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
    format!("HTTP/1.1 {status} Synthetic\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
        .into_bytes()
        .into_iter()
        .chain(body.iter().copied())
        .collect()
}

fn serve(
    listener: TcpListener,
    replies: Vec<Vec<u8>>,
    drip: Option<Duration>,
) -> thread::JoinHandle<()> {
    listener
        .set_nonblocking(true)
        .expect("fixture listener mode");
    thread::spawn(move || {
        let server_deadline = Instant::now() + Duration::from_secs(2);
        for reply in replies {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < server_deadline =>
                    {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(_) => return,
                }
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(IO_SLICE_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_SLICE_TIMEOUT));
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            if let Some(delay) = drip {
                for byte in reply {
                    if Instant::now() >= server_deadline || stream.write_all(&[byte]).is_err() {
                        break;
                    }
                    thread::sleep(delay);
                }
            } else {
                let _ = stream.write_all(&reply);
            }
            let _ = stream.shutdown(Shutdown::Both);
        }
    })
}

fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    (listener, address)
}

#[test]
fn loopback_http_counts_headers_and_bodies_across_get_and_post() {
    let advertisement = crate::http::v2_advertisement();
    let pack = crate::http::synthetic_pack_response();
    let first = response(
        200,
        "application/x-git-upload-pack-advertisement",
        &advertisement,
    );
    let second = response(200, "application/x-git-upload-pack-result", &pack);
    let total = first.len() + second.len();
    let (server, address) = listener();
    let server_thread = serve(server, vec![first, second], None);
    let client = SocketHttp::new(address, total, Duration::from_secs(2));
    let budget = client.budget.clone();

    // Use actual gix HTTP transport and consume both responses.
    use gix_transport::{
        client::blocking_io::Transport as _,
        client::{MessageKind, WriteMode},
        Protocol, Service,
    };
    let url = gix_url::parse("https://fixture.invalid/synthetic.git").unwrap();
    let mut transport = gix_transport::client::blocking_io::http::Transport::new_http(
        client,
        url,
        Protocol::V2,
        false,
    );
    transport
        .handshake(Service::UploadPack, &[])
        .expect("GET handshake");
    let request = transport
        .request(WriteMode::Binary, MessageKind::Flush, false)
        .expect("POST request");
    let mut body = request.into_read().expect("POST response body");
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes).expect("bounded POST body");
    drop(body);
    assert_eq!(
        budget.used(),
        total,
        "status, headers, and bodies are counted together"
    );
    server_thread.join().expect("server thread");
}

#[test]
fn loopback_budget_stops_before_reading_over_limit_across_requests() {
    let advertisement = crate::http::v2_advertisement();
    let pack = crate::http::synthetic_pack_response();
    let first = response(
        200,
        "application/x-git-upload-pack-advertisement",
        &advertisement,
    );
    let second = response(200, "application/x-git-upload-pack-result", &pack);
    let total = first.len() + second.len();
    let (server, address) = listener();
    let server_thread = serve(server, vec![first, second], None);
    let url = gix_url::parse("https://fixture.invalid/synthetic.git").unwrap();
    let client = SocketHttp::new(address, total - 1, Duration::from_secs(2));
    let budget = client.budget.clone();
    let mut transport = gix_transport::client::blocking_io::http::Transport::new_http(
        client,
        url,
        gix_transport::Protocol::V2,
        false,
    );
    use gix_transport::{
        client::blocking_io::Transport as _,
        client::{MessageKind, WriteMode},
        Service,
    };
    transport
        .handshake(Service::UploadPack, &[])
        .expect("GET handshake fits");
    let request = transport
        .request(WriteMode::Binary, MessageKind::Flush, false)
        .expect("POST request");
    let mut body = request.into_read().expect("POST response body");
    assert!(body.read_to_end(&mut Vec::new()).is_err());
    drop(body);
    assert_eq!(budget.used(), total - 1);
    server_thread.join().expect("server thread");
}

#[test]
fn loopback_rejects_redirect_without_contacting_location() {
    let (redirected, redirected_addr) = listener();
    redirected
        .set_nonblocking(true)
        .expect("nonblocking redirect listener");
    let (server, address) = listener();
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://{redirected_addr}/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .into_bytes();
    let server_thread = serve(server, vec![redirect], None);
    let mut client = SocketHttp::new(address, 2048, Duration::from_secs(2));
    let result = client.request(
        "GET",
        "/info/refs?service=git-upload-pack",
        std::iter::empty::<&str>(),
    );
    assert!(result.is_err());
    assert!(
        matches!(redirected.accept(), Err(ref error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    server_thread.join().expect("server thread");
}

#[test]
fn loopback_rejects_auth_required_and_server_error_statuses() {
    for status in [401, 500] {
        let (server, address) = listener();
        let reply = format!(
            "HTTP/1.1 {status} Synthetic\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_bytes();
        let server_thread = serve(server, vec![reply], None);
        let mut client = SocketHttp::new(address, 2048, Duration::from_secs(2));
        let result = client.request("GET", "/", std::iter::empty::<&str>());
        assert!(
            result.is_err(),
            "HTTP {status} must remain a failed request"
        );
        server_thread.join().expect("status server thread");
    }
}

#[test]
fn loopback_deadline_covers_slow_drip_and_malformed_headers_fail_closed() {
    let (server, address) = listener();
    let reply = response(200, "application/x-git-upload-pack-advertisement", &[]);
    let server_thread = serve(server, vec![reply], Some(Duration::from_millis(8)));
    let mut client = SocketHttp::new(address, 2048, Duration::from_millis(60));
    let started = Instant::now();
    assert!(client
        .request("GET", "/", std::iter::empty::<&str>())
        .is_err());
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "deadline should bound drip wait"
    );
    server_thread.join().expect("slow server thread");

    let (server, address) = listener();
    let malformed =
        b"HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\n\r\n"
            .to_vec();
    let server_thread = serve(server, vec![malformed], None);
    let mut client = SocketHttp::new(address, 2048, Duration::from_secs(2));
    assert!(client
        .request("GET", "/", std::iter::empty::<&str>())
        .is_err());
    server_thread.join().expect("malformed server thread");

    for malformed in [
        b"HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: 0\r\nContent-Encoding: gzip\r\n\r\n".to_vec(),
    ] {
        let (server, address) = listener();
        let server_thread = serve(server, vec![malformed], None);
        let mut client = SocketHttp::new(address, 2048, Duration::from_secs(2));
        assert!(client
            .request("GET", "/", std::iter::empty::<&str>())
            .is_err());
        server_thread.join().expect("malformed server thread");
    }
}

#[test]
fn socket_body_preserves_a_buffered_last_byte_for_read() {
    let (listener, address) = listener();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept body reader");
        stream.write_all(b"x").expect("write body byte");
    });
    let stream = TcpStream::connect(address).expect("connect body reader");
    let mut body = SocketBody {
        stream,
        remaining_body: 1,
        budget: SharedBudget::new(1),
        deadline: Instant::now() + Duration::from_secs(2),
        byte: [0],
        available: false,
    };

    assert_eq!(body.fill_buf().expect("fill last byte"), b"x");
    let mut byte = [0];
    assert_eq!(body.read(&mut byte).expect("read buffered last byte"), 1);
    assert_eq!(byte, *b"x");
    assert_eq!(body.read(&mut byte).expect("EOF after last byte"), 0);
    server.join().expect("body server thread");
}

#[test]
fn successful_read_finishing_after_deadline_is_still_charged() {
    let budget = SharedBudget::new(2);
    let deadline = Instant::now() + Duration::from_millis(20);
    let mut output = [0];
    let result = read_counted_with(&budget, deadline, &mut output, |limited| {
        thread::sleep(Duration::from_millis(40));
        limited[0] = b'z';
        Ok(1)
    });

    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_eq!(budget.used(), 1, "late successful read remains charged");
}

#[test]
fn concurrent_socket_reads_serialize_the_shared_byte_budget() {
    let budget = SharedBudget::new(1);
    let deadline = Instant::now() + Duration::from_secs(2);
    let barrier = Arc::new(std::sync::Barrier::new(3));

    let (first, second) = thread::scope(|scope| {
        let first_barrier = Arc::clone(&barrier);
        let first_budget = budget.clone();
        let first = scope.spawn(move || {
            let mut source = Cursor::new(b"a".to_vec());
            let mut output = [0];
            first_barrier.wait();
            read_counted_with(&first_budget, deadline, &mut output, |limited| {
                source.read(limited)
            })
        });

        let second_barrier = Arc::clone(&barrier);
        let second_budget = budget.clone();
        let second = scope.spawn(move || {
            let mut source = Cursor::new(b"b".to_vec());
            let mut output = [0];
            second_barrier.wait();
            read_counted_with(&second_budget, deadline, &mut output, |limited| {
                source.read(limited)
            })
        });

        barrier.wait();
        (
            first.join().expect("first socket reader"),
            second.join().expect("second socket reader"),
        )
    });

    let count = first.as_ref().copied().unwrap_or(0) + second.as_ref().copied().unwrap_or(0);
    assert_eq!(count, 1);
    assert_ne!(first.is_err(), second.is_err());
    let failed = if first.is_err() { first } else { second };
    assert_eq!(failed.unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert_eq!(budget.used(), 1, "serialized reads cannot overrun the cap");
}
