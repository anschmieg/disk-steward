//! Bounded parsing of individual Git SHA-1 pack entry headers.
//!
//! This parses only the entry header and does not inflate or validate the
//! object's contents. For delta entries, `inflated_bytes` is the size of the
//! delta instruction representation, not the reconstructed object's size.
//! An offset base is syntactically checked but is not verified as an entry
//! boundary; a reference base may be external to the pack. Callers must bound
//! and validate inflation, objects, and the resulting object graph separately.

use std::io;

use crate::safe_error;

const MAX_PACK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const PACK_HEADER_BYTES: u64 = 12;
const MAX_HEADER_BYTES: usize = 30;

/// The object kind or delta base described by a pack entry header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackEntryKind {
    Commit,
    Tree,
    Blob,
    Tag,
    OffsetDelta { base_offset: u64 },
    ReferenceDelta { base_oid: [u8; 20] },
}

/// The fields parsed from a Git SHA-1 pack entry header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackEntryHeader {
    kind: PackEntryKind,
    inflated_bytes: u64,
    header_bytes: usize,
}

impl PackEntryHeader {
    /// Declared uncompressed representation size. For deltas this is the
    /// delta instruction stream size, not the reconstructed object size.
    pub fn inflated_bytes(&self) -> u64 {
        self.inflated_bytes
    }

    /// Number of input bytes consumed by this header.
    pub fn header_bytes(&self) -> usize {
        self.header_bytes
    }

    /// The object kind or delta base encoded by this header.
    pub fn kind(&self) -> PackEntryKind {
        self.kind
    }
}

/// Parse a single SHA-1 pack entry header without allocating or inflating.
///
/// `entry_offset` is the absolute byte offset of the entry in its pack.
/// The compressed payload may follow `input`; only the header is consumed.
pub fn parse_sha1_pack_entry_header(
    input: &[u8],
    entry_offset: u64,
    max_inflated_bytes: u64,
) -> io::Result<PackEntryHeader> {
    if max_inflated_bytes > MAX_PACK_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "inflated byte limit exceeds supported maximum",
        ));
    }
    if !(PACK_HEADER_BYTES..=MAX_PACK_BYTES).contains(&entry_offset) {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "pack entry offset is outside supported bounds",
        ));
    }

    let first = *input.first().ok_or_else(|| {
        safe_error(
            io::ErrorKind::UnexpectedEof,
            "pack entry header is truncated",
        )
    })?;
    let type_code = (first >> 4) & 0x07;
    let mut size = u64::from(first & 0x0f);
    let mut cursor = 1usize;
    let mut continuation = first & 0x80 != 0;
    let mut shift = 4u32;

    while continuation {
        if cursor >= 10 {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "pack entry size header is too long",
            ));
        }
        let byte = *input.get(cursor).ok_or_else(|| {
            safe_error(
                io::ErrorKind::UnexpectedEof,
                "pack entry header is truncated",
            )
        })?;
        let chunk = u64::from(byte & 0x7f);
        if shift >= 64 || chunk > (u64::MAX >> shift) {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "pack entry size overflows",
            ));
        }
        size |= chunk << shift;
        continuation = byte & 0x80 != 0;
        cursor += 1;
        shift += 7;
    }

    if size > max_inflated_bytes {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "pack entry size exceeds configured limit",
        ));
    }

    let kind = match type_code {
        1 => PackEntryKind::Commit,
        2 => PackEntryKind::Tree,
        3 => PackEntryKind::Blob,
        4 => PackEntryKind::Tag,
        6 => {
            let byte = *input.get(cursor).ok_or_else(|| {
                safe_error(
                    io::ErrorKind::UnexpectedEof,
                    "pack entry header is truncated",
                )
            })?;
            cursor += 1;
            let mut distance = u64::from(byte & 0x7f);
            let mut continued = byte & 0x80 != 0;
            let mut count = 1usize;
            while continued {
                if count >= 10 {
                    return Err(safe_error(
                        io::ErrorKind::InvalidData,
                        "offset delta header is too long",
                    ));
                }
                let next = *input.get(cursor).ok_or_else(|| {
                    safe_error(
                        io::ErrorKind::UnexpectedEof,
                        "pack entry header is truncated",
                    )
                })?;
                cursor += 1;
                count += 1;
                distance = distance
                    .checked_add(1)
                    .and_then(|v| v.checked_mul(128))
                    .and_then(|v| v.checked_add(u64::from(next & 0x7f)))
                    .ok_or_else(|| {
                        safe_error(
                            io::ErrorKind::InvalidData,
                            "offset delta distance overflows",
                        )
                    })?;
                continued = next & 0x80 != 0;
            }
            let base_offset = entry_offset.checked_sub(distance).ok_or_else(|| {
                safe_error(
                    io::ErrorKind::InvalidData,
                    "offset delta base offset is invalid",
                )
            })?;
            if distance == 0 || base_offset < PACK_HEADER_BYTES {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "offset delta base offset is invalid",
                ));
            }
            PackEntryKind::OffsetDelta { base_offset }
        }
        7 => {
            let end = cursor.checked_add(20).ok_or_else(|| {
                safe_error(
                    io::ErrorKind::InvalidData,
                    "reference delta header overflows",
                )
            })?;
            if end > input.len() {
                return Err(safe_error(
                    io::ErrorKind::UnexpectedEof,
                    "pack entry header is truncated",
                ));
            }
            let mut base_oid = [0u8; 20];
            base_oid.copy_from_slice(&input[cursor..end]);
            if base_oid.iter().all(|byte| *byte == 0) {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "reference delta base object id is invalid",
                ));
            }
            cursor = end;
            PackEntryKind::ReferenceDelta { base_oid }
        }
        _ => {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "pack entry type is invalid",
            ));
        }
    };

    if cursor > MAX_HEADER_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "pack entry header exceeds supported length",
        ));
    }
    Ok(PackEntryHeader {
        kind,
        inflated_bytes: size,
        header_bytes: cursor,
    })
}
