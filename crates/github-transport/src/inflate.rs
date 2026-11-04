//! Bounded inflation of one zlib-compressed Git pack representation.
//!
//! This validates only the zlib stream and its declared uncompressed length.
//! It does not verify object identity, type, count, base references, or graph
//! consistency, and it does not iterate a whole pack or decode multistream
//! input. Callers must enforce aggregate budgets and use an outer worker for
//! hard interruption. The 16 MiB bound is experimental, not a product scope
//! or default. The decoder's internal fixed-state allocation is not fallible
//! through this API.

use std::{io, time::Instant};

use flate2::{Decompress, FlushDecompress, Status};

use crate::{check_deadline, safe_error, CancellationHandle};

const MAX_REPRESENTATION_BYTES: usize = 16 * 1024 * 1024;
const IO_CHUNK_BYTES: usize = 8 * 1024;
const MAX_DECOMPRESS_CALLS: usize = 1_048_576;

/// The inflated bytes and compressed length consumed for one representation.
#[derive(Debug)]
pub struct InflatedRepresentation {
    bytes: Vec<u8>,
    compressed_bytes: usize,
}

impl InflatedRepresentation {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn compressed_bytes(&self) -> usize {
        self.compressed_bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Inflate one zlib representation from a bounded portion of a pack.
///
/// `expected_bytes` must fit both `max_output_bytes` and the implementation's
/// 16 MiB cap. Trailing input is left untouched for the caller's next pack
/// entry; this function does not require EOF after the zlib stream.
pub fn inflate_pack_representation(
    input: &[u8],
    expected_bytes: u64,
    max_output_bytes: usize,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<InflatedRepresentation> {
    check_attempt(cancellation, deadline)?;
    if max_output_bytes > MAX_REPRESENTATION_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "inflate output limit exceeds implementation cap",
        ));
    }
    if input.len() > MAX_REPRESENTATION_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "compressed representation exceeds implementation cap",
        ));
    }
    if expected_bytes > max_output_bytes as u64 || expected_bytes > MAX_REPRESENTATION_BYTES as u64
    {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "inflate output exceeds limit",
        ));
    }

    check_attempt(cancellation, deadline)?;
    let expected = expected_bytes as usize;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(expected).map_err(|_| {
        safe_error(
            io::ErrorKind::OutOfMemory,
            "inflate output allocation failed",
        )
    })?;
    let mut scratch = [0u8; IO_CHUNK_BYTES];
    let mut decoder = Decompress::new(true);
    let mut calls = 0usize;

    loop {
        check_attempt(cancellation, deadline)?;
        calls = calls
            .checked_add(1)
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "inflate call count overflow"))?;
        if calls > MAX_DECOMPRESS_CALLS {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "inflate call limit exceeded",
            ));
        }

        let total_in_before = decoder.total_in();
        let total_out_before = decoder.total_out();
        if total_in_before > input.len() as u64 || total_out_before > expected_bytes {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "inflate counter exceeds supplied bounds",
            ));
        }
        let consumed = total_in_before as usize;
        let remaining_input = &input[consumed..];
        let input_chunk = &remaining_input[..remaining_input.len().min(IO_CHUNK_BYTES)];
        let produced_so_far = total_out_before as usize;
        let remaining_expected = expected - produced_so_far;
        let output_capacity = (remaining_expected + 1).min(IO_CHUNK_BYTES);

        check_attempt(cancellation, deadline)?;
        let decoded = decoder.decompress(
            input_chunk,
            &mut scratch[..output_capacity],
            FlushDecompress::None,
        );
        check_attempt(cancellation, deadline)?;
        let status = decoded
            .map_err(|_| safe_error(io::ErrorKind::InvalidData, "invalid zlib representation"))?;

        let total_in_after = decoder.total_in();
        let total_out_after = decoder.total_out();
        if total_in_after < total_in_before
            || total_out_after < total_out_before
            || total_in_after > input.len() as u64
            || total_in_after - total_in_before > input_chunk.len() as u64
            || total_out_after - total_out_before > output_capacity as u64
        {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "inflate counter exceeds supplied bounds",
            ));
        }

        let produced = (total_out_after - total_out_before) as usize;
        if total_out_after > expected_bytes || produced > remaining_expected {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "inflated representation exceeds declared size",
            ));
        }
        bytes.extend_from_slice(&scratch[..produced]);

        if status == Status::StreamEnd {
            if total_out_after != expected_bytes {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "inflated representation size mismatch",
                ));
            }
            check_attempt(cancellation, deadline)?;
            return Ok(InflatedRepresentation {
                bytes,
                compressed_bytes: total_in_after as usize,
            });
        }

        let progressed = total_in_after != total_in_before || total_out_after != total_out_before;
        if !progressed {
            let kind = if total_in_after as usize == input.len() {
                io::ErrorKind::UnexpectedEof
            } else {
                io::ErrorKind::InvalidData
            };
            return Err(safe_error(kind, "zlib stream made no progress"));
        }
    }
}

fn check_attempt(cancellation: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancellation.check()?;
    check_deadline(deadline)
}
