//! Experimental bounded decoding of complete SHA-1 Git packs.
//!
//! A successful result proves pack checksum, framing of the declared entries,
//! bounded inflation and delta reconstruction, and canonical object IDs. It
//! does not validate typed object syntax, required references, graph
//! completeness, authenticity, or cleanup suitability. This decoder is limited
//! to 16 MiB packs and cumulative decoded work, 1024 entries, backward-only
//! delta bases, and 32 delta levels. These experimental limits are not product
//! scope. The cumulative budget counts representation bytes and reconstructed
//! bytes, not total process memory; callers must use an outer supervised worker.
//!
//! Reference deltas resolve only against an earlier object in this pack. Missing,
//! forward, and external bases are unsupported by this deliberately narrowed
//! experimental decoder.

use std::{io, io::Cursor, time::Instant};

use crate::{
    check_deadline, check_sha1_pack_checksum, git_sha1_object_id, inflate_pack_representation,
    parse_sha1_pack_entry_header, reconstruct_git_delta, safe_error, CancellationHandle,
    GitObjectKind, PackEntryKind,
};

const MAX_PACK_BYTES: usize = 16 * 1024 * 1024;
const MAX_DECODED_BYTES: usize = 16 * 1024 * 1024;
const MAX_OBJECTS: u32 = 1024;
const MAX_DELTA_DEPTH: u8 = 32;
const PACK_HEADER_BYTES: usize = 12;
const TRAILER_BYTES: usize = 20;

/// Fully decoded entries from one pack that passed the supported checks.
#[derive(Debug)]
pub struct DecodedPack {
    objects: Vec<DecodedPackObject>,
    decoded_bytes: usize,
    checksum: [u8; 20],
}

impl DecodedPack {
    pub fn objects(&self) -> &[DecodedPackObject] {
        &self.objects
    }

    /// Sum of each inflated representation plus each reconstructed delta result.
    pub fn decoded_bytes(&self) -> usize {
        self.decoded_bytes
    }

    pub fn checksum(&self) -> &[u8; 20] {
        &self.checksum
    }
}

/// One decoded Git object and its source pack entry metadata.
#[derive(Debug)]
pub struct DecodedPackObject {
    offset: u64,
    kind: GitObjectKind,
    oid: [u8; 20],
    content: Vec<u8>,
    delta_depth: u8,
}

impl DecodedPackObject {
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn kind(&self) -> GitObjectKind {
        self.kind
    }

    pub fn oid(&self) -> &[u8; 20] {
        &self.oid
    }

    pub fn content(&self) -> &[u8] {
        &self.content
    }

    pub fn delta_depth(&self) -> u8 {
        self.delta_depth
    }
}

/// Decode all entries from a bounded SHA-1 pack.
///
/// The checksum is verified before entry parsing. Only earlier decoded entries
/// may be delta bases, and the final entry must end exactly at the checksum trailer.
pub fn decode_sha1_pack(
    input: &[u8],
    max_decoded_bytes: usize,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<DecodedPack> {
    check_attempt(cancellation, deadline)?;
    if input.len() > MAX_PACK_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "pack exceeds decoder input limit",
        ));
    }
    if max_decoded_bytes > MAX_DECODED_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "decoded byte limit exceeds decoder cap",
        ));
    }

    let report = check_sha1_pack_checksum(
        &mut Cursor::new(input),
        input.len() as u64,
        cancellation,
        deadline,
    )?;
    let count = report.declared_object_count();
    if count > MAX_OBJECTS {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "pack object count exceeds decoder limit",
        ));
    }

    let trailer_offset = input
        .len()
        .checked_sub(TRAILER_BYTES)
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "pack trailer offset is invalid"))?;
    check_attempt(cancellation, deadline)?;
    let mut objects = Vec::new();
    objects.try_reserve_exact(count as usize).map_err(|_| {
        safe_error(
            io::ErrorKind::OutOfMemory,
            "decoded object allocation failed",
        )
    })?;

    let mut cursor = PACK_HEADER_BYTES;
    let mut decoded_bytes = 0usize;
    for _ in 0..count {
        check_attempt(cancellation, deadline)?;
        if cursor >= trailer_offset {
            return Err(safe_error(
                io::ErrorKind::UnexpectedEof,
                "pack entry is missing",
            ));
        }
        let offset = u64::try_from(cursor)
            .map_err(|_| safe_error(io::ErrorKind::InvalidData, "pack offset is invalid"))?;
        let entry_input = &input[cursor..trailer_offset];
        let remaining_budget = max_decoded_bytes
            .checked_sub(decoded_bytes)
            .ok_or_else(|| {
                safe_error(io::ErrorKind::InvalidData, "decoded byte budget exceeded")
            })?;
        let header = parse_sha1_pack_entry_header(entry_input, offset, remaining_budget as u64)?;
        let representation_size = usize::try_from(header.inflated_bytes()).map_err(|_| {
            safe_error(
                io::ErrorKind::InvalidData,
                "entry size exceeds decoder bounds",
            )
        })?;
        decoded_bytes = decoded_bytes
            .checked_add(representation_size)
            .filter(|size| *size <= max_decoded_bytes)
            .ok_or_else(|| {
                safe_error(io::ErrorKind::InvalidData, "decoded byte budget exceeded")
            })?;

        let payload_start = header
            .header_bytes()
            .checked_add(cursor)
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "entry offset overflow"))?;
        if payload_start > trailer_offset {
            return Err(safe_error(
                io::ErrorKind::UnexpectedEof,
                "pack entry header exceeds payload",
            ));
        }
        let representation = inflate_pack_representation(
            &input[payload_start..trailer_offset],
            header.inflated_bytes(),
            representation_size,
            cancellation,
            deadline,
        )?;
        cursor = payload_start
            .checked_add(representation.compressed_bytes())
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "entry end offset overflow"))?;
        if cursor > trailer_offset {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "pack entry exceeds trailer",
            ));
        }

        let (kind, content, delta_depth) = match header.kind() {
            PackEntryKind::Commit => (GitObjectKind::Commit, representation.into_bytes(), 0),
            PackEntryKind::Tree => (GitObjectKind::Tree, representation.into_bytes(), 0),
            PackEntryKind::Blob => (GitObjectKind::Blob, representation.into_bytes(), 0),
            PackEntryKind::Tag => (GitObjectKind::Tag, representation.into_bytes(), 0),
            delta @ (PackEntryKind::OffsetDelta { .. } | PackEntryKind::ReferenceDelta { .. }) => {
                let base = objects
                    .iter()
                    .find(|object: &&DecodedPackObject| match delta {
                        PackEntryKind::OffsetDelta { base_offset } => object.offset == base_offset,
                        PackEntryKind::ReferenceDelta { base_oid } => object.oid == base_oid,
                        _ => false,
                    })
                    .ok_or_else(|| {
                        safe_error(
                            io::ErrorKind::Unsupported,
                            "delta base is unavailable or not earlier in pack",
                        )
                    })?;
                let depth = base
                    .delta_depth
                    .checked_add(1)
                    .filter(|depth| *depth <= MAX_DELTA_DEPTH)
                    .ok_or_else(|| {
                        safe_error(
                            io::ErrorKind::InvalidData,
                            "delta chain exceeds decoder limit",
                        )
                    })?;
                let remaining = max_decoded_bytes
                    .checked_sub(decoded_bytes)
                    .ok_or_else(|| {
                        safe_error(io::ErrorKind::InvalidData, "decoded byte budget exceeded")
                    })?;
                let content = reconstruct_git_delta(
                    &base.content,
                    representation.bytes(),
                    remaining,
                    cancellation,
                    deadline,
                )?;
                decoded_bytes = decoded_bytes
                    .checked_add(content.len())
                    .filter(|size| *size <= max_decoded_bytes)
                    .ok_or_else(|| {
                        safe_error(io::ErrorKind::InvalidData, "decoded byte budget exceeded")
                    })?;
                (base.kind, content, depth)
            }
        };
        let oid = git_sha1_object_id(kind, &content, cancellation, deadline)?;
        objects.push(DecodedPackObject {
            offset,
            kind,
            oid,
            content,
            delta_depth,
        });
    }

    check_attempt(cancellation, deadline)?;
    if cursor != trailer_offset {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "pack contains unframed or extra entry bytes",
        ));
    }
    check_attempt(cancellation, deadline)?;
    Ok(DecodedPack {
        objects,
        decoded_bytes,
        checksum: *report.checksum(),
    })
}

fn check_attempt(cancellation: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancellation.check()?;
    check_deadline(deadline)
}
