//! Offline demonstration of a shared response-body budget at gix's actual
//! custom HTTP trait. The fixture backend is entirely in memory.
use std::{
    any::Any,
    collections::VecDeque,
    io::{self, BufRead, Cursor, Read},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use gix_error::{ErrorExt, ExnMessageResult, ExnResult};
use gix_transport::client::blocking_io::http::{GetResponse, Http, PostBodyDataKind, PostResponse};

#[derive(Clone, Debug)]
pub struct ByteBudget {
    limit: usize,
    used: Arc<AtomicUsize>,
}

impl ByteBudget {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            used: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Atomically reserve at most `requested` bytes for exposure to a reader.
    fn reserve(&self, requested: usize) -> usize {
        let mut used = self.used.load(Ordering::SeqCst);
        loop {
            let granted = requested.min(self.limit.saturating_sub(used));
            if granted == 0 {
                return 0;
            }
            match self.used.compare_exchange(
                used,
                used + granted,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return granted,
                Err(actual) => used = actual,
            }
        }
    }
}

/// BufRead wrapper with one aggregate byte budget shared across all responses.
/// It counts bytes when exposed by fill_buf, before gix can parse them.
pub struct BoundedBody<R> {
    inner: R,
    budget: ByteBudget,
    unconsumed: usize,
}

impl<R> BoundedBody<R> {
    fn new(inner: R, budget: ByteBudget) -> Self {
        Self {
            inner,
            budget,
            unconsumed: 0,
        }
    }
}

impl<R: BufRead> BufRead for BoundedBody<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.unconsumed > 0 {
            let buf = self.inner.fill_buf()?;
            return Ok(&buf[..self.unconsumed.min(buf.len())]);
        }
        let buf = self.inner.fill_buf()?;
        if buf.is_empty() {
            return Ok(buf);
        }
        let exposed = self.budget.reserve(buf.len());
        if exposed == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "aggregate Git response byte limit {} exceeded",
                    self.budget.limit()
                ),
            ));
        }
        self.unconsumed = exposed;
        Ok(&buf[..exposed])
    }

    fn consume(&mut self, amount: usize) {
        let consumed = amount.min(self.unconsumed);
        self.inner.consume(consumed);
        self.unconsumed -= consumed;
    }
}

impl<R: BufRead> Read for BoundedBody<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let buf = self.fill_buf()?;
        let read = buf.len().min(out.len());
        out[..read].copy_from_slice(&buf[..read]);
        self.consume(read);
        Ok(read)
    }
}

/// Shared-budget decorator implementing the current gix `Http` trait.
pub struct BudgetedHttp<H: Http> {
    inner: H,
    budget: ByteBudget,
}

impl<H: Http> BudgetedHttp<H> {
    pub fn new(inner: H, byte_limit: usize) -> Self {
        Self {
            inner,
            budget: ByteBudget::new(byte_limit),
        }
    }

    pub fn budget(&self) -> ByteBudget {
        self.budget.clone()
    }
}

impl<H: Http> Http for BudgetedHttp<H> {
    type Headers = H::Headers;
    type ResponseBody = BoundedBody<H::ResponseBody>;
    type PostBody = H::PostBody;

    fn get(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> ExnMessageResult<GetResponse<Self::Headers, Self::ResponseBody>> {
        let response = self.inner.get(url, base_url, headers)?;
        Ok(GetResponse {
            headers: response.headers,
            body: BoundedBody::new(response.body, self.budget.clone()),
        })
    }

    fn post(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        body: PostBodyDataKind,
    ) -> ExnMessageResult<PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>> {
        let response = self.inner.post(url, base_url, headers, body)?;
        Ok(PostResponse {
            post_body: response.post_body,
            headers: response.headers,
            body: BoundedBody::new(response.body, self.budget.clone()),
        })
    }

    fn configure(&mut self, config: &dyn Any) -> ExnResult {
        self.inner.configure(config)
    }

    fn redirected_base_url(&self) -> Option<String> {
        self.inner.redirected_base_url()
    }
}

#[derive(Clone, Debug)]
pub struct FixtureResponse {
    pub status: u16,
    pub content_type: Option<&'static str>,
    pub body: Vec<u8>,
}

impl FixtureResponse {
    pub fn ok(content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: Some(content_type),
            body,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Get,
    Post,
}

pub type RequestLog = Arc<Mutex<Vec<RequestKind>>>;

/// In-memory backend that implements gix `Http`, and refuses every non-success
/// status rather than following redirects or retrying authentication.
pub struct FixtureHttp {
    responses: VecDeque<FixtureResponse>,
    requests: RequestLog,
}

impl FixtureHttp {
    pub fn new(responses: impl IntoIterator<Item = FixtureResponse>) -> Self {
        Self {
            responses: responses.into_iter().collect(),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn request_count(&self) -> usize {
        self.requests
            .lock()
            .expect("fixture request log mutex")
            .len()
    }

    pub fn request_log(&self) -> RequestLog {
        Arc::clone(&self.requests)
    }

    pub fn requests(&self) -> Vec<RequestKind> {
        self.requests
            .lock()
            .expect("fixture request log mutex")
            .clone()
    }

    fn next(&mut self, kind: RequestKind) -> ExnMessageResult<FixtureResponse> {
        self.requests
            .lock()
            .expect("fixture request log mutex")
            .push(kind);
        let response = self.responses.pop_front().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "no synthetic HTTP response queued",
            )
            .and_raise(gix_error::message("synthetic fixture exhausted"))
        })?;
        if (300..400).contains(&response.status) {
            return Err(io::Error::other("redirect is disabled in fixture backend")
                .and_raise(gix_error::message("synthetic redirect rejected")));
        }
        if response.status == 401 {
            return Err(
                io::Error::new(io::ErrorKind::PermissionDenied, "synthetic auth required")
                    .and_raise(gix_error::message("synthetic authentication rejected")),
            );
        }
        if !(200..300).contains(&response.status) {
            return Err(io::Error::other("synthetic HTTP error status")
                .and_raise(gix_error::message("synthetic HTTP response rejected")));
        }
        Ok(response)
    }

    fn headers(response: &FixtureResponse) -> Cursor<Vec<u8>> {
        let mut bytes = Vec::new();
        if let Some(content_type) = response.content_type {
            bytes.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
        Cursor::new(bytes)
    }
}

impl Http for FixtureHttp {
    type Headers = Cursor<Vec<u8>>;
    type ResponseBody = Cursor<Vec<u8>>;
    type PostBody = io::Sink;

    fn get(
        &mut self,
        _url: &str,
        _base_url: &str,
        _headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> ExnMessageResult<GetResponse<Self::Headers, Self::ResponseBody>> {
        let response = self.next(RequestKind::Get)?;
        Ok(GetResponse {
            headers: Self::headers(&response),
            body: Cursor::new(response.body),
        })
    }

    fn post(
        &mut self,
        _url: &str,
        _base_url: &str,
        _headers: impl IntoIterator<Item = impl AsRef<str>>,
        _body: PostBodyDataKind,
    ) -> ExnMessageResult<PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>> {
        let response = self.next(RequestKind::Post)?;
        Ok(PostResponse {
            post_body: io::sink(),
            headers: Self::headers(&response),
            body: Cursor::new(response.body),
        })
    }

    fn configure(&mut self, _config: &dyn Any) -> ExnResult {
        Ok(())
    }
}

pub fn packetline(payload: &[u8]) -> Vec<u8> {
    let len = payload.len() + 4;
    let mut out = format!("{len:04x}").into_bytes();
    out.extend_from_slice(payload);
    out
}

pub fn v2_advertisement() -> Vec<u8> {
    let mut out = packetline(b"version 2\n");
    out.extend(packetline(b"agent=synthetic\n"));
    out.extend(packetline(b"ls-refs=unborn\n"));
    out.extend_from_slice(b"0000");
    out
}

pub fn synthetic_pack_response() -> Vec<u8> {
    let mut out = packetline(b"\x01PACK");
    out.extend_from_slice(b"0000");
    out
}

pub fn fixture_status_error(status: u16) -> ExnMessageResult<()> {
    let mut fixture = FixtureHttp::new([FixtureResponse {
        status,
        content_type: None,
        body: Vec::new(),
    }]);
    let _ = fixture.next(RequestKind::Get)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix_transport::client::blocking_io::Transport as _;
    use gix_transport::{
        client::{MessageKind, WriteMode},
        Protocol, Service,
    };

    type GitHttpTransport =
        gix_transport::client::blocking_io::http::Transport<BudgetedHttp<FixtureHttp>>;

    struct SynchronizedBuf {
        cursor: Cursor<Vec<u8>>,
        barrier: Arc<std::sync::Barrier>,
        synchronized: bool,
    }

    impl Read for SynchronizedBuf {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.cursor.read(output)
        }
    }

    impl BufRead for SynchronizedBuf {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if !self.synchronized {
                self.synchronized = true;
                self.barrier.wait();
            }
            self.cursor.fill_buf()
        }

        fn consume(&mut self, amount: usize) {
            self.cursor.consume(amount);
        }
    }

    #[test]
    fn simultaneous_bodies_atomically_share_the_strict_ceiling() {
        let limit = 8;
        let budget = ByteBudget::new(limit);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut first = BoundedBody::new(
            SynchronizedBuf {
                cursor: Cursor::new(vec![b'a'; 8]),
                barrier: Arc::clone(&barrier),
                synchronized: false,
            },
            budget.clone(),
        );
        let mut second = BoundedBody::new(
            SynchronizedBuf {
                cursor: Cursor::new(vec![b'b'; 8]),
                barrier,
                synchronized: false,
            },
            budget.clone(),
        );

        let (first_result, second_result) = std::thread::scope(|scope| {
            let first_thread = scope.spawn(move || first.fill_buf().map(|bytes| bytes.len()));
            let second_thread = scope.spawn(move || second.fill_buf().map(|bytes| bytes.len()));
            (
                first_thread.join().expect("first reader"),
                second_thread.join().expect("second reader"),
            )
        });

        let exposed = first_result.as_ref().copied().unwrap_or(0)
            + second_result.as_ref().copied().unwrap_or(0);
        assert_eq!(exposed, limit, "only the winning reservation is exposed");
        assert_ne!(first_result.is_err(), second_result.is_err());
        let loser = if first_result.is_err() {
            &first_result
        } else {
            &second_result
        };
        assert_eq!(
            loser.as_ref().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            budget.used(),
            limit,
            "aggregate count never exceeds ceiling"
        );
    }

    fn make_transport(
        responses: impl IntoIterator<Item = FixtureResponse>,
        limit: usize,
    ) -> (GitHttpTransport, ByteBudget, RequestLog) {
        let fixture = FixtureHttp::new(responses);
        let requests = fixture.request_log();
        let budgeted = BudgetedHttp::new(fixture, limit);
        let budget = budgeted.budget();
        let url =
            gix_url::parse("https://fixture.invalid/synthetic.git").expect("synthetic URL parses");
        let transport = GitHttpTransport::new_http(budgeted, url, Protocol::V2, false);
        (transport, budget, requests)
    }

    fn responses(advertisement: Vec<u8>, pack: Vec<u8>) -> [FixtureResponse; 2] {
        [
            FixtureResponse::ok("application/x-git-upload-pack-advertisement", advertisement),
            FixtureResponse::ok("application/x-git-upload-pack-result", pack),
        ]
    }

    #[test]
    fn aggregate_budget_covers_info_refs_and_pack_response() {
        let advertisement = v2_advertisement();
        let pack = synthetic_pack_response();
        let expected_total = advertisement.len() + pack.len();
        let (mut transport, budget, requests) =
            make_transport(responses(advertisement, pack), expected_total);

        let result = transport
            .handshake(Service::UploadPack, &[])
            .expect("synthetic refs handshake");
        assert_eq!(result.actual_protocol, Protocol::V2);
        assert_eq!(
            budget.used(),
            expected_total - synthetic_pack_response().len()
        );
        drop(result);

        let request = transport
            .request(WriteMode::Binary, MessageKind::Flush, false)
            .expect("synthetic pack request");
        let mut response = request.into_read().expect("pack reader constructed");
        let mut received = Vec::new();
        response
            .read_to_end(&mut received)
            .expect("in-budget pack response");
        assert_eq!(
            budget.used(),
            expected_total,
            "the budget persists across GET and POST bodies"
        );
        assert_eq!(
            &*requests.lock().expect("request log"),
            &[RequestKind::Get, RequestKind::Post]
        );
    }

    #[test]
    fn aggregate_limit_stops_pack_before_exposing_the_over_limit_byte() {
        let advertisement = v2_advertisement();
        let pack = synthetic_pack_response();
        let limit = advertisement.len() + pack.len() - 1;
        let (mut transport, budget, requests) =
            make_transport(responses(advertisement.clone(), pack.clone()), limit);

        transport
            .handshake(Service::UploadPack, &[])
            .expect("synthetic refs handshake");
        assert_eq!(budget.used(), advertisement.len());
        let request = transport
            .request(WriteMode::Binary, MessageKind::Flush, false)
            .expect("synthetic pack request");
        let mut response = request.into_read().expect("pack reader constructed");
        let error = response
            .read_to_end(&mut Vec::new())
            .expect_err("aggregate budget must abort pack response");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            budget.used(),
            limit,
            "no byte beyond the aggregate ceiling was exposed to gix"
        );
        assert_eq!(
            &*requests.lock().expect("request log"),
            &[RequestKind::Get, RequestKind::Post]
        );
    }

    #[test]
    fn redirect_auth_and_server_errors_remain_failures_without_retry() {
        for status in [302, 401, 500] {
            let fixture_response = FixtureResponse {
                status,
                content_type: None,
                body: Vec::new(),
            };
            let (mut transport, _budget, requests) = make_transport([fixture_response], 128);
            assert!(
                transport.handshake(Service::UploadPack, &[]).is_err(),
                "status {status} must fail"
            );
            assert_eq!(
                requests.lock().expect("request log").as_slice(),
                &[RequestKind::Get]
            );
        }

        // Authentication failure on the POST path also remains an error.
        let mut queued = responses(v2_advertisement(), Vec::new()).to_vec();
        queued[1].status = 401;
        queued[1].content_type = None;
        let (mut transport, _budget, requests) = make_transport(queued, 128);
        transport
            .handshake(Service::UploadPack, &[])
            .expect("synthetic refs handshake");
        assert!(transport
            .request(WriteMode::Binary, MessageKind::Flush, false)
            .is_err());
        assert_eq!(
            requests.lock().expect("request log").as_slice(),
            &[RequestKind::Get, RequestKind::Post]
        );
    }

    #[test]
    fn incorrect_content_type_is_rejected_before_gix_accepts_refs_or_pack() {
        let bad_get = FixtureResponse::ok("text/plain", v2_advertisement());
        let (mut transport, _budget, requests) = make_transport([bad_get], 128);
        assert!(transport.handshake(Service::UploadPack, &[]).is_err());
        assert_eq!(
            requests.lock().expect("request log").as_slice(),
            &[RequestKind::Get]
        );

        let mut queued = responses(v2_advertisement(), synthetic_pack_response()).to_vec();
        queued[1].content_type = Some("text/plain");
        let (mut transport, _budget, requests) = make_transport(queued, 128);
        transport
            .handshake(Service::UploadPack, &[])
            .expect("synthetic refs handshake");
        let request = transport
            .request(WriteMode::Binary, MessageKind::Flush, false)
            .expect("request built");
        let mut response = request.into_read().expect("response reader built");
        assert!(response.read_to_end(&mut Vec::new()).is_err());
        assert_eq!(
            requests.lock().expect("request log").as_slice(),
            &[RequestKind::Get, RequestKind::Post]
        );
    }
}
