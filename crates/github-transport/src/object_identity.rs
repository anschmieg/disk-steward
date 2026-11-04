//! Bounded identity hashing for one supplied Git object representation.
//!
//! The digest binds the supplied object kind and raw content bytes using Git's
//! canonical object header. It does not validate type-specific syntax, delta
//! chains, pack checksums or counts, object graphs, trust, authenticity, or
//! suitability for cleanup. The 16 MiB cap is experimental and is not a product
//! limit.

use std::{io, time::Instant};

use sha1_checked::{digest::Digest, Sha1};

use crate::{check_deadline, safe_error, CancellationHandle};

const MAX_CONTENT_BYTES: usize = 16 * 1024 * 1024;
const HASH_CHUNK_BYTES: usize = 8 * 1024;

/// Git object kind included in the canonical object identity header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitObjectKind {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl GitObjectKind {
    fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Commit => b"commit",
            Self::Tree => b"tree",
            Self::Blob => b"blob",
            Self::Tag => b"tag",
        }
    }
}

/// Compute the SHA-1 identity of supplied Git object bytes.
///
/// Hashes `'<type> <decimal length>\0'` followed by `content`, in bounded
/// chunks. This function only binds the caller-supplied kind and bytes; it does
/// not parse or validate the content as a commit, tree, blob, or tag.
pub fn git_sha1_object_id(
    kind: GitObjectKind,
    content: &[u8],
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<[u8; 20]> {
    check_attempt(cancellation, deadline)?;
    if content.len() > MAX_CONTENT_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "object content exceeds supported bounds",
        ));
    }

    // Largest header is "commit 16777216\0" (16 bytes); keep spare room and
    // build it without allocating. Decimal digits are emitted from a small
    // reverse-order stack array.
    let mut header = [0u8; 32];
    let mut header_len = 0usize;
    let object_type = kind.as_bytes();
    header[..object_type.len()].copy_from_slice(object_type);
    header_len += object_type.len();
    header[header_len] = b' ';
    header_len += 1;

    let mut digits = [0u8; 20];
    let mut number = content.len();
    let mut digit_count = 0usize;
    loop {
        digits[digit_count] = b'0' + (number % 10) as u8;
        digit_count += 1;
        number /= 10;
        if number == 0 {
            break;
        }
    }
    for index in (0..digit_count).rev() {
        header[header_len] = digits[index];
        header_len += 1;
    }
    header[header_len] = 0;
    header_len += 1;

    let mut hasher = Sha1::new();
    checked_hash(&mut hasher, &header[..header_len], cancellation, deadline)?;
    for chunk in content.chunks(HASH_CHUNK_BYTES) {
        checked_hash(&mut hasher, chunk, cancellation, deadline)?;
    }

    check_attempt(cancellation, deadline)?;
    let digest = hasher.try_finalize();
    check_attempt(cancellation, deadline)?;
    if digest.has_collision() {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "object identity contains a SHA-1 collision",
        ));
    }
    let mut object_id = [0u8; 20];
    object_id.copy_from_slice(digest.hash().as_ref());
    Ok(object_id)
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

fn check_attempt(cancellation: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancellation.check()?;
    check_deadline(deadline)
}
