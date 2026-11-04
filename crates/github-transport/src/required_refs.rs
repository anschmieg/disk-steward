//! Immutable, bounded manifest of caller-supplied required references.
//!
//! Entries are claims supplied by the caller. This manifest is not a local
//! reference inventory, symref state, backup, retention policy, signature, or
//! cleanup authorization. Its existing entry caps are experimental and are
//! not product scope.

use std::{io, time::Instant};

use gix_transport::bstr::ByteSlice;
use sha2::{Digest, Sha256};

use crate::{check_deadline, safe_error, CancellationHandle, GitObjectKind};

const MAX_ENTRIES: usize = 1024;
const MAX_NAME_BYTES: usize = 1024;
const MAX_TOTAL_NAME_BYTES: usize = 65_536;
const HASH_CHUNK_BYTES: usize = 8 * 1024;
const DOMAIN: &[u8] = b"disk-steward-required-refs-v1\0";

/// A borrowed required-reference claim. Names are raw Git reference bytes.
#[derive(Clone, Copy, Debug)]
pub struct RequiredRefInput<'a> {
    pub name: &'a [u8],
    pub oid: [u8; 20],
    pub kind: GitObjectKind,
}

/// One owned required-reference claim in canonical name order.
#[derive(Debug, PartialEq, Eq)]
pub struct RequiredRefEntry {
    name: Vec<u8>,
    oid: [u8; 20],
    kind: GitObjectKind,
}

impl RequiredRefEntry {
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    pub fn oid(&self) -> &[u8; 20] {
        &self.oid
    }

    pub fn kind(&self) -> GitObjectKind {
        self.kind
    }
}

/// Sorted immutable required-reference claims and their canonical SHA-256.
#[derive(Debug)]
pub struct RequiredRefManifest {
    entries: Vec<RequiredRefEntry>,
    digest: [u8; 32],
}

impl RequiredRefManifest {
    pub fn entries(&self) -> &[RequiredRefEntry] {
        &self.entries
    }

    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// Validate and copy bounded caller claims, then hash their canonical form.
    pub fn capture(
        inputs: &[RequiredRefInput<'_>],
        cancel: &CancellationHandle,
        deadline: Instant,
    ) -> io::Result<Self> {
        check_attempt(cancel, deadline)?;
        let mut total_name_bytes = 0usize;
        if inputs.is_empty() || inputs.len() > MAX_ENTRIES {
            return Err(invalid("invalid required reference manifest"));
        }
        for input in inputs {
            check_attempt(cancel, deadline)?;
            validate_claim(input)?;
            total_name_bytes = total_name_bytes
                .checked_add(input.name.len())
                .ok_or_else(|| invalid("invalid required reference manifest"))?;
            if total_name_bytes > MAX_TOTAL_NAME_BYTES {
                return Err(invalid("invalid required reference manifest"));
            }
        }

        check_attempt(cancel, deadline)?;
        let mut entries = Vec::new();
        entries
            .try_reserve(inputs.len())
            .map_err(|_| allocation_error())?;
        for input in inputs {
            check_attempt(cancel, deadline)?;
            let mut name = Vec::new();
            name.try_reserve(input.name.len())
                .map_err(|_| allocation_error())?;
            name.extend_from_slice(input.name);
            entries.push(RequiredRefEntry {
                name,
                oid: input.oid,
                kind: input.kind,
            });
        }

        check_attempt(cancel, deadline)?;
        entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        check_attempt(cancel, deadline)?;
        for pair in entries.windows(2) {
            check_attempt(cancel, deadline)?;
            if pair[0].name == pair[1].name {
                return Err(invalid("duplicate required reference"));
            }
        }
        check_attempt(cancel, deadline)?;
        let digest = hash_entries(&entries, cancel, deadline)?;
        check_attempt(cancel, deadline)?;
        Ok(Self { entries, digest })
    }

    /// Confirm that the complete canonical entry sequence has not changed.
    pub fn check_unchanged(
        &self,
        current: &Self,
        cancel: &CancellationHandle,
        deadline: Instant,
    ) -> io::Result<()> {
        check_attempt(cancel, deadline)?;
        let changed = self.entries.len() != current.entries.len();
        if changed {
            return Err(invalid("required ref manifest changed"));
        }
        for (expected, actual) in self.entries.iter().zip(&current.entries) {
            check_attempt(cancel, deadline)?;
            if expected != actual {
                return Err(invalid("required ref manifest changed"));
            }
        }
        check_attempt(cancel, deadline)?;
        Ok(())
    }
}

fn validate_claim(input: &RequiredRefInput<'_>) -> io::Result<()> {
    let name = input.name;
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || input.oid.iter().all(|byte| *byte == 0)
        || (name != b"HEAD"
            && (!name.starts_with(b"refs/")
                || gix_validate::reference::name(name.as_bstr()).is_err()))
        || (name.starts_with(b"refs/heads/")
            && gix_validate::reference::branch_name(name.as_bstr()).is_err())
        || ((name == b"HEAD" || name.starts_with(b"refs/heads/"))
            && input.kind != GitObjectKind::Commit)
    {
        return Err(invalid("invalid required reference manifest"));
    }
    Ok(())
}

fn hash_entries(
    entries: &[RequiredRefEntry],
    cancel: &CancellationHandle,
    deadline: Instant,
) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    hash_bytes(&mut hash, DOMAIN, cancel, deadline)?;
    hash_bytes(
        &mut hash,
        &(entries.len() as u32).to_be_bytes(),
        cancel,
        deadline,
    )?;
    for entry in entries {
        check_attempt(cancel, deadline)?;
        hash_bytes(
            &mut hash,
            &(entry.name.len() as u32).to_be_bytes(),
            cancel,
            deadline,
        )?;
        hash_bytes(&mut hash, &entry.name, cancel, deadline)?;
        hash_bytes(&mut hash, &[kind_byte(entry.kind)], cancel, deadline)?;
        hash_bytes(&mut hash, &entry.oid, cancel, deadline)?;
    }
    check_attempt(cancel, deadline)?;
    let digest = hash.finalize();
    check_attempt(cancel, deadline)?;
    Ok(digest.into())
}

fn hash_bytes(
    hash: &mut Sha256,
    bytes: &[u8],
    cancel: &CancellationHandle,
    deadline: Instant,
) -> io::Result<()> {
    for chunk in bytes.chunks(HASH_CHUNK_BYTES) {
        check_attempt(cancel, deadline)?;
        hash.update(chunk);
    }
    check_attempt(cancel, deadline)
}

const fn kind_byte(kind: GitObjectKind) -> u8 {
    match kind {
        GitObjectKind::Commit => 1,
        GitObjectKind::Tree => 2,
        GitObjectKind::Blob => 3,
        GitObjectKind::Tag => 4,
    }
}

fn check_attempt(cancel: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancel.check()?;
    check_deadline(deadline)?;
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    safe_error(io::ErrorKind::InvalidData, message)
}

fn allocation_error() -> io::Error {
    safe_error(
        io::ErrorKind::OutOfMemory,
        "required reference allocation failed",
    )
}
