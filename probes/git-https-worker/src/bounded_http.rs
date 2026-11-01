use std::{
    any::Any,
    io::{self, BufRead, Read},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use gix_transport::client::blocking_io::http::{GetResponse, Http, PostBodyDataKind, PostResponse};

const BODY_LABEL: &str = "Git HTTP response body";
const HEADER_LABEL: &str = "Git HTTP response headers";

#[derive(Clone)]
struct SharedBudget {
    label: &'static str,
    limit: u64,
    used: Arc<AtomicU64>,
}

impl SharedBudget {
    fn new(label: &'static str, limit: u64) -> Self {
        Self {
            label,
            limit,
            used: Arc::new(AtomicU64::new(0)),
        }
    }

    fn used(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }

    fn reserve(&self, requested: usize) -> io::Result<usize> {
        let requested = u64::try_from(requested).unwrap_or(u64::MAX);
        let mut used = self.used.load(Ordering::SeqCst);
        loop {
            let remaining = self.limit.saturating_sub(used);
            if remaining == 0 {
                return Err(budget_error(self.label));
            }
            let granted = requested.min(remaining);
            match self.used.compare_exchange(
                used,
                used + granted,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(usize::try_from(granted).unwrap_or(usize::MAX)),
                Err(actual) => used = actual,
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct AttemptBudget {
    body: SharedBudget,
    headers: SharedBudget,
    deadline: Instant,
}

impl AttemptBudget {
    pub(crate) fn new(body_bytes: u64, header_bytes: u64, deadline: Instant) -> Self {
        Self {
            body: SharedBudget::new(BODY_LABEL, body_bytes),
            headers: SharedBudget::new(HEADER_LABEL, header_bytes),
            deadline,
        }
    }

    pub(crate) fn body_bytes_used(&self) -> u64 {
        self.body.used()
    }

    pub(crate) fn header_bytes_used(&self) -> u64 {
        self.headers.used()
    }
}

pub(crate) struct LimitedReader<R> {
    inner: R,
    budget: SharedBudget,
    deadline: Instant,
    unconsumed: usize,
}

impl<R> LimitedReader<R> {
    fn body(inner: R, budget: &AttemptBudget) -> Self {
        Self::new(inner, budget.body.clone(), budget.deadline)
    }

    fn headers(inner: R, budget: &AttemptBudget) -> Self {
        Self::new(inner, budget.headers.clone(), budget.deadline)
    }

    fn new(inner: R, budget: SharedBudget, deadline: Instant) -> Self {
        Self {
            inner,
            budget,
            deadline,
            unconsumed: 0,
        }
    }

    fn check_deadline(deadline: Instant) -> io::Result<()> {
        if Instant::now() >= deadline {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Git HTTP attempt deadline exceeded",
            ))
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: BufRead> BufRead for LimitedReader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let deadline = self.deadline;
        Self::check_deadline(deadline)?;
        if self.unconsumed == 0 && self.budget.used() >= self.budget.limit {
            let available = self.inner.fill_buf()?;
            Self::check_deadline(deadline)?;
            return if available.is_empty() {
                Ok(available)
            } else {
                Err(budget_error(self.budget.label))
            };
        }
        let available = self.inner.fill_buf()?;
        Self::check_deadline(deadline)?;
        if available.is_empty() {
            return Ok(available);
        }
        if self.unconsumed == 0 {
            self.unconsumed = self.budget.reserve(available.len())?;
        }
        Ok(&available[..self.unconsumed.min(available.len())])
    }

    fn consume(&mut self, amount: usize) {
        let consumed = amount.min(self.unconsumed);
        self.inner.consume(consumed);
        self.unconsumed -= consumed;
    }
}

impl<R: BufRead> Read for LimitedReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let input = self.fill_buf()?;
        let amount = input.len().min(out.len());
        out[..amount].copy_from_slice(&input[..amount]);
        self.consume(amount);
        Ok(amount)
    }
}

pub(crate) struct BudgetedHttp<H: Http> {
    inner: H,
    budget: AttemptBudget,
}

impl<H: Http> BudgetedHttp<H> {
    pub(crate) fn new(inner: H, budget: AttemptBudget) -> Self {
        Self { inner, budget }
    }
}

impl<H: Http> Http for BudgetedHttp<H> {
    type Headers = LimitedReader<H::Headers>;
    type ResponseBody = LimitedReader<H::ResponseBody>;
    type PostBody = H::PostBody;

    fn get(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<
        GetResponse<Self::Headers, Self::ResponseBody>,
        gix_transport::client::blocking_io::http::Error,
    > {
        let response = self.inner.get(url, base_url, headers)?;
        Ok(GetResponse {
            headers: LimitedReader::headers(response.headers, &self.budget),
            body: LimitedReader::body(response.body, &self.budget),
        })
    }

    fn post(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        body: PostBodyDataKind,
    ) -> Result<
        PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>,
        gix_transport::client::blocking_io::http::Error,
    > {
        let response = self.inner.post(url, base_url, headers, body)?;
        Ok(PostResponse {
            post_body: response.post_body,
            headers: LimitedReader::headers(response.headers, &self.budget),
            body: LimitedReader::body(response.body, &self.budget),
        })
    }

    fn configure(
        &mut self,
        config: &dyn Any,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.inner.configure(config)
    }
}

fn budget_error(label: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{label} byte budget exceeded"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, Cursor},
        sync::{Arc, Barrier},
        thread,
        time::Duration,
    };

    #[test]
    fn response_boundary_exposes_at_most_the_shared_limit() {
        let budget = AttemptBudget::new(5, 8, Instant::now() + Duration::from_secs(1));
        let mut body = LimitedReader::body(Cursor::new(b"abcdef".to_vec()), &budget);
        assert_eq!(body.fill_buf().unwrap(), b"abcde");
        body.consume(5);
        let error = body.fill_buf().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(budget.body_bytes_used(), 5);
        assert_eq!(body.into_inner().position(), 5);
    }

    #[test]
    fn exact_limit_is_accepted_only_after_confirmed_eof() {
        let budget = AttemptBudget::new(5, 8, Instant::now() + Duration::from_secs(1));
        let mut body = LimitedReader::body(Cursor::new(b"abcde".to_vec()), &budget);
        assert_eq!(body.fill_buf().unwrap(), b"abcde");
        body.consume(2);
        assert_eq!(body.fill_buf().unwrap(), b"cde");
        body.consume(3);
        assert!(body.fill_buf().unwrap().is_empty());
        assert_eq!(budget.body_bytes_used(), 5);
        assert_eq!(body.into_inner().position(), 5);
    }

    #[test]
    fn over_limit_is_rejected_without_exposing_sixth_byte() {
        let budget = AttemptBudget::new(5, 8, Instant::now() + Duration::from_secs(1));
        let mut body = LimitedReader::body(Cursor::new(b"abcdef".to_vec()), &budget);
        assert_eq!(body.fill_buf().unwrap(), b"abcde");
        body.consume(5);
        let error = body.fill_buf().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(budget.body_bytes_used(), 5);
        assert_eq!(body.into_inner().position(), 5);
    }

    #[test]
    fn exact_cap_eof_is_shared_across_response_readers() {
        let budget = AttemptBudget::new(5, 8, Instant::now() + Duration::from_secs(1));
        let mut first = LimitedReader::body(Cursor::new(b"abc".to_vec()), &budget);
        assert_eq!(first.fill_buf().unwrap(), b"abc");
        first.consume(3);
        assert!(first.fill_buf().unwrap().is_empty());
        let mut second = LimitedReader::body(Cursor::new(b"de".to_vec()), &budget);
        assert_eq!(second.fill_buf().unwrap(), b"de");
        second.consume(2);
        assert!(second.fill_buf().unwrap().is_empty());
        assert_eq!(budget.body_bytes_used(), 5);
    }

    #[test]
    fn get_and_post_bodies_share_one_atomic_budget_across_readers() {
        let budget = AttemptBudget::new(7, 8, Instant::now() + Duration::from_secs(1));
        let gate = Arc::new(Barrier::new(3));
        let readers: Vec<_> = [b"abcd".to_vec(), b"efgh".to_vec()]
            .into_iter()
            .map(|bytes| {
                let budget = budget.clone();
                let gate = Arc::clone(&gate);
                thread::spawn(move || {
                    let mut body = LimitedReader::body(Cursor::new(bytes), &budget);
                    gate.wait();
                    let mut exposed = 0;
                    loop {
                        match body.fill_buf() {
                            Ok(bytes) if !bytes.is_empty() => {
                                exposed += bytes.len();
                                let len = bytes.len();
                                body.consume(len);
                            }
                            Ok(_) => break,
                            Err(error) => {
                                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                                break;
                            }
                        }
                    }
                    exposed
                })
            })
            .collect();
        gate.wait();
        let exposed: usize = readers
            .into_iter()
            .map(|reader| reader.join().unwrap())
            .sum();
        assert_eq!(exposed, 7);
        assert_eq!(budget.body_bytes_used(), 7);
    }

    struct SlowReader {
        done: bool,
    }

    impl Read for SlowReader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let bytes = self.fill_buf()?;
            let count = bytes.len().min(out.len());
            out[..count].copy_from_slice(&bytes[..count]);
            self.consume(count);
            Ok(count)
        }
    }

    impl BufRead for SlowReader {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            thread::sleep(Duration::from_millis(50));
            if self.done {
                Ok(&[])
            } else {
                Ok(b"slow")
            }
        }

        fn consume(&mut self, _amount: usize) {
            self.done = true;
        }
    }

    #[test]
    fn slow_read_after_absolute_deadline_exposes_no_late_bytes() {
        let budget = AttemptBudget::new(10, 10, Instant::now() + Duration::from_millis(10));
        let mut body = LimitedReader::body(SlowReader { done: false }, &budget);
        let error = body.fill_buf().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(budget.body_bytes_used(), 0);
    }

    #[test]
    fn header_cap_is_separate_and_shared() {
        let budget = AttemptBudget::new(10, 5, Instant::now() + Duration::from_secs(1));
        let mut headers = LimitedReader::headers(Cursor::new(b"a: b\nc: d".to_vec()), &budget);
        assert_eq!(headers.fill_buf().unwrap(), b"a: b\n");
        headers.consume(5);
        assert_eq!(
            headers.fill_buf().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(budget.header_bytes_used(), 5);
        assert_eq!(budget.body_bytes_used(), 0);
    }
}
