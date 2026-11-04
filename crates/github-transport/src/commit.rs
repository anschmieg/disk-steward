//! Bounded structural parsing for a supplied SHA-1 commit payload.
//!
//! The result is link metadata only. This parser does not bind bytes to a
//! commit hash or pack type, establish referenced-object presence or type,
//! validate author identity, verify signatures, perform full fsck, validate a
//! graph, or establish cleanup eligibility. These experimental limits and
//! strict subset may reject noncanonical historical objects. Optional headers
//! (including signature and merge-tag headers) are uninterpreted and unverified.
//! See [Git Objects](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects)
//! and [gitformat-signature](https://git-scm.com/docs/gitformat-signature) for
//! object and signature formats.

use std::{io, time::Instant};

use crate::{check_deadline, safe_error, CancellationHandle};

const MAX_COMMIT_BYTES: usize = 16 * 1024 * 1024;
const MAX_PARENTS: usize = 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_LINE_BYTES: usize = 8192;
const MAX_PHYSICAL_LINES: usize = 2048;

/// Structurally parsed tree and parent links from a supplied commit payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommitLinks {
    tree: [u8; 20],
    parents: Vec<[u8; 20]>,
    header_bytes: usize,
}

impl GitCommitLinks {
    pub fn tree(&self) -> &[u8; 20] {
        &self.tree
    }

    pub fn parents(&self) -> &[[u8; 20]] {
        &self.parents
    }

    pub fn header_bytes(&self) -> usize {
        self.header_bytes
    }
}

/// Parse the bounded header of one raw SHA-1 commit payload.
pub fn parse_sha1_commit(
    input: &[u8],
    max_parents: usize,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<GitCommitLinks> {
    check_attempt(cancellation, deadline)?;
    if input.len() > MAX_COMMIT_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "commit byte limit exceeded",
        ));
    }
    if max_parents > MAX_PARENTS {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "commit parent limit invalid",
        ));
    }

    check_attempt(cancellation, deadline)?;
    let mut parents = Vec::new();
    parents
        .try_reserve_exact(max_parents)
        .map_err(|_| safe_error(io::ErrorKind::OutOfMemory, "commit allocation failed"))?;
    check_attempt(cancellation, deadline)?;

    let mut offset = 0usize;
    let mut header_bytes = 0usize;
    let mut lines = 0usize;
    let tree_line = read_header_line(
        input,
        &mut offset,
        &mut header_bytes,
        &mut lines,
        cancellation,
        deadline,
    )?;
    let tree = parse_core_oid(tree_line, b"tree ")
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "commit tree header malformed"))?;

    let author_line = loop {
        let line = read_header_line(
            input,
            &mut offset,
            &mut header_bytes,
            &mut lines,
            cancellation,
            deadline,
        )?;
        if line.starts_with(b"parent ") {
            if parents.len() >= max_parents {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "commit parent limit exceeded",
                ));
            }
            let oid = parse_core_oid(line, b"parent ").ok_or_else(|| {
                safe_error(io::ErrorKind::InvalidData, "commit parent header malformed")
            })?;
            parents.push(oid);
            continue;
        }
        if line.starts_with(b"tree ")
            || line.starts_with(b"author ")
            || line.starts_with(b"committer ")
        {
            if line.starts_with(b"author ") {
                break line;
            }
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "commit core header misplaced",
            ));
        }
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "commit author header missing",
        ));
    };
    if author_line.len() <= b"author ".len() {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "commit author header malformed",
        ));
    }

    let committer_line = read_header_line(
        input,
        &mut offset,
        &mut header_bytes,
        &mut lines,
        cancellation,
        deadline,
    )?;
    if !committer_line.starts_with(b"committer ") || committer_line.len() <= b"committer ".len() {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "commit committer header malformed",
        ));
    }

    let mut previous_optional = false;
    loop {
        let line = read_header_line(
            input,
            &mut offset,
            &mut header_bytes,
            &mut lines,
            cancellation,
            deadline,
        )?;
        if line.is_empty() {
            check_attempt(cancellation, deadline)?;
            return Ok(GitCommitLinks {
                tree,
                parents,
                header_bytes,
            });
        }
        if line[0] == b' ' {
            if !previous_optional {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "commit continuation misplaced",
                ));
            }
            continue;
        }
        if is_core_key(line) {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "commit core header duplicated",
            ));
        }
        let Some(space) = line.iter().position(|byte| *byte == b' ') else {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "commit optional header malformed",
            ));
        };
        if space == 0
            || space + 1 == line.len()
            || !line[..space]
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "commit optional header malformed",
            ));
        }
        previous_optional = true;
    }
}

fn read_header_line<'a>(
    input: &'a [u8],
    offset: &mut usize,
    header_bytes: &mut usize,
    lines: &mut usize,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<&'a [u8]> {
    check_attempt(cancellation, deadline)?;
    if *lines >= MAX_PHYSICAL_LINES || *header_bytes >= MAX_HEADER_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "commit header limit exceeded",
        ));
    }
    let remaining = MAX_HEADER_BYTES - *header_bytes;
    let search_len = input
        .len()
        .saturating_sub(*offset)
        .min(MAX_LINE_BYTES + 1)
        .min(remaining);
    let slice = &input[*offset..*offset + search_len];
    let newline = slice
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "commit header line malformed"))?;
    check_attempt(cancellation, deadline)?;
    if newline > MAX_LINE_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "commit header line limit exceeded",
        ));
    }
    let line = &slice[..newline];
    if line.contains(&0) || line.contains(&b'\r') {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "commit header line malformed",
        ));
    }
    let consumed = newline + 1;
    *header_bytes += consumed;
    *offset += consumed;
    *lines += 1;
    check_attempt(cancellation, deadline)?;
    Ok(line)
}

fn parse_core_oid(line: &[u8], prefix: &[u8]) -> Option<[u8; 20]> {
    let hex = line.strip_prefix(prefix)?;
    if hex.len() != 40 {
        return None;
    }
    let mut oid = [0u8; 20];
    for (index, pair) in hex.as_chunks::<2>().0.iter().enumerate() {
        let high = lower_hex(pair[0])?;
        let low = lower_hex(pair[1])?;
        oid[index] = (high << 4) | low;
    }
    if oid.iter().all(|byte| *byte == 0) {
        return None;
    }
    Some(oid)
}

fn lower_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn is_core_key(line: &[u8]) -> bool {
    [b"tree".as_slice(), b"parent", b"author", b"committer"]
        .iter()
        .any(|key| {
            line.strip_prefix(*key)
                .is_some_and(|tail| tail.first() == Some(&b' '))
        })
}

fn check_attempt(cancellation: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancellation.check()?;
    check_deadline(deadline)
}
