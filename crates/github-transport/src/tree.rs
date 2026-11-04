//! Bounded structural parsing for one supplied SHA-1 Git tree payload.
//!
//! This validates entry framing, canonical modes, names, ordering, and nonzero
//! object IDs. It does not establish that referenced objects exist, their
//! content types, full fsck validity, platform path safety, extraction safety,
//! graph completeness, or cleanup eligibility. Gitlinks are external commit
//! metadata and are not followed. Product complex-layout gates remain in force.
//! The 16 MiB, 4096-entry, and 1024-byte name limits are experimental parser
//! limits, not product scope. Callers must bind these bytes to a verified tree
//! hash and pack type and validate any relevant object graph.

use std::{collections::HashSet, io, time::Instant};

use crate::{check_deadline, safe_error, CancellationHandle};

const MAX_TREE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TREE_ENTRIES: usize = 4096;
const MAX_NAME_BYTES: usize = 1024;

/// The canonical mode class recorded by a Git tree entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitTreeMode {
    Directory,
    Regular,
    Executable,
    Symlink,
    Gitlink,
}

/// One structurally parsed tree entry. Its raw name borrows the input bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitTreeEntry<'a> {
    mode: GitTreeMode,
    name: &'a [u8],
    oid: [u8; 20],
}

impl<'a> GitTreeEntry<'a> {
    pub fn mode(&self) -> GitTreeMode {
        self.mode
    }

    pub fn name(&self) -> &'a [u8] {
        self.name
    }

    pub fn oid(&self) -> &[u8; 20] {
        &self.oid
    }
}

/// Parse a bounded raw SHA-1 tree payload, preserving each name as a slice of
/// `input`. Ordering follows Git's tree comparison, where a directory name
/// has a virtual trailing `/` and every other name has a virtual trailing NUL.
pub fn parse_sha1_tree<'a>(
    input: &'a [u8],
    max_entries: usize,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<Vec<GitTreeEntry<'a>>> {
    check_attempt(cancellation, deadline)?;
    if input.len() > MAX_TREE_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "tree byte limit exceeded",
        ));
    }
    if max_entries > MAX_TREE_ENTRIES {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "tree entry limit invalid",
        ));
    }

    check_attempt(cancellation, deadline)?;
    let mut entries: Vec<GitTreeEntry<'a>> = Vec::new();
    entries
        .try_reserve_exact(max_entries)
        .map_err(|_| safe_error(io::ErrorKind::OutOfMemory, "tree allocation failed"))?;
    check_attempt(cancellation, deadline)?;
    let mut names = HashSet::new();
    names
        .try_reserve(max_entries)
        .map_err(|_| safe_error(io::ErrorKind::OutOfMemory, "tree allocation failed"))?;

    let mut offset = 0usize;
    while offset < input.len() {
        check_attempt(cancellation, deadline)?;
        if entries.len() == max_entries {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "tree entry limit exceeded",
            ));
        }

        // A canonical mode occupies at most six ASCII digits and a space.
        let mode_start = offset;
        let mut mode_end = mode_start;
        while mode_end < input.len() && mode_end - mode_start <= 6 {
            let byte = input[mode_end];
            if byte == b' ' {
                break;
            }
            if !byte.is_ascii_digit() {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "tree mode malformed",
                ));
            }
            mode_end += 1;
        }
        if mode_end >= input.len() || input[mode_end] != b' ' || mode_end == mode_start {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "tree mode malformed",
            ));
        }
        let mode = match &input[mode_start..mode_end] {
            b"40000" => GitTreeMode::Directory,
            b"100644" => GitTreeMode::Regular,
            b"100755" => GitTreeMode::Executable,
            b"120000" => GitTreeMode::Symlink,
            b"160000" => GitTreeMode::Gitlink,
            _ => return Err(safe_error(io::ErrorKind::InvalidData, "tree mode invalid")),
        };
        let name_start = mode_end + 1;
        let scan_end = name_start
            .saturating_add(MAX_NAME_BYTES + 1)
            .min(input.len());
        let nul = input[name_start..scan_end]
            .iter()
            .position(|byte| *byte == 0)
            .map(|position| name_start + position)
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "tree name malformed"))?;
        let name = &input[name_start..nul];
        if name.is_empty() || name.len() > MAX_NAME_BYTES {
            return Err(safe_error(io::ErrorKind::InvalidData, "tree name invalid"));
        }
        if name.contains(&b'/') || name == b"." || name == b".." || is_git_name(name) {
            return Err(safe_error(io::ErrorKind::InvalidData, "tree name invalid"));
        }
        check_attempt(cancellation, deadline)?;
        if !names.insert(name) {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "tree name duplicated",
            ));
        }
        if let Some(previous) = entries.last() {
            if compare_git_names(previous.name, previous.mode, name, mode).is_ge() {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "tree entries unordered",
                ));
            }
        }

        let oid_start = nul + 1;
        let oid_end = oid_start
            .checked_add(20)
            .filter(|end| *end <= input.len())
            .ok_or_else(|| safe_error(io::ErrorKind::UnexpectedEof, "tree object ID truncated"))?;
        let mut oid = [0u8; 20];
        oid.copy_from_slice(&input[oid_start..oid_end]);
        if oid.iter().all(|byte| *byte == 0) {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "tree object ID invalid",
            ));
        }
        entries.push(GitTreeEntry { mode, name, oid });
        offset = oid_end;
        check_attempt(cancellation, deadline)?;
    }

    check_attempt(cancellation, deadline)?;
    Ok(entries)
}

fn is_git_name(name: &[u8]) -> bool {
    name.len() == 4
        && name
            .iter()
            .zip(b".git")
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn compare_git_names(
    a: &[u8],
    a_mode: GitTreeMode,
    b: &[u8],
    b_mode: GitTreeMode,
) -> std::cmp::Ordering {
    let a_term = if a_mode == GitTreeMode::Directory {
        b'/'
    } else {
        0
    };
    let b_term = if b_mode == GitTreeMode::Directory {
        b'/'
    } else {
        0
    };
    a.iter()
        .copied()
        .chain(std::iter::once(a_term))
        .cmp(b.iter().copied().chain(std::iter::once(b_term)))
}

fn check_attempt(cancellation: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancellation.check()?;
    check_deadline(deadline)
}
