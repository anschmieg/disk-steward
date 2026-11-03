use super::*;
use std::io::Cursor;

const EMPTY_CHECKSUM: [u8; 20] = [
    0x02, 0x9d, 0x08, 0x82, 0x3b, 0xd8, 0xa8, 0xea, 0xb5, 0x10, 0xad, 0x6a, 0xc7, 0x5c, 0x82, 0x3c,
    0xfd, 0x3e, 0xd3, 0x1e,
];
fn empty_pack() -> Vec<u8> {
    let mut bytes = b"PACK\0\0\0\x02\0\0\0\0".to_vec();
    bytes.extend_from_slice(&EMPTY_CHECKSUM);
    bytes
}
fn check(bytes: &[u8]) -> io::Result<PackChecksumReport> {
    check_sha1_pack_checksum(
        &mut Cursor::new(bytes),
        bytes.len() as u64,
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5),
    )
}

struct Segments<'a> {
    data: &'a [u8],
    step: usize,
    max_requested: usize,
}
impl Read for Segments<'_> {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        self.max_requested = self.max_requested.max(target.len());
        let n = target.len().min(self.step).min(self.data.len());
        target[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

#[test]
fn checksum_matches_independent_empty_pack_vector_with_split_reads() {
    let bytes = empty_pack();
    let mut reader = Segments {
        data: &bytes,
        step: 1,
        max_requested: 0,
    };
    let report = check_sha1_pack_checksum(
        &mut reader,
        32,
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(report.pack_bytes(), 32);
    assert_eq!(report.version(), 2);
    assert_eq!(report.declared_object_count(), 0);
    assert_eq!(report.checksum(), &EMPTY_CHECKSUM);
    assert!(reader.data.is_empty());
    assert!(reader.max_requested <= 20);
}

#[test]
fn checksum_match_does_not_establish_object_validity_or_declared_count() {
    let mut bytes = b"PACK\0\0\0\x03\xff\xff\xff\xff".to_vec();
    bytes.extend_from_slice(&vec![b'x'; 20_000]); // deliberately not an object stream
    let hash = sha1_checked::Sha1::try_digest(&bytes);
    assert!(!hash.has_collision());
    bytes.extend_from_slice(hash.hash());
    let mut reader = Segments {
        data: &bytes,
        step: 997,
        max_requested: 0,
    };
    let report = check_sha1_pack_checksum(
        &mut reader,
        bytes.len() as u64,
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(report.version(), 3);
    assert_eq!(report.declared_object_count(), u32::MAX);
    assert!(reader.max_requested <= 8192);
}

#[test]
fn checksum_rejects_header_corruption_truncation_and_appended_bytes() {
    let valid = empty_pack();
    let mut wrong_magic = valid.clone();
    wrong_magic[0] ^= 1;
    let mut wrong_version = valid.clone();
    wrong_version[7] = 1;
    let mut wrong_checksum = valid.clone();
    wrong_checksum[31] ^= 1;
    for bytes in [wrong_magic, wrong_version, wrong_checksum] {
        assert_eq!(
            check(&bytes).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    assert_eq!(
        check_sha1_pack_checksum(
            &mut Cursor::new(&valid[..31]),
            32,
            &CancellationHandle::new(),
            Instant::now() + Duration::from_secs(5)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::UnexpectedEof
    );
    let mut extra = valid;
    extra.push(0);
    assert_eq!(
        check_sha1_pack_checksum(
            &mut Cursor::new(extra),
            32,
            &CancellationHandle::new(),
            Instant::now() + Duration::from_secs(5)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidData
    );
}

struct NoRead;
impl Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("preflight must not read")
    }
}
#[test]
fn checksum_preflight_rejects_lengths_cancel_and_deadline_before_io() {
    let signal = CancellationHandle::new();
    for length in [0, 31, 2 * 1024 * 1024 * 1024 + 1] {
        assert_eq!(
            check_sha1_pack_checksum(
                &mut NoRead,
                length,
                &signal,
                Instant::now() + Duration::from_secs(5)
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        check_sha1_pack_checksum(&mut NoRead, 32, &signal, Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    signal.cancel();
    assert_eq!(
        check_sha1_pack_checksum(
            &mut NoRead,
            32,
            &signal,
            Instant::now() + Duration::from_secs(5)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::ConnectionAborted
    );
}

struct InvalidCount;
impl Read for InvalidCount {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        Ok(target.len() + 1)
    }
}
struct Interrupts {
    calls: usize,
}
impl Read for Interrupts {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        Err(io::ErrorKind::Interrupted.into())
    }
}
struct ErrorRead;
impl Read for ErrorRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "fixture secret context",
        ))
    }
}
#[test]
fn checksum_bounds_invalid_readers_and_sanitizes_errors() {
    let signal = CancellationHandle::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    assert_eq!(
        check_sha1_pack_checksum(&mut InvalidCount, 32, &signal, deadline)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    let mut interrupts = Interrupts { calls: 0 };
    assert!(check_sha1_pack_checksum(&mut interrupts, 32, &signal, deadline).is_err());
    assert!(interrupts.calls <= 17);
    let error = check_sha1_pack_checksum(&mut ErrorRead, 32, &signal, deadline).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(!error.to_string().contains("fixture secret"));
}
struct CancelRead {
    signal: CancellationHandle,
    calls: usize,
}
impl Read for CancelRead {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        target.fill(0);
        self.signal.cancel();
        Ok(target.len())
    }
}
#[test]
fn checksum_checks_cancellation_after_a_positive_read() {
    let signal = CancellationHandle::new();
    let mut reader = CancelRead {
        signal: signal.clone(),
        calls: 0,
    };
    assert_eq!(
        check_sha1_pack_checksum(
            &mut reader,
            32,
            &signal,
            Instant::now() + Duration::from_secs(5)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::ConnectionAborted
    );
    assert_eq!(reader.calls, 1);
}
