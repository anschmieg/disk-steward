//! Standalone development proof of a byte-limited reader at gix's blocking
//! transport boundary. Input is synthetic and entirely in memory.
use std::{
    any::Any,
    borrow::Cow,
    io::{self, Cursor, Read},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use gix_transport::{
    bstr::{BStr, ByteSlice},
    client::{
        blocking_io::{
            ExtendedBufRead, HandleProgress, RequestWriter, SetServiceResponse, Transport,
        },
        Capabilities, TransportWithoutIO,
    },
    packetline::{blocking_io::StreamingPeekableIter, read::ProgressAction, PacketLineRef},
    Protocol, Service,
};

#[derive(Debug)]
pub struct ByteLimitExceeded {
    pub limit: usize,
}

impl std::fmt::Display for ByteLimitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "synthetic transport byte limit of {} exceeded",
            self.limit
        )
    }
}

impl std::error::Error for ByteLimitExceeded {}

/// A strict reader cap: once the cap has been returned, another read errors
/// instead of fetching even one lookahead byte. `consumed` tracks bytes exposed
/// through this adapter, not TLS, HTTP headers, or lower-layer socket prefetch.
pub struct BoundedRead<R> {
    inner: R,
    limit: usize,
    consumed: Arc<AtomicUsize>,
}

impl<R> BoundedRead<R> {
    pub fn new(inner: R, limit: usize) -> Self {
        Self {
            inner,
            limit,
            consumed: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn consumed(&self) -> usize {
        self.consumed.load(Ordering::Relaxed)
    }
}

impl<R: Read> Read for BoundedRead<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let remaining = self.limit.saturating_sub(self.consumed());
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                ByteLimitExceeded { limit: self.limit },
            ));
        }
        let allowed = buffer.len().min(remaining);
        let read = self.inner.read(&mut buffer[..allowed])?;
        self.consumed.fetch_add(read, Ordering::Relaxed);
        Ok(read)
    }
}

/// Synthetic custom gix transport used solely to verify trait integration.
/// Its payload contains one valid packet line and no network-capable code.
pub struct SyntheticTransport {
    source: StreamingPeekableIter<BoundedRead<Cursor<Vec<u8>>>>,
    url: Vec<u8>,
    consumed: Arc<AtomicUsize>,
}

impl SyntheticTransport {
    pub fn new(packet_stream: Vec<u8>, max_read_bytes: usize) -> Self {
        let bounded = BoundedRead::new(Cursor::new(packet_stream), max_read_bytes);
        let consumed = Arc::clone(&bounded.consumed);
        let source = StreamingPeekableIter::new(bounded, &[PacketLineRef::Flush], false);
        Self {
            source,
            url: b"file:///synthetic-fixture".to_vec(),
            consumed,
        }
    }

    pub fn bytes_exposed(&self) -> usize {
        self.consumed.load(Ordering::Relaxed)
    }
}

impl TransportWithoutIO for SyntheticTransport {
    fn to_url(&self) -> Cow<'_, BStr> {
        Cow::Borrowed(self.url.as_bstr())
    }

    fn connection_persists_across_multiple_requests(&self) -> bool {
        false
    }

    fn configure(&mut self, _config: &dyn Any) -> gix_error::ExnResult {
        Ok(())
    }
}

impl Transport for SyntheticTransport {
    fn handshake<'a>(
        &mut self,
        _service: Service,
        _extra_parameters: &'a [(&'a str, Option<&'a str>)],
    ) -> Result<SetServiceResponse<'_>, gix_transport::client::Error> {
        Ok(SetServiceResponse {
            actual_protocol: Protocol::V2,
            capabilities: Capabilities::default(),
            refs: None,
        })
    }

    fn request(
        &mut self,
        write_mode: gix_transport::client::WriteMode,
        on_into_read: gix_transport::client::MessageKind,
        trace: bool,
    ) -> Result<RequestWriter<'_>, gix_transport::client::Error> {
        let handle_progress: HandleProgress<'_> = Box::new(|_, _| ProgressAction::Continue(()));
        let input: Box<dyn ExtendedBufRead + Unpin + '_> =
            Box::new(self.source.as_read_with_sidebands(handle_progress));
        Ok(RequestWriter::new_from_bufread(
            io::sink(),
            input,
            write_mode,
            on_into_read,
            trace,
        ))
    }
}

/// Read through the actual gix blocking `Transport` API. The response path
/// contains the strict byte wrapper before gix packet-line decoding.
pub fn consume_synthetic_response(
    packet_stream: Vec<u8>,
    limit: usize,
) -> io::Result<(Vec<u8>, usize)> {
    let mut transport = SyntheticTransport::new(packet_stream, limit);
    let _ =
        Transport::handshake(&mut transport, Service::UploadPack, &[]).map_err(io::Error::other)?;
    let request = Transport::request(
        &mut transport,
        gix_transport::client::WriteMode::Binary,
        gix_transport::client::MessageKind::Flush,
        false,
    )
    .map_err(io::Error::other)?;
    let mut response = request.into_read()?;
    let mut payload = Vec::new();
    response.read_to_end(&mut payload)?;
    drop(response);
    Ok((payload, transport.bytes_exposed()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_allows_fixture_under_limit() {
        // sideband packet line: 0009 + channel 1 + "PACK", followed by FLUSH.
        let (payload, count) =
            consume_synthetic_response(b"0009\x01PACK0000".to_vec(), 20).unwrap();
        assert_eq!(payload, b"PACK");
        assert_eq!(count, 13);
    }

    #[test]
    fn gix_transport_reader_stops_at_configured_limit() {
        let mut transport = SyntheticTransport::new(b"0009\x01PACK0000".to_vec(), 12);
        let _ = Transport::handshake(&mut transport, Service::UploadPack, &[]).unwrap();
        let request = Transport::request(
            &mut transport,
            gix_transport::client::WriteMode::Binary,
            gix_transport::client::MessageKind::Flush,
            false,
        )
        .unwrap();
        let mut response = request.into_read().unwrap();
        let error = response
            .read_to_end(&mut Vec::new())
            .expect_err("a packet stream crossing the limit must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("byte limit"));
        drop(response);
        assert_eq!(
            transport.bytes_exposed(),
            12,
            "the wrapper must not read a thirteenth byte"
        );
    }
}

pub mod http;

#[cfg(test)]
mod loopback_http_tests;

#[cfg(test)]
mod tls_ingress_tests;
