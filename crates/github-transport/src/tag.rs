//! Bounded structural parsing for one supplied SHA-1 annotated-tag payload.
//!
//! This is an experimental link parser. It does not bind bytes to an object
//! identity or pack type, look up or peel the target, verify signatures or
//! identities, perform full fsck, validate a graph, or establish cleanup
//! eligibility. Message and signature bytes are opaque. The strict subset and
//! byte limits are parser bounds, not product scope. See
//! [git-mktag](https://git-scm.com/docs/git-mktag) and
//! [gitformat-signature](https://git-scm.com/docs/gitformat-signature).

use std::{io, time::Instant};

use gix_transport::bstr::ByteSlice;

use crate::{check_deadline, safe_error, CancellationHandle, GitObjectKind};

const MAX_TAG_BYTES: usize = 16 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_LINE_BYTES: usize = 8192;
const MAX_NAME_BYTES: usize = 1024;

/// Structural target link and raw tag name borrowed from the supplied payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitTagLink<'a> {
    target: [u8; 20],
    target_kind: GitObjectKind,
    name: &'a [u8],
    header_bytes: usize,
}

impl<'a> GitTagLink<'a> {
    pub fn target(&self) -> &[u8; 20] {
        &self.target
    }
    pub fn target_kind(&self) -> GitObjectKind {
        self.target_kind
    }
    pub fn name(&self) -> &'a [u8] {
        self.name
    }
    pub fn header_bytes(&self) -> usize {
        self.header_bytes
    }
}

/// Parse the bounded, supported header subset of one raw SHA-1 tag payload.
pub fn parse_sha1_tag<'a>(
    input: &'a [u8],
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<GitTagLink<'a>> {
    check_attempt(cancellation, deadline)?;
    if input.len() > MAX_TAG_BYTES {
        return Err(invalid("tag byte limit exceeded"));
    }

    let mut offset = 0usize;
    let mut header_bytes = 0usize;
    let object = read_header_line(
        input,
        &mut offset,
        &mut header_bytes,
        cancellation,
        deadline,
    )?;
    let target = parse_object_line(object).ok_or_else(|| invalid("tag object header malformed"))?;
    let kind_line = read_header_line(
        input,
        &mut offset,
        &mut header_bytes,
        cancellation,
        deadline,
    )?;
    let target_kind = match kind_line {
        b"type commit" => GitObjectKind::Commit,
        b"type tree" => GitObjectKind::Tree,
        b"type blob" => GitObjectKind::Blob,
        b"type tag" => GitObjectKind::Tag,
        _ => return Err(invalid("tag type header malformed")),
    };
    let tag_line = read_header_line(
        input,
        &mut offset,
        &mut header_bytes,
        cancellation,
        deadline,
    )?;
    let name = tag_line
        .strip_prefix(b"tag ")
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid("tag name header malformed"))?;
    if name.len() > MAX_NAME_BYTES {
        return Err(invalid("tag name limit exceeded"));
    }

    let line = read_header_line(
        input,
        &mut offset,
        &mut header_bytes,
        cancellation,
        deadline,
    )?;
    let blank = if line.is_empty() {
        true
    } else {
        if !line.starts_with(b"tagger ") || line.len() == b"tagger ".len() {
            return Err(invalid("tag header unsupported"));
        }
        read_header_line(
            input,
            &mut offset,
            &mut header_bytes,
            cancellation,
            deadline,
        )?
        .is_empty()
    };
    if !blank {
        return Err(invalid("tag header malformed"));
    }

    check_attempt(cancellation, deadline)?;
    if gix_validate::tag::name(ByteSlice::as_bstr(name)).is_err() {
        return Err(invalid("tag name invalid"));
    }
    check_attempt(cancellation, deadline)?;
    Ok(GitTagLink {
        target,
        target_kind,
        name,
        header_bytes,
    })
}

fn read_header_line<'a>(
    input: &'a [u8],
    offset: &mut usize,
    header_bytes: &mut usize,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<&'a [u8]> {
    check_attempt(cancellation, deadline)?;
    if *offset >= input.len() {
        return Err(invalid("tag header truncated"));
    }
    let remaining = input.len() - *offset;
    let scan_len = remaining.min(MAX_LINE_BYTES + 1);
    let slice = &input[*offset..*offset + scan_len];
    let lf = slice
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| invalid("tag header line malformed"))?;
    if lf > MAX_LINE_BYTES {
        return Err(invalid("tag header line limit exceeded"));
    }
    let line = &slice[..lf];
    if line.iter().any(|byte| *byte == 0 || *byte == b'\r') {
        return Err(invalid("tag header line invalid"));
    }
    let line_size = lf + 1;
    *header_bytes = header_bytes
        .checked_add(line_size)
        .filter(|size| *size <= MAX_HEADER_BYTES)
        .ok_or_else(|| invalid("tag header limit exceeded"))?;
    *offset += line_size;
    check_attempt(cancellation, deadline)?;
    Ok(line)
}

fn parse_object_line(line: &[u8]) -> Option<[u8; 20]> {
    let hex = line.strip_prefix(b"object ")?;
    if hex.len() != 40 {
        return None;
    }
    let mut oid = [0u8; 20];
    let mut nonzero = false;
    for (index, pair) in hex.as_chunks::<2>().0.iter().enumerate() {
        let high = lower_hex(pair[0])?;
        let low = lower_hex(pair[1])?;
        let byte = (high << 4) | low;
        nonzero |= byte != 0;
        oid[index] = byte;
    }
    nonzero.then_some(oid)
}

fn lower_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn check_attempt(cancellation: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancellation.check()?;
    check_deadline(deadline)
}

fn invalid(message: &'static str) -> io::Error {
    safe_error(io::ErrorKind::InvalidData, message)
}
