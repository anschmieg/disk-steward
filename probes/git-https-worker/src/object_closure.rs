use std::collections::{HashMap, VecDeque};

use gix::{hash::ObjectId, objs::Kind};

pub(crate) const MAX_CLOSURE_OBJECTS: usize = 100_000;
pub(crate) const MAX_CLOSURE_DECODED_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_CLOSURE_SINGLE_OBJECT_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_CLOSURE_WORK_UNITS: usize = 250_000;
const MAX_PENDING_OBJECTS: usize = 100_000;

#[derive(Clone, Copy)]
struct ClosureLimits {
    max_objects: usize,
    max_decoded_bytes: u64,
    max_single_object_bytes: u64,
    max_work_units: usize,
    max_pending_objects: usize,
}

const DEFAULT_LIMITS: ClosureLimits = ClosureLimits {
    max_objects: MAX_CLOSURE_OBJECTS,
    max_decoded_bytes: MAX_CLOSURE_DECODED_BYTES,
    max_single_object_bytes: MAX_CLOSURE_SINGLE_OBJECT_BYTES,
    max_work_units: MAX_CLOSURE_WORK_UNITS,
    max_pending_objects: MAX_PENDING_OBJECTS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClosureStats {
    pub(crate) object_count: u32,
    pub(crate) decoded_bytes: u64,
    pub(crate) excluded_gitlink_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClosureError {
    Missing,
    Malformed,
    TypeMismatch,
    HashMismatch,
    Budget,
}

trait ObjectSource {
    fn header(&mut self, id: &ObjectId) -> Result<Option<(Kind, u64)>, ClosureError>;
    fn object(&mut self, id: &ObjectId) -> Result<Option<(Kind, Vec<u8>)>, ClosureError>;
}

struct RepositorySource<'repo>(&'repo gix::Repository);

impl ObjectSource for RepositorySource<'_> {
    fn header(&mut self, id: &ObjectId) -> Result<Option<(Kind, u64)>, ClosureError> {
        self.0
            .try_find_header(id.to_owned())
            .map(|header| header.map(|header| (header.kind(), header.size())))
            .map_err(|_| ClosureError::Missing)
    }

    fn object(&mut self, id: &ObjectId) -> Result<Option<(Kind, Vec<u8>)>, ClosureError> {
        self.0
            .try_find_object(id.to_owned())
            .map(|object| {
                object.map(|mut object| {
                    let kind = object.kind;
                    let data = std::mem::take(&mut object.data);
                    (kind, data)
                })
            })
            .map_err(|_| ClosureError::Malformed)
    }
}

pub(crate) fn validate_advertised_refs(
    repo: &gix::Repository,
    refs: &[gix::protocol::handshake::Ref],
) -> Result<ClosureStats, ClosureError> {
    let roots = advertised_ref_roots(refs)?;
    validate_roots(&mut RepositorySource(repo), repo.object_hash(), roots)
}

fn advertised_ref_roots(
    refs: &[gix::protocol::handshake::Ref],
) -> Result<Vec<(ObjectId, Option<Kind>)>, ClosureError> {
    if refs.len() > MAX_PENDING_OBJECTS {
        return Err(ClosureError::Budget);
    }
    let mut roots = Vec::with_capacity(refs.len());
    for reference in refs {
        let root = match reference {
            gix::protocol::handshake::Ref::Direct { object, .. } => Some((*object, None)),
            gix::protocol::handshake::Ref::Peeled { tag, .. } => Some((*tag, Some(Kind::Tag))),
            gix::protocol::handshake::Ref::Symbolic { tag, object, .. } => Some((
                *tag.as_ref().unwrap_or(object),
                tag.as_ref().map(|_| Kind::Tag),
            )),
            gix::protocol::handshake::Ref::Unborn { .. } => None,
        };
        if let Some(root) = root {
            roots.push(root);
        }
    }
    Ok(roots)
}

fn validate_roots(
    source: &mut impl ObjectSource,
    hash_kind: gix::hash::Kind,
    roots: impl IntoIterator<Item = (ObjectId, Option<Kind>)>,
) -> Result<ClosureStats, ClosureError> {
    validate_roots_with_limits(source, hash_kind, roots, DEFAULT_LIMITS)
}

fn validate_roots_with_limits(
    source: &mut impl ObjectSource,
    hash_kind: gix::hash::Kind,
    roots: impl IntoIterator<Item = (ObjectId, Option<Kind>)>,
    limits: ClosureLimits,
) -> Result<ClosureStats, ClosureError> {
    let mut pending = VecDeque::new();
    for root in roots {
        if pending.len() >= limits.max_pending_objects {
            return Err(ClosureError::Budget);
        }
        pending.push_back(root);
    }

    let mut seen = HashMap::<ObjectId, Kind>::new();
    let mut decoded_bytes = 0_u64;
    let mut work_units = 0_usize;
    let mut excluded_gitlink_count = 0_u32;

    while let Some((id, expected_kind)) = pending.pop_front() {
        work_units = work_units.checked_add(1).ok_or(ClosureError::Budget)?;
        if work_units > limits.max_work_units {
            return Err(ClosureError::Budget);
        }
        if let Some(known_kind) = seen.get(&id) {
            if expected_kind.is_some_and(|expected| expected != *known_kind) {
                return Err(ClosureError::TypeMismatch);
            }
            continue;
        }

        let (header_kind, object_size) = source.header(&id)?.ok_or(ClosureError::Missing)?;
        if object_size > limits.max_single_object_bytes {
            return Err(ClosureError::Budget);
        }
        decoded_bytes = decoded_bytes
            .checked_add(object_size)
            .filter(|total| *total <= limits.max_decoded_bytes)
            .ok_or(ClosureError::Budget)?;

        let (kind, data) = source.object(&id)?.ok_or(ClosureError::Missing)?;
        if kind != header_kind || data.len() as u64 != object_size {
            return Err(ClosureError::Malformed);
        }
        if expected_kind.is_some_and(|expected| expected != kind) {
            return Err(ClosureError::TypeMismatch);
        }
        let computed =
            gix::objs::compute_hash(hash_kind, kind, &data).map_err(|_| ClosureError::Malformed)?;
        if computed != id {
            return Err(ClosureError::HashMismatch);
        }
        seen.insert(id, kind);
        if seen.len() > limits.max_objects {
            return Err(ClosureError::Budget);
        }

        match kind {
            Kind::Blob => {}
            Kind::Commit => {
                let commit =
                    gix::objs::CommitRef::from_bytes(&data).map_err(|_| ClosureError::Malformed)?;
                enqueue(
                    &mut pending,
                    &mut work_units,
                    limits,
                    commit.tree(),
                    Kind::Tree,
                )?;
                for parent in commit.parents() {
                    enqueue(&mut pending, &mut work_units, limits, parent, Kind::Commit)?;
                }
            }
            Kind::Tree => {
                for entry in gix::objs::TreeRefIter::from_bytes(&data) {
                    let entry = entry.map_err(|_| ClosureError::Malformed)?;
                    let child = entry.oid.to_owned();
                    match entry.mode.kind() {
                        gix::objs::tree::EntryKind::Tree => {
                            enqueue(&mut pending, &mut work_units, limits, child, Kind::Tree)?
                        }
                        gix::objs::tree::EntryKind::Blob
                        | gix::objs::tree::EntryKind::BlobExecutable
                        | gix::objs::tree::EntryKind::Link => {
                            enqueue(&mut pending, &mut work_units, limits, child, Kind::Blob)?
                        }
                        // Gitlinks name commits in another repository. They are deliberately
                        // excluded; this probe does not fetch or claim submodule closure.
                        gix::objs::tree::EntryKind::Commit => {
                            excluded_gitlink_count = excluded_gitlink_count
                                .checked_add(1)
                                .ok_or(ClosureError::Budget)?;
                            work_units = work_units.checked_add(1).ok_or(ClosureError::Budget)?;
                            if work_units > limits.max_work_units {
                                return Err(ClosureError::Budget);
                            }
                        }
                    }
                }
            }
            Kind::Tag => {
                let tag =
                    gix::objs::TagRef::from_bytes(&data).map_err(|_| ClosureError::Malformed)?;
                enqueue(
                    &mut pending,
                    &mut work_units,
                    limits,
                    tag.target(),
                    tag.target_kind,
                )?;
            }
        }
    }

    Ok(ClosureStats {
        object_count: u32::try_from(seen.len()).map_err(|_| ClosureError::Budget)?,
        decoded_bytes,
        excluded_gitlink_count,
    })
}

fn enqueue(
    pending: &mut VecDeque<(ObjectId, Option<Kind>)>,
    work_units: &mut usize,
    limits: ClosureLimits,
    id: ObjectId,
    kind: Kind,
) -> Result<(), ClosureError> {
    *work_units = (*work_units).checked_add(1).ok_or(ClosureError::Budget)?;
    if *work_units > limits.max_work_units || pending.len() >= limits.max_pending_objects {
        return Err(ClosureError::Budget);
    }
    pending.push_back((id, Some(kind)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MemoryObject {
        kind: Kind,
        data: Vec<u8>,
    }

    #[derive(Default)]
    struct MemorySource(HashMap<ObjectId, MemoryObject>);

    impl MemorySource {
        fn insert(&mut self, kind: Kind, data: Vec<u8>) -> ObjectId {
            let id = gix::objs::compute_hash(gix::hash::Kind::Sha1, kind, &data).unwrap();
            self.0.insert(id, MemoryObject { kind, data });
            id
        }
    }

    impl ObjectSource for MemorySource {
        fn header(&mut self, id: &ObjectId) -> Result<Option<(Kind, u64)>, ClosureError> {
            Ok(self
                .0
                .get(id)
                .map(|object| (object.kind, object.data.len() as u64)))
        }

        fn object(&mut self, id: &ObjectId) -> Result<Option<(Kind, Vec<u8>)>, ClosureError> {
            Ok(self
                .0
                .get(id)
                .map(|object| (object.kind, object.data.clone())))
        }
    }

    fn ref_bytes(field: &str, id: ObjectId) -> Vec<u8> {
        format!("{field} {id}\n").into_bytes()
    }

    fn tree_entry(mode: &str, name: &str, id: ObjectId) -> Vec<u8> {
        let mut entry = format!("{mode} {name}").into_bytes();
        entry.push(0);
        entry.extend_from_slice(id.as_bytes());
        entry
    }

    #[test]
    fn advertised_refs_include_tags_and_skip_unborn() {
        let direct = ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap();
        let tag = ObjectId::from_hex(b"2222222222222222222222222222222222222222").unwrap();
        let peeled = ObjectId::from_hex(b"3333333333333333333333333333333333333333").unwrap();
        let refs = [
            gix::protocol::handshake::Ref::Direct {
                full_ref_name: "refs/heads/main".into(),
                object: direct,
            },
            gix::protocol::handshake::Ref::Peeled {
                full_ref_name: "refs/tags/v1".into(),
                tag,
                object: peeled,
            },
            gix::protocol::handshake::Ref::Symbolic {
                full_ref_name: "HEAD".into(),
                target: "refs/heads/main".into(),
                tag: None,
                object: direct,
            },
            gix::protocol::handshake::Ref::Unborn {
                full_ref_name: "HEAD".into(),
                target: "refs/heads/main".into(),
            },
        ];
        assert_eq!(
            advertised_ref_roots(&refs).unwrap(),
            vec![(direct, None), (tag, Some(Kind::Tag)), (direct, None),]
        );
    }

    #[test]
    fn repository_adapter_reads_and_hash_checks_real_gix_objects() {
        let mut store = crate::OwnedStore::create().unwrap();
        let repository_path = store.path().join("synthetic.git");
        let stats = {
            let repo = gix::init_bare(&repository_path).unwrap();
            let blob = repo
                .write_object(gix::objs::BlobRef { data: b"synthetic" })
                .unwrap()
                .detach();
            let root = gix::protocol::handshake::Ref::Direct {
                full_ref_name: "refs/heads/main".into(),
                object: blob,
            };
            validate_advertised_refs(&repo, &[root]).unwrap()
        };
        assert_eq!(stats.object_count, 1);
        assert_eq!(stats.decoded_bytes, b"synthetic".len() as u64);
        assert_eq!(stats.excluded_gitlink_count, 0);
        assert!(store.cleanup());
        store.retain();
    }

    #[test]
    fn validates_typed_small_graph_and_excludes_gitlink() {
        let mut source = MemorySource::default();
        let blob = source.insert(Kind::Blob, b"file contents\n".to_vec());
        let link_blob = source.insert(Kind::Blob, b"target/path".to_vec());
        let mut tree_data = tree_entry("100644", "file", blob);
        tree_data.extend(tree_entry("120000", "link", link_blob));
        let nested = source.insert(Kind::Tree, tree_entry("100644", "inner", blob));
        tree_data.extend(tree_entry("40000", "nested", nested));
        tree_data.extend(tree_entry(
            "160000",
            "submodule",
            gix::hash::ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap(),
        ));
        let tree = source.insert(Kind::Tree, tree_data);
        let parent_tree = source.insert(Kind::Tree, Vec::new());
        let mut parent_data = ref_bytes("tree", parent_tree);
        parent_data.extend_from_slice(
            b"author Test <test@example.invalid> 1 +0000\ncommitter Test <test@example.invalid> 1 +0000\n\nroot\n",
        );
        let parent = source.insert(Kind::Commit, parent_data);
        let mut commit_data = ref_bytes("tree", tree);
        commit_data.extend_from_slice(format!("parent {parent}\n").as_bytes());
        commit_data.extend_from_slice(
            b"author Test <test@example.invalid> 2 +0000\ncommitter Test <test@example.invalid> 2 +0000\n\nnext\n",
        );
        let commit = source.insert(Kind::Commit, commit_data);
        let tag_data = format!(
            "object {commit}\ntype commit\ntag v1\ntagger Test <test@example.invalid> 1 +0000\n\nrelease\n"
        )
        .into_bytes();
        let tag = source.insert(Kind::Tag, tag_data);

        let stats =
            validate_roots(&mut source, gix::hash::Kind::Sha1, [(tag, Some(Kind::Tag))]).unwrap();
        assert_eq!(stats.object_count, 8);
        assert_eq!(stats.excluded_gitlink_count, 1);
        assert!(stats.decoded_bytes < MAX_CLOSURE_DECODED_BYTES);
    }

    #[test]
    fn rejects_missing_objects_and_type_mismatches() {
        let mut source = MemorySource::default();
        let missing =
            gix::hash::ObjectId::from_hex(b"2222222222222222222222222222222222222222").unwrap();
        assert_eq!(
            validate_roots(&mut source, gix::hash::Kind::Sha1, [(missing, None)]),
            Err(ClosureError::Missing)
        );

        let missing_tree =
            gix::hash::ObjectId::from_hex(b"3333333333333333333333333333333333333333").unwrap();
        let mut dangling_commit = ref_bytes("tree", missing_tree);
        dangling_commit.extend_from_slice(
            b"author Test <test@example.invalid> 1 +0000\ncommitter Test <test@example.invalid> 1 +0000\n\ndangling\n",
        );
        let dangling_commit = source.insert(Kind::Commit, dangling_commit);
        assert_eq!(
            validate_roots(
                &mut source,
                gix::hash::Kind::Sha1,
                [(dangling_commit, None)]
            ),
            Err(ClosureError::Missing)
        );

        let blob = source.insert(Kind::Blob, b"x".to_vec());
        assert_eq!(
            validate_roots(
                &mut source,
                gix::hash::Kind::Sha1,
                [(blob, Some(Kind::Tree))]
            ),
            Err(ClosureError::TypeMismatch)
        );

        let wrong_tag = source.insert(
            Kind::Tag,
            format!(
                "object {blob}\ntype commit\ntag bad\ntagger Test <test@example.invalid> 1 +0000\n\nbad type\n"
            )
            .into_bytes(),
        );
        assert_eq!(
            validate_roots(
                &mut source,
                gix::hash::Kind::Sha1,
                [(wrong_tag, Some(Kind::Tag))]
            ),
            Err(ClosureError::TypeMismatch)
        );
    }

    #[test]
    fn rejects_wrong_typed_duplicate_after_object_was_already_seen() {
        let mut source = MemorySource::default();
        let blob = source.insert(Kind::Blob, b"not a tree".to_vec());
        let malformed_tree_link = source.insert(Kind::Tree, tree_entry("40000", "child", blob));
        let mut commit_data = ref_bytes("tree", malformed_tree_link);
        commit_data.extend_from_slice(
            b"author Test <test@example.invalid> 1 +0000\ncommitter Test <test@example.invalid> 1 +0000\n\nroot\n",
        );
        let commit = source.insert(Kind::Commit, commit_data);

        // Visit the same ID first as an untyped direct ref, then through a
        // tree entry that requires the object to be a tree.
        assert_eq!(
            validate_roots(
                &mut source,
                gix::hash::Kind::Sha1,
                [(blob, None), (commit, None)]
            ),
            Err(ClosureError::TypeMismatch)
        );
    }

    #[test]
    fn rejects_malformed_objects_and_content_hash_mismatch() {
        let mut source = MemorySource::default();
        let malformed = source.insert(Kind::Commit, b"not a commit".to_vec());
        assert_eq!(
            validate_roots(&mut source, gix::hash::Kind::Sha1, [(malformed, None)]),
            Err(ClosureError::Malformed)
        );

        let id = gix::objs::compute_hash(gix::hash::Kind::Sha1, Kind::Blob, b"original").unwrap();
        source.0.insert(
            id,
            MemoryObject {
                kind: Kind::Blob,
                data: b"tampered".to_vec(),
            },
        );
        assert_eq!(
            validate_roots(&mut source, gix::hash::Kind::Sha1, [(id, None)]),
            Err(ClosureError::HashMismatch)
        );
    }

    #[test]
    fn enforces_decoded_byte_work_and_object_budgets() {
        let mut source = MemorySource::default();
        let blob = source.insert(Kind::Blob, b"four".to_vec());
        let strict_limits = ClosureLimits {
            max_objects: 1,
            max_decoded_bytes: 3,
            max_single_object_bytes: 4,
            max_work_units: 10,
            max_pending_objects: 10,
        };
        assert_eq!(
            validate_roots_with_limits(
                &mut source,
                gix::hash::Kind::Sha1,
                [(blob, None)],
                strict_limits
            ),
            Err(ClosureError::Budget)
        );

        let single_object_limits = ClosureLimits {
            max_objects: 10,
            max_decoded_bytes: 10,
            max_single_object_bytes: 3,
            max_work_units: 10,
            max_pending_objects: 10,
        };
        assert_eq!(
            validate_roots_with_limits(
                &mut source,
                gix::hash::Kind::Sha1,
                [(blob, None)],
                single_object_limits
            ),
            Err(ClosureError::Budget)
        );

        let one_object_limits = ClosureLimits {
            max_objects: 0,
            max_decoded_bytes: 10,
            max_single_object_bytes: 10,
            max_work_units: 10,
            max_pending_objects: 1,
        };
        assert_eq!(
            validate_roots_with_limits(
                &mut source,
                gix::hash::Kind::Sha1,
                [(blob, None)],
                one_object_limits
            ),
            Err(ClosureError::Budget)
        );

        let no_work_limits = ClosureLimits {
            max_objects: 10,
            max_decoded_bytes: 10,
            max_single_object_bytes: 10,
            max_work_units: 0,
            max_pending_objects: 10,
        };
        assert_eq!(
            validate_roots_with_limits(
                &mut source,
                gix::hash::Kind::Sha1,
                [(blob, None)],
                no_work_limits
            ),
            Err(ClosureError::Budget)
        );

        let no_pending_limits = ClosureLimits {
            max_objects: 10,
            max_decoded_bytes: 10,
            max_single_object_bytes: 10,
            max_work_units: 10,
            max_pending_objects: 0,
        };
        assert_eq!(
            validate_roots_with_limits(
                &mut source,
                gix::hash::Kind::Sha1,
                [(blob, None)],
                no_pending_limits
            ),
            Err(ClosureError::Budget)
        );
    }
}
