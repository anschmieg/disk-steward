//! Experimental closure checking for objects named by one completed pack transfer.
//!
//! This validates advertised roots and tag peel claims while binding the supplied
//! bytes to the transfer's accepted-byte SHA-256 digest. This is not full fsck,
//! an atomic remote snapshot, or proof of physical sink persistence. A sink's
//! accepted byte count remains its own contract. This establishes no local
//! preservation or cleanup eligibility and provides no RSS proof. These bounds
//! are experimental, not product scope.

use sha2::{Digest, Sha256};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use crate::{
    check_deadline, check_pack_graph_closure, decode_sha1_pack, parse_sha1_tag, safe_error,
    AdvertisedRef, CancellationHandle, DecodedPack, GitObjectKind, PackGraphReport,
    PackTransferReport,
};

const MAX_REFS: usize = 1024;
const MAX_ROOTS: usize = 64;
const MAX_PEEL_STEPS: usize = 16_384;

/// A decoded pack and graph closure report for advertised roots.
#[derive(Debug)]
pub struct AdvertisedPackReport {
    context: crate::attempt_context::AttemptContext,
    decoded: DecodedPack,
    graph: PackGraphReport,
    advertised_ref_count: usize,
    roots: Vec<([u8; 20], GitObjectKind)>,
}

/// Bounded presence/type reachability for a caller-supplied required object set.
/// Counts do not capture or attest a local repository inventory or ref identities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequiredObjectCoverageReport {
    required_object_count: usize,
    graph: PackGraphReport,
}
impl RequiredObjectCoverageReport {
    /// Number of supplied requests, including duplicates.
    pub fn required_object_count(&self) -> usize {
        self.required_object_count
    }
    pub fn graph(&self) -> PackGraphReport {
        self.graph
    }
}

/// How a caller-supplied local ref name differs from this advertisement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteRefMismatch {
    MissingName,
    DifferentTarget,
    DifferentSymrefTarget,
}

/// Reachable typed objects plus names that still need explicit disposition.
/// Matching remote names do not promise retention or preserve local symref state.
#[derive(Debug)]
pub struct NamedRefCoverageReport<'a> {
    manifest: &'a crate::RequiredRefManifest,
    coverage: RequiredObjectCoverageReport,
    unmatched: Vec<(usize, RemoteRefMismatch)>,
    context: crate::attempt_context::AttemptContext,
    manifest_changed: AtomicBool,
}
impl<'a> NamedRefCoverageReport<'a> {
    pub fn manifest(&self) -> &'a crate::RequiredRefManifest {
        self.manifest
    }
    pub fn coverage(&self) -> RequiredObjectCoverageReport {
        self.coverage
    }
    pub fn unmatched_remote_refs(
        &self,
    ) -> impl ExactSizeIterator<Item = (&crate::RequiredRefEntry, RemoteRefMismatch)> {
        self.unmatched
            .iter()
            .map(|(index, reason)| (&self.manifest.entries()[*index], *reason))
    }

    /// Recheck this report's immutable required-ref claims against a caller's
    /// current manifest. The supplied manifest remains a claim; this does not
    /// read native repository refs or create a persisted receipt, atomic
    /// snapshot, backup, or cleanup authorization. A changed source requires
    /// new analysis and evidence; this report cannot be automatically retried.
    pub fn recheck_manifest(
        &self,
        expected_repository: &crate::RepositoryUrl,
        current: &crate::RequiredRefManifest,
        cancellation: &CancellationHandle,
        deadline: Instant,
    ) -> io::Result<()> {
        check_attempt(cancellation, deadline)?;
        self.context.check(expected_repository)?;
        if self.manifest_changed.load(Ordering::Acquire) {
            return Err(invalid("required ref manifest changed"));
        }
        if let Err(error) = self
            .manifest
            .check_unchanged(current, cancellation, deadline)
        {
            if error.kind() == io::ErrorKind::InvalidData {
                self.manifest_changed.store(true, Ordering::Release);
            }
            return Err(error);
        }
        check_attempt(cancellation, deadline)?;
        self.context.check(expected_repository)?;
        if self.manifest_changed.load(Ordering::Acquire) {
            return Err(invalid("required ref manifest changed"));
        }
        Ok(())
    }
}

impl AdvertisedPackReport {
    /// Check all manifest IDs and retain names not identically advertised by
    /// the same attempt. Caller claims are not a stable local inventory or backup.
    pub fn check_required_refs<'a>(
        &self,
        expected_repository: &crate::RepositoryUrl,
        transfer: &PackTransferReport,
        manifest: &'a crate::RequiredRefManifest,
        cancellation: &CancellationHandle,
        deadline: Instant,
    ) -> io::Result<NamedRefCoverageReport<'a>> {
        check_attempt(cancellation, deadline)?;
        transfer.check_context(expected_repository)?;
        self.check_context(expected_repository)?;
        if &self.context != transfer.context() {
            return Err(invalid("verification attempt mismatch"));
        }
        let mut required = Vec::new();
        required
            .try_reserve_exact(manifest.entries().len())
            .map_err(|_| {
                safe_error(io::ErrorKind::OutOfMemory, "required ref allocation failed")
            })?;
        for entry in manifest.entries() {
            check_attempt(cancellation, deadline)?;
            required.push((*entry.oid(), entry.kind()));
        }
        let coverage =
            self.check_required_objects(expected_repository, &required, cancellation, deadline)?;
        let refs = transfer
            .advertised_refs()
            .ok_or_else(|| invalid("advertised refs unavailable"))?;
        let mut unmatched = Vec::new();
        unmatched
            .try_reserve_exact(manifest.entries().len())
            .map_err(|_| {
                safe_error(
                    io::ErrorKind::OutOfMemory,
                    "required name allocation failed",
                )
            })?;
        for (index, entry) in manifest.entries().iter().enumerate() {
            check_attempt(cancellation, deadline)?;
            let mut found = None;
            for advertised in refs {
                check_attempt(cancellation, deadline)?;
                if advertised.name() == entry.name() {
                    found = Some(advertised);
                    break;
                }
            }
            match found {
                None => unmatched.push((index, RemoteRefMismatch::MissingName)),
                Some(advertised) if advertised.oid() != entry.oid() => {
                    unmatched.push((index, RemoteRefMismatch::DifferentTarget))
                }
                Some(advertised) if advertised.symref_target() != entry.symref_target() => {
                    unmatched.push((index, RemoteRefMismatch::DifferentSymrefTarget))
                }
                _ => {}
            }
        }
        check_attempt(cancellation, deadline)?;
        self.check_context(expected_repository)?;
        transfer.check_context(expected_repository)?;
        Ok(NamedRefCoverageReport {
            manifest,
            coverage,
            unmatched,
            context: self.context.clone(),
            manifest_changed: AtomicBool::new(false),
        })
    }

    pub fn repository(&self) -> &crate::RepositoryUrl {
        self.context.repository()
    }

    /// Recheck exact repository binding and in-process dual-clock expiry.
    /// Observed clock failures remain terminal across this attempt's clones.
    pub fn check_context(&self, expected: &crate::RepositoryUrl) -> io::Result<()> {
        self.context.check(expected)
    }

    /// Require each supplied typed object to be reachable from this report's
    /// advertised roots. Mere presence elsewhere in the pack is insufficient.
    ///
    /// The set must contain 1..=1024 nonzero IDs. Duplicate requests count toward
    /// the limit and each expected kind is checked. This re-runs bounded graph
    /// traversal with cooperative controls. It does not inventory local work,
    /// preserve ref names, prove receipt freshness or authorize cleanup.
    pub fn check_required_objects(
        &self,
        expected_repository: &crate::RepositoryUrl,
        required: &[([u8; 20], GitObjectKind)],
        cancellation: &CancellationHandle,
        deadline: Instant,
    ) -> io::Result<RequiredObjectCoverageReport> {
        cancellation.check()?;
        check_deadline(deadline)?;
        self.check_context(expected_repository)?;
        let graph = crate::graph::check_pack_graph_coverage(
            &self.decoded,
            &self.roots,
            required,
            cancellation,
            deadline,
        )?;
        self.check_context(expected_repository)?;
        Ok(RequiredObjectCoverageReport {
            required_object_count: required.len(),
            graph,
        })
    }

    pub fn decoded(&self) -> &DecodedPack {
        &self.decoded
    }

    pub fn graph(&self) -> PackGraphReport {
        self.graph
    }

    pub fn advertised_ref_count(&self) -> usize {
        self.advertised_ref_count
    }
}

/// Decode a completed transfer and check closure from its advertised references.
pub fn decode_advertised_pack(
    report: &PackTransferReport,
    expected_repository: &crate::RepositoryUrl,
    input: &[u8],
    max_decoded_bytes: usize,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<AdvertisedPackReport> {
    check_attempt(cancellation, deadline)?;
    let input_len = u64::try_from(input.len())
        .map_err(|_| safe_error(io::ErrorKind::InvalidInput, "pack input length invalid"))?;
    if !report.is_complete_transfer() || report.bytes_written() != input_len {
        return Err(invalid("pack transfer evidence does not match input"));
    }
    report.check_context(expected_repository)?;
    let refs = report
        .advertised_refs()
        .filter(|refs| !refs.is_empty() && refs.len() <= MAX_REFS)
        .ok_or_else(|| invalid("advertised reference set invalid"))?;
    validate_advertised_symrefs(refs, cancellation, deadline)?;

    if input.len() > crate::decode::MAX_PACK_BYTES {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "pack exceeds decoder input limit",
        ));
    }
    let mut digest = Sha256::new();
    for chunk in input.chunks(8192) {
        check_attempt(cancellation, deadline)?;
        digest.update(chunk);
    }
    check_attempt(cancellation, deadline)?;
    let observed: [u8; 32] = digest.finalize().into();
    check_attempt(cancellation, deadline)?;
    if report.accepted_pack_sha256() != Some(&observed) {
        return Err(invalid("pack bytes do not match transfer digest"));
    }

    let decoded = decode_sha1_pack(input, max_decoded_bytes, cancellation, deadline)?;
    let objects = decoded.objects();
    if objects.len() > MAX_REFS {
        return Err(invalid(
            "decoded object count exceeds advertised graph limit",
        ));
    }

    let mut roots = Vec::new();
    roots.try_reserve_exact(MAX_ROOTS).map_err(|_| {
        safe_error(
            io::ErrorKind::OutOfMemory,
            "advertised root allocation failed",
        )
    })?;

    let mut peel_steps = 0usize;
    for advertised in refs {
        check_attempt(cancellation, deadline)?;
        let object =
            find_object(objects, advertised.oid(), cancellation, deadline)?.ok_or_else(|| {
                safe_error(
                    io::ErrorKind::NotFound,
                    "advertised object missing from pack",
                )
            })?;
        let required_kind =
            if advertised.name() == b"HEAD" || advertised.name().starts_with(b"refs/heads/") {
                GitObjectKind::Commit
            } else {
                object.kind()
            };
        if object.kind() != required_kind {
            return Err(invalid("advertised object has unexpected type"));
        }

        if !roots.iter().any(|(oid, _)| oid == advertised.oid()) {
            if roots.len() == MAX_ROOTS {
                return Err(invalid("advertised root limit exceeded"));
            }
            roots.push((*advertised.oid(), required_kind));
        }

        if let Some(peeled) = advertised.peeled() {
            check_peeled(
                objects,
                object,
                peeled,
                &mut peel_steps,
                cancellation,
                deadline,
            )?;
        }
        check_attempt(cancellation, deadline)?;
    }

    let graph = check_pack_graph_closure(&decoded, &roots, cancellation, deadline)?;
    check_attempt(cancellation, deadline)?;
    report.check_context(expected_repository)?;
    Ok(AdvertisedPackReport {
        context: report.context().clone(),
        decoded,
        graph,
        advertised_ref_count: refs.len(),
        roots,
    })
}

fn find_object<'a>(
    objects: &'a [crate::DecodedPackObject],
    oid: &[u8; 20],
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<Option<&'a crate::DecodedPackObject>> {
    let mut found = None;
    for object in objects {
        check_attempt(cancellation, deadline)?;
        if object.oid() == oid {
            found = Some(object);
            break;
        }
    }
    check_attempt(cancellation, deadline)?;
    Ok(found)
}

fn validate_advertised_symrefs(
    refs: &[AdvertisedRef],
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<()> {
    const UNSEEN: u8 = 0;
    const VISITING: u8 = 1;
    const DONE: u8 = 2;

    if refs.len() > MAX_REFS {
        return Err(invalid("advertised reference set invalid"));
    }
    let mut states = [UNSEEN; MAX_REFS];
    let mut path = [0usize; MAX_REFS];
    for start in 0..refs.len() {
        check_attempt(cancellation, deadline)?;
        if states[start] == DONE {
            continue;
        }
        let mut current = start;
        let mut path_len = 0usize;
        loop {
            check_attempt(cancellation, deadline)?;
            match states[current] {
                VISITING => return Err(invalid("advertised symbolic reference cycle")),
                DONE => break,
                UNSEEN => {}
                _ => return Err(invalid("invalid advertised symbolic reference state")),
            }

            states[current] = VISITING;
            path[path_len] = current;
            path_len += 1;
            let reference = &refs[current];
            let Some(target_name) = reference.symref_target() else {
                break;
            };
            let target_index = find_ref_index(refs, target_name, cancellation, deadline)?
                .ok_or_else(|| invalid("symref target is not advertised"))?;
            if refs[target_index].oid() != reference.oid() {
                return Err(invalid("symref target object does not match"));
            }
            current = target_index;
        }

        while path_len > 0 {
            check_attempt(cancellation, deadline)?;
            path_len -= 1;
            states[path[path_len]] = DONE;
        }
    }
    check_attempt(cancellation, deadline)
}

fn find_ref_index(
    refs: &[AdvertisedRef],
    name: &[u8],
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<Option<usize>> {
    for (index, candidate) in refs.iter().enumerate() {
        check_attempt(cancellation, deadline)?;
        if candidate.name() == name {
            check_attempt(cancellation, deadline)?;
            return Ok(Some(index));
        }
    }
    check_attempt(cancellation, deadline)?;
    Ok(None)
}

fn check_peeled(
    objects: &[crate::DecodedPackObject],
    start: &crate::DecodedPackObject,
    claimed: &[u8; 20],
    peel_steps: &mut usize,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<()> {
    if start.kind() != GitObjectKind::Tag {
        return Err(invalid("peeled claim does not start at a tag"));
    }
    let mut current = start;
    for _ in 0..objects.len().min(MAX_REFS) {
        check_attempt(cancellation, deadline)?;
        if current.kind() != GitObjectKind::Tag {
            return if current.oid() == claimed {
                Ok(())
            } else {
                Err(invalid("peeled object does not match claim"))
            };
        }
        *peel_steps = peel_steps
            .checked_add(1)
            .filter(|steps| *steps <= MAX_PEEL_STEPS)
            .ok_or_else(|| invalid("cumulative tag peel limit exceeded"))?;
        let link = parse_sha1_tag(current.content(), cancellation, deadline)?;
        check_attempt(cancellation, deadline)?;
        current = find_object(objects, link.target(), cancellation, deadline)?
            .ok_or_else(|| safe_error(io::ErrorKind::NotFound, "tag target missing from pack"))?;
        if current.kind() != link.target_kind() {
            return Err(invalid("tag target has unexpected type"));
        }
        check_attempt(cancellation, deadline)?;
    }
    Err(invalid("tag chain exceeds supported bound"))
}

fn check_attempt(cancellation: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancellation.check()?;
    check_deadline(deadline)
}

fn invalid(message: &'static str) -> io::Error {
    safe_error(io::ErrorKind::InvalidData, message)
}
