//! Bounded, pre-inflation verification of a Git pack's trailing SHA-1.
//!
//! This checks that the pack trailer matches the bytes in the pack header and
//! compressed payload. It does not validate objects, object counts, authenticity,
//! completeness, reachability, or suitability for retention or cleanup.

use std::{
    io::{self, Read},
    time::Instant,
};

use sha1_checked::{digest::Digest, Sha1};

use crate::{check_deadline, safe_error, CancellationHandle};

const HEADER_BYTES: usize = 12;
const TRAILER_BYTES: usize = 20;
const MIN_PACK_BYTES: u64 = (HEADER_BYTES + TRAILER_BYTES) as u64;
const MAX_PACK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const BODY_BUFFER_BYTES: usize = 8 * 1024;
const MAX_INTERRUPTED_READS: usize = 16;

/// Summary of the claims checked by [`check_sha1_pack_checksum`].
///
/// A successful report means only that a supported header was present, the
/// bounded byte count matched, and its trailer matched the collision-checked
/// digest. It does not establish object integrity or trustworthiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackChecksumReport {
    pack_bytes: u64,
    version: u32,
    declared_object_count: u32,
    checksum: [u8; TRAILER_BYTES],
}

impl PackChecksumReport {
    pub fn pack_bytes(&self) -> u64 {
        self.pack_bytes
    }
    pub fn version(&self) -> u32 {
        self.version
    }
    pub fn declared_object_count(&self) -> u32 {
        self.declared_object_count
    }
    pub fn checksum(&self) -> &[u8; TRAILER_BYTES] {
        &self.checksum
    }
}

/// Check a bounded legacy SHA-1 pack without inflating or enumerating objects.
///
/// Reads exactly `expected_len` bytes plus a one-byte EOF probe. Generic readers
/// may block inside `Read`; callers requiring a hard wall-clock bound must run
/// this function under an outer worker deadline.
pub fn check_sha1_pack_checksum(
    reader: &mut impl Read,
    expected_len: u64,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<PackChecksumReport> {
    check_attempt(cancellation, deadline)?;
    if !(MIN_PACK_BYTES..=MAX_PACK_BYTES).contains(&expected_len) {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "pack length is outside supported bounds",
        ));
    }

    let mut header = [0u8; HEADER_BYTES];
    read_exact_bounded(
        reader,
        &mut header,
        cancellation,
        deadline,
        "pack header read failed",
    )?;
    check_attempt(cancellation, deadline)?;
    if &header[..4] != b"PACK" {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "pack header magic is invalid",
        ));
    }
    let version = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    if version != 2 && version != 3 {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "pack version is unsupported",
        ));
    }
    let declared_object_count = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);

    let mut hasher = Sha1::new();
    checked_hash(&mut hasher, &header, cancellation, deadline)?;

    let body_len = expected_len - MIN_PACK_BYTES;
    let mut remaining = body_len;
    let mut buffer = [0u8; BODY_BUFFER_BYTES];
    while remaining != 0 {
        check_attempt(cancellation, deadline)?;
        let limit = usize::try_from(remaining.min(BODY_BUFFER_BYTES as u64))
            .map_err(|_| safe_error(io::ErrorKind::InvalidData, "pack body length is invalid"))?;
        let count = read_some_bounded(
            reader,
            &mut buffer[..limit],
            cancellation,
            deadline,
            "pack payload read failed",
        )?;
        if count == 0 {
            return Err(safe_error(
                io::ErrorKind::UnexpectedEof,
                "pack payload is truncated",
            ));
        }
        // read_some_bounded has checked the count before returning it, so the
        // slice is safe to form and remains within the fixed stack buffer.
        checked_hash(&mut hasher, &buffer[..count], cancellation, deadline)?;
        remaining -= count as u64;
    }

    let mut trailer = [0u8; TRAILER_BYTES];
    read_exact_bounded(
        reader,
        &mut trailer,
        cancellation,
        deadline,
        "pack checksum read failed",
    )?;
    check_attempt(cancellation, deadline)?;
    let digest = hasher.try_finalize();
    check_attempt(cancellation, deadline)?;
    if digest.has_collision() {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "pack checksum contains a SHA-1 collision",
        ));
    }
    let mut checksum = [0u8; TRAILER_BYTES];
    checksum.copy_from_slice(digest.hash().as_ref());
    if checksum != trailer {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "pack checksum does not match",
        ));
    }

    let mut extra = [0u8; 1];
    let count = read_some_bounded(
        reader,
        &mut extra,
        cancellation,
        deadline,
        "pack end-of-input check failed",
    )?;
    if count != 0 {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "bytes follow pack checksum",
        ));
    }
    check_attempt(cancellation, deadline)?;
    Ok(PackChecksumReport {
        pack_bytes: expected_len,
        version,
        declared_object_count,
        checksum,
    })
}

fn checked_hash(
    hasher: &mut Sha1,
    bytes: &[u8],
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<()> {
    check_attempt(cancellation, deadline)?;
    hasher.update(bytes);
    check_attempt(cancellation, deadline)
}

fn read_exact_bounded(
    reader: &mut impl Read,
    mut target: &mut [u8],
    cancellation: &CancellationHandle,
    deadline: Instant,
    message: &'static str,
) -> io::Result<()> {
    while !target.is_empty() {
        let count = read_some_bounded(reader, target, cancellation, deadline, message)?;
        if count == 0 {
            return Err(safe_error(io::ErrorKind::UnexpectedEof, message));
        }
        target = &mut target[count..];
    }
    Ok(())
}

fn read_some_bounded(
    reader: &mut impl Read,
    target: &mut [u8],
    cancellation: &CancellationHandle,
    deadline: Instant,
    message: &'static str,
) -> io::Result<usize> {
    let mut interrupted = 0usize;
    loop {
        check_attempt(cancellation, deadline)?;
        match reader.read(target) {
            Ok(count) => {
                check_attempt(cancellation, deadline)?;
                if count > target.len() {
                    return Err(safe_error(
                        io::ErrorKind::InvalidData,
                        "reader returned an invalid byte count",
                    ));
                }
                return Ok(count);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                interrupted += 1;
                check_attempt(cancellation, deadline)?;
                if interrupted > MAX_INTERRUPTED_READS {
                    return Err(safe_error(
                        io::ErrorKind::Interrupted,
                        "reader interruption limit reached",
                    ));
                }
            }
            Err(error) => {
                let kind = error.kind();
                check_attempt(cancellation, deadline)?;
                return Err(safe_error(kind, message));
            }
        }
    }
}

fn check_attempt(cancellation: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancellation.check()?;
    check_deadline(deadline)
}
