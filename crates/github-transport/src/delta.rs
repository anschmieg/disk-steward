//! Bounded reconstruction of one already-inflated Git delta instruction stream.
//!
//! This applies the stream to the supplied base bytes. It does not inflate a
//! packed delta, resolve a base object or delta chain, or verify a base ID,
//! object ID, object type, graph, or pack completeness. Callers remain
//! responsible for aggregate memory limits, chain depth, and outer worker
//! controls. The 16 MiB implementation limit is not a product default.

use std::{io, time::Instant};

use crate::{check_deadline, safe_error, CancellationHandle};

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_INSTRUCTIONS: usize = 1_048_576;
const COPY_CHUNK_BYTES: usize = 8 * 1024;

/// Reconstruct one Git delta instruction stream against `base`.
///
/// The delta format starts with base and result sizes encoded as little-endian
/// 7-bit varints. Copy instructions reference byte ranges in `base`; insert
/// instructions carry literal bytes. Both inputs and the declared result are
/// bounded to 16 MiB, and `max_result_bytes` must itself be at most 16 MiB.
pub fn reconstruct_git_delta(
    base: &[u8],
    delta: &[u8],
    max_result_bytes: usize,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    check_attempt(cancellation, deadline)?;
    if max_result_bytes > MAX_INPUT_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "delta result limit exceeds implementation cap",
        ));
    }
    if base.len() > MAX_INPUT_BYTES || delta.len() > MAX_INPUT_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "delta input exceeds implementation cap",
        ));
    }

    let mut cursor = 0usize;
    let declared_base = read_varint(delta, &mut cursor)?;
    let declared_result = read_varint(delta, &mut cursor)?;
    if declared_base != base.len() as u64 {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "delta base size mismatch",
        ));
    }
    if declared_result > max_result_bytes as u64 || declared_result > MAX_INPUT_BYTES as u64 {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "delta result exceeds limit",
        ));
    }
    let declared_result = declared_result as usize;

    check_attempt(cancellation, deadline)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(declared_result)
        .map_err(|_| safe_error(io::ErrorKind::OutOfMemory, "delta result allocation failed"))?;
    let mut instructions = 0usize;

    while cursor < delta.len() {
        check_attempt(cancellation, deadline)?;
        instructions = instructions.checked_add(1).ok_or_else(|| {
            safe_error(
                io::ErrorKind::InvalidData,
                "delta instruction count overflow",
            )
        })?;
        if instructions > MAX_INSTRUCTIONS {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "delta instruction limit exceeded",
            ));
        }

        let opcode = delta[cursor];
        cursor += 1;
        if opcode == 0 {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "reserved delta opcode",
            ));
        }
        if opcode & 0x80 == 0 {
            let literal_len = opcode as usize;
            let literal_end = cursor.checked_add(literal_len).ok_or_else(|| {
                safe_error(io::ErrorKind::InvalidData, "delta literal range overflow")
            })?;
            if literal_end > delta.len() {
                return Err(safe_error(
                    io::ErrorKind::UnexpectedEof,
                    "truncated delta literal",
                ));
            }
            ensure_output_room(output.len(), literal_len, declared_result)?;
            append_checked(
                &mut output,
                &delta[cursor..literal_end],
                cancellation,
                deadline,
            )?;
            cursor = literal_end;
            continue;
        }

        let mut offset = 0usize;
        let mut size = 0usize;
        for bit in 0..4 {
            if opcode & (1 << bit) != 0 {
                let byte = read_instruction_byte(delta, &mut cursor)? as usize;
                offset |= byte << (bit * 8);
            }
        }
        for bit in 0..3 {
            if opcode & (1 << (bit + 4)) != 0 {
                let byte = read_instruction_byte(delta, &mut cursor)? as usize;
                size |= byte << (bit * 8);
            }
        }
        if size == 0 {
            size = 0x1_0000;
        }
        let base_end = offset
            .checked_add(size)
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "delta copy range overflow"))?;
        if base_end > base.len() {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "delta copy exceeds base",
            ));
        }
        ensure_output_room(output.len(), size, declared_result)?;
        append_checked(&mut output, &base[offset..base_end], cancellation, deadline)?;
    }

    check_attempt(cancellation, deadline)?;
    if output.len() != declared_result {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "delta result size mismatch",
        ));
    }
    Ok(output)
}

fn read_varint(bytes: &[u8], cursor: &mut usize) -> io::Result<u64> {
    let mut value = 0u64;
    for index in 0..10 {
        let byte = read_instruction_byte(bytes, cursor)?;
        let payload = (byte & 0x7f) as u64;
        let shift = index * 7;
        if shift == 63 && payload > 1 {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "delta size varint overflow",
            ));
        }
        value |= payload
            .checked_shl(shift as u32)
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "delta size varint overflow"))?;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(safe_error(
        io::ErrorKind::InvalidData,
        "delta size varint exceeds 10 bytes",
    ))
}

fn read_instruction_byte(bytes: &[u8], cursor: &mut usize) -> io::Result<u8> {
    let byte = bytes
        .get(*cursor)
        .copied()
        .ok_or_else(|| safe_error(io::ErrorKind::UnexpectedEof, "truncated delta instruction"))?;
    *cursor += 1;
    Ok(byte)
}

fn ensure_output_room(current: usize, additional: usize, declared: usize) -> io::Result<()> {
    let end = current
        .checked_add(additional)
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "delta output size overflow"))?;
    if end > declared {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "delta instruction exceeds declared result",
        ));
    }
    Ok(())
}

fn append_checked(
    output: &mut Vec<u8>,
    source: &[u8],
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<()> {
    for chunk in source.chunks(COPY_CHUNK_BYTES) {
        check_attempt(cancellation, deadline)?;
        output.extend_from_slice(chunk);
    }
    Ok(())
}

fn check_attempt(cancellation: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancellation.check()?;
    check_deadline(deadline)
}
