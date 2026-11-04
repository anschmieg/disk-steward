//! Experimental in-memory presence and type closure for selected pack roots.
//!
//! This checks only objects reachable from caller-supplied roots. Unselected
//! objects are not syntax-checked, and this is not a full fsck. It proves no
//! root provenance or authenticity and provides no cleanup or persistence
//! proof. The limits below are experimental bounds, not product scope.

use std::{collections::HashMap, io, time::Instant};

use crate::{
    check_deadline, parse_sha1_commit, parse_sha1_tag, parse_sha1_tree, safe_error,
    CancellationHandle, DecodedPack, GitObjectKind, GitTreeMode,
};

const MAX_ROOTS: usize = 64;
const MAX_PARENTS: usize = 1024;
const MAX_TREE_ENTRIES: usize = 4096;
const MAX_EDGES: usize = 16_384;

/// Counts produced by a successful reachable-object closure check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackGraphReport {
    root_count: usize,
    reachable_objects: usize,
    checked_edges: usize,
}

impl PackGraphReport {
    pub fn root_count(&self) -> usize {
        self.root_count
    }

    pub fn reachable_objects(&self) -> usize {
        self.reachable_objects
    }

    pub fn checked_edges(&self) -> usize {
        self.checked_edges
    }
}

/// Check presence and declared type for every object reachable from `roots`.
///
/// Edges are counted with repetitions. Duplicate pack entries with an identical
/// object ID, kind, and content are represented by their first entry; conflicting
/// entries for one ID are rejected before traversal.
pub fn check_pack_graph_closure(
    pack: &DecodedPack,
    roots: &[([u8; 20], GitObjectKind)],
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<PackGraphReport> {
    check_pack_graph(pack, roots, &[], cancellation, deadline)
}

/// Check that each required object is present with its declared type and is
/// reachable from `roots`. Caller-supplied required claims are not local
/// inventory, provenance, or cleanup proof.
pub(crate) fn check_pack_graph_coverage(
    pack: &DecodedPack,
    roots: &[([u8; 20], GitObjectKind)],
    required: &[([u8; 20], GitObjectKind)],
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<PackGraphReport> {
    check_attempt(cancellation, deadline)?;
    if required.is_empty() || required.len() > 1024 {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "required object count invalid",
        ));
    }
    for (oid, _) in required {
        check_attempt(cancellation, deadline)?;
        if oid.iter().all(|byte| *byte == 0) {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "required object ID invalid",
            ));
        }
    }
    check_pack_graph(pack, roots, required, cancellation, deadline)
}

fn check_pack_graph(
    pack: &DecodedPack,
    roots: &[([u8; 20], GitObjectKind)],
    required: &[([u8; 20], GitObjectKind)],
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<PackGraphReport> {
    check_attempt(cancellation, deadline)?;
    if roots.is_empty() || roots.len() > MAX_ROOTS {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "graph root count invalid",
        ));
    }
    let objects = pack.objects();
    check_attempt(cancellation, deadline)?;
    let mut by_oid: HashMap<[u8; 20], usize> = HashMap::new();
    by_oid
        .try_reserve(objects.len())
        .map_err(|_| safe_error(io::ErrorKind::OutOfMemory, "graph index allocation failed"))?;
    check_attempt(cancellation, deadline)?;
    for (index, object) in objects.iter().enumerate() {
        check_attempt(cancellation, deadline)?;
        if let Some(previous) = by_oid.get(object.oid()) {
            let previous = &objects[*previous];
            if previous.kind() != object.kind() || previous.content() != object.content() {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "conflicting objects share an object ID",
                ));
            }
        } else {
            by_oid.insert(*object.oid(), index);
        }
        check_attempt(cancellation, deadline)?;
    }

    check_attempt(cancellation, deadline)?;
    let mut visited = Vec::new();
    visited
        .try_reserve_exact(objects.len())
        .map_err(|_| safe_error(io::ErrorKind::OutOfMemory, "graph visit allocation failed"))?;
    visited.resize(objects.len(), false);
    check_attempt(cancellation, deadline)?;
    let mut queue = Vec::new();
    queue
        .try_reserve_exact(objects.len())
        .map_err(|_| safe_error(io::ErrorKind::OutOfMemory, "graph queue allocation failed"))?;
    check_attempt(cancellation, deadline)?;

    for (oid, expected_kind) in roots {
        check_attempt(cancellation, deadline)?;
        if oid.iter().all(|byte| *byte == 0) {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "graph root ID invalid",
            ));
        }
        let index = resolve(&by_oid, objects, oid, *expected_kind)?;
        if !visited[index] {
            visited[index] = true;
            queue.push(index);
        }
        check_attempt(cancellation, deadline)?;
    }

    let mut cursor = 0usize;
    let mut checked_edges = 0usize;
    while cursor < queue.len() {
        check_attempt(cancellation, deadline)?;
        let object = &objects[queue[cursor]];
        cursor += 1;
        match object.kind() {
            GitObjectKind::Blob => {}
            GitObjectKind::Commit => {
                let links =
                    parse_sha1_commit(object.content(), MAX_PARENTS, cancellation, deadline)?;
                visit_edge(
                    links.tree(),
                    GitObjectKind::Tree,
                    &by_oid,
                    objects,
                    &mut visited,
                    &mut queue,
                    &mut checked_edges,
                    cancellation,
                    deadline,
                )?;
                for parent in links.parents() {
                    visit_edge(
                        parent,
                        GitObjectKind::Commit,
                        &by_oid,
                        objects,
                        &mut visited,
                        &mut queue,
                        &mut checked_edges,
                        cancellation,
                        deadline,
                    )?;
                }
            }
            GitObjectKind::Tree => {
                let entries =
                    parse_sha1_tree(object.content(), MAX_TREE_ENTRIES, cancellation, deadline)?;
                for entry in entries {
                    let kind = match entry.mode() {
                        GitTreeMode::Directory => GitObjectKind::Tree,
                        GitTreeMode::Regular | GitTreeMode::Executable | GitTreeMode::Symlink => {
                            GitObjectKind::Blob
                        }
                        GitTreeMode::Gitlink => {
                            return Err(safe_error(
                                io::ErrorKind::Unsupported,
                                "reachable tree contains unsupported gitlink",
                            ));
                        }
                    };
                    visit_edge(
                        entry.oid(),
                        kind,
                        &by_oid,
                        objects,
                        &mut visited,
                        &mut queue,
                        &mut checked_edges,
                        cancellation,
                        deadline,
                    )?;
                }
            }
            GitObjectKind::Tag => {
                let link = parse_sha1_tag(object.content(), cancellation, deadline)?;
                visit_edge(
                    link.target(),
                    link.target_kind(),
                    &by_oid,
                    objects,
                    &mut visited,
                    &mut queue,
                    &mut checked_edges,
                    cancellation,
                    deadline,
                )?;
            }
        }
        check_attempt(cancellation, deadline)?;
    }

    for (oid, expected_kind) in required {
        check_attempt(cancellation, deadline)?;
        let index = resolve(&by_oid, objects, oid, *expected_kind)?;
        if !visited[index] {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "required object is not reachable from graph roots",
            ));
        }
        check_attempt(cancellation, deadline)?;
    }

    check_attempt(cancellation, deadline)?;
    Ok(PackGraphReport {
        root_count: roots.len(),
        reachable_objects: queue.len(),
        checked_edges,
    })
}

#[allow(clippy::too_many_arguments)]
fn visit_edge(
    oid: &[u8; 20],
    expected_kind: GitObjectKind,
    by_oid: &HashMap<[u8; 20], usize>,
    objects: &[crate::DecodedPackObject],
    visited: &mut [bool],
    queue: &mut Vec<usize>,
    checked_edges: &mut usize,
    cancellation: &CancellationHandle,
    deadline: Instant,
) -> io::Result<()> {
    check_attempt(cancellation, deadline)?;
    *checked_edges = checked_edges
        .checked_add(1)
        .filter(|count| *count <= MAX_EDGES)
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "graph edge limit exceeded"))?;
    check_attempt(cancellation, deadline)?;
    let index = resolve(by_oid, objects, oid, expected_kind)?;
    check_attempt(cancellation, deadline)?;
    if !visited[index] {
        visited[index] = true;
        queue.push(index);
    }
    check_attempt(cancellation, deadline)
}

fn resolve(
    by_oid: &HashMap<[u8; 20], usize>,
    objects: &[crate::DecodedPackObject],
    oid: &[u8; 20],
    expected_kind: GitObjectKind,
) -> io::Result<usize> {
    let index = by_oid.get(oid).copied().ok_or_else(|| {
        safe_error(
            io::ErrorKind::NotFound,
            "reachable object is missing from pack",
        )
    })?;
    if objects[index].kind() != expected_kind {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "reachable object has unexpected type",
        ));
    }
    Ok(index)
}

fn check_attempt(cancellation: &CancellationHandle, deadline: Instant) -> io::Result<()> {
    cancellation.check()?;
    check_deadline(deadline)
}
