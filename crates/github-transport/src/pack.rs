use std::{
    collections::HashSet,
    io::{self, Write},
    time::Instant,
};

use gix_transport::client::blocking_io::{
    ExtendedBufRead as _, ReadlineBufRead as _, TransportV2Ext as _,
};
use sha2::{Digest, Sha256};

use crate::{
    check_deadline, safe_error, AdvertisedRef, CancellationHandle, GitHubTransport,
    HttpFinalizationReport,
};

const MAX_FETCH_TIPS: usize = 64;
const MAX_PACK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_PACKETS: usize = 65_536;
const MAX_PACKET_DATA: usize = 64 * 1024;
const MAX_PROGRESS_BYTES: usize = 1024 * 1024;
const MAX_INTERRUPTED_WRITES: usize = 16;

/// Outcome evidence for one bounded pack transfer attempt.
///
/// A completed transfer establishes only that the requested pack stream and
/// tracked HTTP response framing completed. It does not establish pack
/// integrity, object completeness, reachability, or removal eligibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackTransferReport {
    context: crate::attempt_context::AttemptContext,
    bytes_written: u64,
    operation_error_kind: Option<io::ErrorKind>,
    finalization: HttpFinalizationReport,
    advertised_refs: Option<Vec<AdvertisedRef>>,
    accepted_pack_sha256: Option<[u8; 32]>,
}

impl PackTransferReport {
    /// Exact validated URL used to construct this attempt. No alias rewriting.
    pub fn repository(&self) -> &crate::RepositoryUrl {
        self.context.repository()
    }

    /// Require a complete transfer, the expected exact repository, and an age
    /// below fifteen minutes on both monotonic and wall clocks from transport
    /// construction in this process.
    /// Clock expiry/invalidity remains terminal across clones of this attempt.
    /// This is not a durable receipt, retention promise or cleanup authorization.
    pub fn check_context(&self, expected: &crate::RepositoryUrl) -> io::Result<()> {
        if !self.is_complete_transfer() {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "pack transfer incomplete",
            ));
        }
        self.context.check(expected)
    }
    pub(crate) fn context(&self) -> &crate::attempt_context::AttemptContext {
        &self.context
    }

    /// Bytes the sink reported accepting, including any partial write before
    /// a later sink or protocol failure.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Sanitized operation failure metadata, if Git transfer failed.
    pub fn operation_error_kind(&self) -> Option<io::ErrorKind> {
        self.operation_error_kind
    }

    /// HTTP framing evidence collected after the Git operation ended.
    pub fn finalization(&self) -> &HttpFinalizationReport {
        &self.finalization
    }

    /// Syntax-validated ref claims from this transfer's own advertisement, available only
    /// when the pack operation and tracked HTTP framing both completed.
    /// These remain unverified server claims, not proof of object integrity.
    pub fn advertised_refs(&self) -> Option<&[AdvertisedRef]> {
        self.advertised_refs.as_deref()
    }

    /// SHA-256 of pack bytes the caller's sink reported accepting. Available
    /// only when the pack operation and tracked HTTP framing both completed.
    /// This binds the reported accepted byte stream; it does not prove physical
    /// persistence, authenticate the source, verify pack integrity, or establish
    /// cleanup eligibility.
    pub fn accepted_pack_sha256(&self) -> Option<&[u8; 32]> {
        self.accepted_pack_sha256.as_ref()
    }

    /// True only when the Git pack section completed and all tracked HTTP
    /// response bodies finalized successfully. This is not object verification.
    pub fn is_complete_transfer(&self) -> bool {
        self.operation_error_kind.is_none()
            && self.bytes_written > 0
            && self.finalization.is_complete()
            && self.advertised_refs.is_some()
            && self.accepted_pack_sha256.is_some()
    }
}

/// Stream the pack for every distinct freshly advertised SHA-1 ref tip into a
/// caller-owned sink, then finalize all tracked HTTP response bodies.
///
/// The report exposes refs only when both this pack operation and the HTTP
/// finalization complete. Those are unverified server-advertised claims bound
/// to the same attempt. They do not establish object integrity, completeness,
/// reachability, retention, an atomic ref snapshot, recoverability, or cleanup
/// eligibility. Any bytes accepted before an error remain in the caller's sink
/// and are the caller's responsibility to retain or remove.
pub(super) fn download_advertised_pack(
    mut transport: GitHubTransport,
    sink: &mut impl Write,
) -> io::Result<PackTransferReport> {
    let context = transport.context.clone();
    let cancellation = transport.cancellation_handle();
    let deadline = transport.deadline;
    let mut bytes_written = 0;
    let (operation, digest) = {
        let mut digesting_sink = DigestingSink {
            inner: sink,
            digest: Sha256::new(),
        };
        let operation = transfer_pack(
            &mut transport,
            &mut digesting_sink,
            &cancellation,
            deadline,
            &mut bytes_written,
        );
        (operation, digesting_sink.digest)
    };

    // A sink may refuse growth because its owned-store reserve is exhausted.
    // Do not download the remaining response merely to finalize that failure.
    if operation.is_err() {
        cancellation.cancel();
    }
    let operation_error_kind = operation.as_ref().err().map(io::Error::kind);
    let finalization = transport.finalize_http_framing()?;
    let (advertised_refs, accepted_pack_sha256) = match operation {
        Ok(refs) if finalization.is_complete() => (Some(refs), Some(digest.finalize().into())),
        _ => (None, None),
    };
    Ok(PackTransferReport {
        context,
        bytes_written,
        operation_error_kind,
        finalization,
        advertised_refs,
        accepted_pack_sha256,
    })
}

struct DigestingSink<'a, W: Write> {
    inner: &'a mut W,
    digest: Sha256,
}

impl<W: Write> Write for DigestingSink<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.inner.write(bytes) {
            Ok(count) => {
                if count <= bytes.len() {
                    self.digest.update(&bytes[..count]);
                }
                Ok(count)
            }
            Err(error) => Err(error),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn transfer_pack(
    transport: &mut GitHubTransport,
    sink: &mut impl Write,
    cancellation: &CancellationHandle,
    deadline: Instant,
    bytes_written: &mut u64,
) -> io::Result<Vec<AdvertisedRef>> {
    check_attempt(cancellation, deadline)?;
    let refs = crate::refs::read_advertised_refs_for_fetch(transport)?;
    let tips = distinct_tips(&refs)?;
    check_attempt(cancellation, deadline)?;

    let args = fetch_arguments(&tips);
    let mut reader = transport
        .invoke(
            "fetch",
            std::iter::empty::<(&str, Option<&str>)>(),
            Some(args.into_iter()),
            false,
        )
        .map_err(|error| client_error(error, cancellation, deadline, "fetch request failed"))?;

    check_attempt(cancellation, deadline)?;
    let first_line = reader.readline();
    check_attempt(cancellation, deadline)?;
    let first = first_line
        .ok_or_else(|| safe_error(io::ErrorKind::UnexpectedEof, "fetch response missing"))?
        .map_err(|error| read_error(error, cancellation, deadline, "fetch response malformed"))?;
    if !matches!(
        first,
        Ok(gix_transport::packetline::PacketLineRef::Data(
            b"packfile\n"
        ))
    ) {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "fetch packfile section missing",
        ));
    }

    let mut packet_count = 0usize;
    let mut pack_bytes = 0u64;
    let mut progress_bytes = 0usize;
    let mut pack_prefix = PackPrefix::default();

    loop {
        check_attempt(cancellation, deadline)?;
        let line = reader.readline();
        check_attempt(cancellation, deadline)?;
        let Some(line) = line else {
            if reader.stopped_at() == Some(gix_transport::client::MessageKind::Flush) {
                break;
            }
            return Err(safe_error(
                io::ErrorKind::UnexpectedEof,
                "fetch pack stream truncated",
            ));
        };
        packet_count += 1;
        if packet_count > MAX_PACKETS {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "fetch packet limit exceeded",
            ));
        }
        let line = line
            .map_err(|error| read_error(error, cancellation, deadline, "fetch packet malformed"))?;
        match line {
            Ok(gix_transport::packetline::PacketLineRef::Data(data)) if !data.is_empty() => {
                if data.len() > MAX_PACKET_DATA {
                    return Err(safe_error(
                        io::ErrorKind::InvalidData,
                        "fetch packet data limit exceeded",
                    ));
                }
                match data[0] {
                    1 => {
                        let payload = &data[1..];
                        let next_pack_bytes = pack_bytes
                            .checked_add(payload.len() as u64)
                            .ok_or_else(|| {
                                safe_error(io::ErrorKind::InvalidData, "fetch pack size overflow")
                            })?;
                        if next_pack_bytes > MAX_PACK_BYTES {
                            return Err(safe_error(
                                io::ErrorKind::InvalidData,
                                "fetch pack limit exceeded",
                            ));
                        }
                        pack_bytes = next_pack_bytes;
                        write_pack_payload(
                            payload,
                            sink,
                            bytes_written,
                            &mut pack_prefix,
                            cancellation,
                            deadline,
                        )?;
                    }
                    2 => {
                        let payload_len = data.len() - 1;
                        progress_bytes =
                            progress_bytes.checked_add(payload_len).ok_or_else(|| {
                                safe_error(
                                    io::ErrorKind::InvalidData,
                                    "fetch progress size overflow",
                                )
                            })?;
                        if progress_bytes > MAX_PROGRESS_BYTES {
                            return Err(safe_error(
                                io::ErrorKind::InvalidData,
                                "fetch progress limit exceeded",
                            ));
                        }
                    }
                    3 => {
                        return Err(safe_error(
                            io::ErrorKind::InvalidData,
                            "fetch server reported failure",
                        ));
                    }
                    _ => {
                        return Err(safe_error(
                            io::ErrorKind::InvalidData,
                            "fetch sideband channel invalid",
                        ));
                    }
                }
            }
            _ => {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "fetch response packet invalid",
                ));
            }
        }
    }

    check_attempt(cancellation, deadline)?;
    if !pack_prefix.validated || pack_bytes == 0 || *bytes_written != pack_bytes {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "fetch pack stream incomplete",
        ));
    }
    Ok(refs)
}

fn distinct_tips(refs: &[AdvertisedRef]) -> io::Result<Vec<[u8; 20]>> {
    distinct_oids(refs.iter().map(AdvertisedRef::oid))
}

fn distinct_oids<'a>(oids: impl IntoIterator<Item = &'a [u8; 20]>) -> io::Result<Vec<[u8; 20]>> {
    let mut seen = HashSet::new();
    let mut tips = Vec::new();
    for oid in oids {
        let oid = *oid;
        if seen.insert(oid) {
            if tips.len() == MAX_FETCH_TIPS {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "advertised tip limit exceeded",
                ));
            }
            tips.push(oid);
        }
    }
    if tips.is_empty() {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "repository advertises no tips",
        ));
    }
    tips.sort_unstable();
    Ok(tips)
}

fn fetch_arguments(tips: &[[u8; 20]]) -> Vec<gix_transport::bstr::BString> {
    let mut args = Vec::with_capacity(tips.len() + 3);
    args.extend(
        tips.iter()
            .map(|oid| format!("want {}", format_oid(oid)).into()),
    );
    args.push("ofs-delta".into());
    args.push("no-progress".into());
    args.push("done".into());
    args
}

fn format_oid(oid: &[u8; 20]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(40);
    for byte in oid {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[derive(Default)]
struct PackPrefix {
    bytes: [u8; 4],
    len: usize,
    validated: bool,
}

fn write_pack_payload(
    mut payload: &[u8],
    sink: &mut impl Write,
    bytes_written: &mut u64,
    prefix: &mut PackPrefix,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<()> {
    check_attempt(cancellation, deadline)?;
    if !prefix.validated {
        let needed = prefix.bytes.len() - prefix.len;
        let take = needed.min(payload.len());
        prefix.bytes[prefix.len..prefix.len + take].copy_from_slice(&payload[..take]);
        prefix.len += take;
        payload = &payload[take..];
        if prefix.len == prefix.bytes.len() {
            if &prefix.bytes != b"PACK" {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "fetch pack signature invalid",
                ));
            }
            prefix.validated = true;
            write_all_counted(&prefix.bytes, sink, bytes_written, cancellation, deadline)?;
        }
    }
    if prefix.validated && !payload.is_empty() {
        write_all_counted(payload, sink, bytes_written, cancellation, deadline)?;
    }
    check_attempt(cancellation, deadline)
}

fn write_all_counted(
    mut bytes: &[u8],
    sink: &mut impl Write,
    bytes_written: &mut u64,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<()> {
    let mut interrupted = 0usize;
    while !bytes.is_empty() {
        check_attempt(cancellation, deadline)?;
        match sink.write(bytes) {
            Ok(0) => {
                check_attempt(cancellation, deadline)?;
                return Err(safe_error(
                    io::ErrorKind::WriteZero,
                    "pack sink made no progress",
                ));
            }
            Ok(count) if count <= bytes.len() => {
                *bytes_written = bytes_written.checked_add(count as u64).ok_or_else(|| {
                    safe_error(io::ErrorKind::InvalidData, "pack sink byte count overflow")
                })?;
                bytes = &bytes[count..];
                interrupted = 0;
                check_attempt(cancellation, deadline)?;
            }
            Ok(_) => {
                check_attempt(cancellation, deadline)?;
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "pack sink returned invalid byte count",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                interrupted += 1;
                check_attempt(cancellation, deadline)?;
                if interrupted > MAX_INTERRUPTED_WRITES {
                    return Err(safe_error(
                        io::ErrorKind::Interrupted,
                        "pack sink interruption limit exceeded",
                    ));
                }
            }
            Err(error) => {
                return Err(check_attempt(cancellation, deadline)
                    .err()
                    .unwrap_or_else(|| safe_error(error.kind(), "pack sink write failed")));
            }
        }
    }
    check_attempt(cancellation, deadline)
}

fn check_attempt(cancellation: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancellation.check()?;
    check_deadline(deadline)
}

fn read_error(
    error: io::Error,
    cancellation: &CancellationHandle,
    deadline: Instant,
    message: &'static str,
) -> io::Error {
    if let Err(signal) = check_attempt(cancellation, deadline) {
        signal
    } else {
        safe_error(crate::refs::io_failure_kind(&error), message)
    }
}

fn client_error(
    error: gix_transport::client::Error,
    cancellation: &CancellationHandle,
    deadline: Instant,
    message: &'static str,
) -> io::Error {
    if let Err(signal) = check_attempt(cancellation, deadline) {
        return signal;
    }
    crate::refs::map_client_error(error, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        io,
        sync::{atomic::AtomicBool, Arc},
    };

    struct PartialThenFail {
        accepted: Vec<u8>,
        writes_left: Cell<usize>,
    }

    impl Write for PartialThenFail {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.writes_left.get() == 0 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "fixture detail"));
            }
            self.writes_left.set(self.writes_left.get() - 1);
            let count = bytes.len().min(2);
            self.accepted.extend_from_slice(&bytes[..count]);
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn counted_writer_reports_accepted_partial_bytes_before_failure() {
        let cancellation = CancellationHandle(Arc::new(AtomicBool::new(false)));
        let mut sink = PartialThenFail {
            accepted: Vec::new(),
            writes_left: Cell::new(2),
        };
        let mut written = 0;
        let error = write_all_counted(
            b"PACKpayload",
            &mut sink,
            &mut written,
            &cancellation,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(written, sink.accepted.len() as u64);
        assert_eq!(sink.accepted, b"PACK");
    }

    struct InvalidCount;

    impl Write for InvalidCount {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len() + 1)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ZeroWriter;

    impl Write for ZeroWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn counted_writer_rejects_invalid_sink_count_without_claiming_bytes() {
        let cancellation = CancellationHandle(Arc::new(AtomicBool::new(false)));
        let mut written = 0;
        let error = write_all_counted(
            b"PACK",
            &mut InvalidCount,
            &mut written,
            &cancellation,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(written, 0);
    }

    #[test]
    fn counted_writer_refuses_zero_progress() {
        let cancellation = CancellationHandle(Arc::new(AtomicBool::new(false)));
        let mut written = 0;
        let error = write_all_counted(
            b"PACK",
            &mut ZeroWriter,
            &mut written,
            &cancellation,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        assert_eq!(written, 0);
    }

    #[test]
    fn distinct_tips_deduplicates_and_refuses_more_than_sixty_four() {
        let oids = (0..65).map(|value| [value as u8; 20]).collect::<Vec<_>>();
        assert_eq!(distinct_oids(oids[..64].iter()).unwrap().len(), 64);
        assert_eq!(
            distinct_oids(oids.iter()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let duplicate = [[0; 20], [0; 20]];
        assert_eq!(distinct_oids(duplicate.iter()).unwrap(), vec![[0; 20]]);
        let empty: [[u8; 20]; 0] = [];
        assert_eq!(
            distinct_oids(empty.iter()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn pack_payload_requires_magic_before_sink_acceptance() {
        let cancellation = CancellationHandle(Arc::new(AtomicBool::new(false)));
        let mut sink = Vec::new();
        let mut written = 0;
        let mut prefix = PackPrefix::default();
        let error = write_pack_payload(
            b"BAD!payload",
            &mut sink,
            &mut written,
            &mut prefix,
            &cancellation,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(sink.is_empty());
        assert_eq!(written, 0);
    }

    #[test]
    fn cancellation_and_expired_deadline_are_checked() {
        let cancellation = CancellationHandle(Arc::new(AtomicBool::new(true)));
        let error = check_attempt(
            &cancellation,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
        let cancellation = CancellationHandle(Arc::new(AtomicBool::new(false)));
        let error = check_attempt(&cancellation, Instant::now()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
