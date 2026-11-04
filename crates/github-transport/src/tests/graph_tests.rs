use crate::{
    check_pack_graph_closure as check, decode_sha1_pack, git_sha1_object_id, CancellationHandle,
    DecodedPack, GitObjectKind as K,
};
use flate2::{write::ZlibEncoder, Compression};
use sha1_checked::{digest::Digest, Sha1};
use std::{
    io::{ErrorKind, Write},
    time::{Duration, Instant},
};
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
pub(super) fn oid(kind: K, bytes: &[u8]) -> [u8; 20] {
    git_sha1_object_id(kind, bytes, &CancellationHandle::new(), deadline()).unwrap()
}
pub(super) fn hex(id: &[u8; 20]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn pack_bytes(objects: &[(K, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"PACK\0\0\0\x02".to_vec();
    out.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    for (kind, content) in objects {
        let type_bits = match kind {
            K::Commit => 1,
            K::Tree => 2,
            K::Blob => 3,
            K::Tag => 4,
        };
        let mut size = content.len();
        let mut byte = (type_bits << 4) | (size as u8 & 15);
        size >>= 4;
        while size > 0 {
            out.push(byte | 128);
            byte = size as u8 & 127;
            size >>= 7;
        }
        out.push(byte);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(content).unwrap();
        out.extend(encoder.finish().unwrap());
    }
    let digest = Sha1::digest(&out);
    out.extend_from_slice(&digest);
    out
}
fn pack(objects: &[(K, Vec<u8>)]) -> DecodedPack {
    decode_sha1_pack(
        &pack_bytes(objects),
        16 * 1024 * 1024,
        &CancellationHandle::new(),
        deadline(),
    )
    .unwrap()
}
pub(super) fn tree(mode: &str, id: [u8; 20]) -> Vec<u8> {
    let mut b = format!("{mode} file\0").into_bytes();
    b.extend(id);
    b
}
pub(super) fn commit(tree: [u8; 20], parents: &[[u8; 20]]) -> Vec<u8> {
    let mut b = format!("tree {}\n", hex(&tree));
    for parent in parents {
        b.push_str(&format!("parent {}\n", hex(parent)));
    }
    b.push_str("author A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nmessage");
    b.into_bytes()
}
fn run(p: &DecodedPack, roots: &[([u8; 20], K)]) -> std::io::Result<crate::PackGraphReport> {
    check(p, roots, &CancellationHandle::new(), deadline())
}
#[test]
fn shared_closure_and_identical_duplicate_entries_are_counted_once() {
    let blob = b"x".to_vec();
    let t = tree("100644", oid(K::Blob, &blob));
    let c = commit(oid(K::Tree, &t), &[]);
    let tag = format!(
        "object {}\ntype commit\ntag fixture\n\n",
        hex(&oid(K::Commit, &c))
    )
    .into_bytes();
    let roots = [(oid(K::Commit, &c), K::Commit), (oid(K::Tag, &tag), K::Tag)];
    let p = pack(&[
        (K::Blob, blob.clone()),
        (K::Tree, t),
        (K::Commit, c),
        (K::Tag, tag),
        (K::Blob, blob),
    ]);
    let r = run(&p, &roots).unwrap();
    assert_eq!(r.root_count(), 2);
    assert_eq!(r.reachable_objects(), 4);
    assert_eq!(r.checked_edges(), 3);
}
#[test]
fn roots_and_each_link_must_resolve_with_expected_type() {
    let blob = b"x".to_vec();
    let id = oid(K::Blob, &blob);
    let p = pack(&[(K::Blob, blob.clone())]);
    for roots in [
        vec![],
        vec![([0; 20], K::Blob)],
        vec![([1; 20], K::Blob)],
        vec![(id, K::Commit)],
        vec![(id, K::Blob); 65],
    ] {
        assert!(run(&p, &roots).is_err());
    }
    for (kind, bytes) in [
        (K::Tree, tree("100644", [1; 20])),
        (K::Commit, commit([1; 20], &[])),
        (K::Commit, commit(id, &[])),
        (
            K::Tag,
            format!("object {}\ntype tree\ntag fixture\n\n", hex(&id)).into_bytes(),
        ),
    ] {
        let root = oid(kind, &bytes);
        let p = pack(&[(K::Blob, blob.clone()), (kind, bytes)]);
        assert!(run(&p, &[(root, kind)]).is_err());
    }
}
#[test]
fn gitlinks_are_unsupported_and_only_reachable_objects_are_parsed() {
    let bytes = tree("160000", [1; 20]);
    let root = oid(K::Tree, &bytes);
    let p = pack(&[(K::Tree, bytes)]);
    assert_eq!(
        run(&p, &[(root, K::Tree)]).unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    for kind in [K::Commit, K::Tree, K::Tag] {
        let bad = b"malformed".to_vec();
        let id = oid(kind, &bad);
        let p = pack(&[(kind, bad), (K::Blob, b"x".to_vec())]);
        assert!(run(&p, &[(id, kind)]).is_err());
        assert_eq!(
            run(&p, &[(oid(K::Blob, b"x"), K::Blob)])
                .unwrap()
                .reachable_objects(),
            1
        );
    }
}
#[test]
fn repeated_edges_consume_the_cumulative_limit() {
    let mut objects = vec![(K::Tree, Vec::new())];
    let tree_id = oid(K::Tree, b"");
    let initial = commit(tree_id, &[]);
    let mut id = oid(K::Commit, &initial);
    objects.push((K::Commit, initial));
    for _ in 0..15 {
        let c = commit(tree_id, &vec![id; 1024]);
        id = oid(K::Commit, &c);
        objects.push((K::Commit, c));
    }
    assert_eq!(
        run(&pack(&objects), &[(id, K::Commit)])
            .unwrap()
            .checked_edges(),
        15 * 1025 + 1
    );
    let c = commit(tree_id, &vec![id; 1024]);
    id = oid(K::Commit, &c);
    objects.push((K::Commit, c));
    assert!(run(&pack(&objects), &[(id, K::Commit)]).is_err());
}
#[test]
fn controls_refuse_before_returning_a_report() {
    let p = pack(&[(K::Blob, b"x".to_vec())]);
    let roots = [(oid(K::Blob, b"x"), K::Blob)];
    let cancel = CancellationHandle::new();
    cancel.cancel();
    assert_eq!(
        check(&p, &roots, &cancel, deadline()).unwrap_err().kind(),
        ErrorKind::ConnectionAborted
    );
    assert_eq!(
        check(&p, &roots, &CancellationHandle::new(), Instant::now())
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
}

#[test]
fn directory_blob_modes_parent_and_nested_tag_links_are_checked() {
    let blob = b"x".to_vec();
    let blob_id = oid(K::Blob, &blob);
    for mode in ["100644", "100755", "120000"] {
        let leaf = tree(mode, blob_id);
        let leaf_id = oid(K::Tree, &leaf);
        let directory = tree("40000", leaf_id);
        let directory_id = oid(K::Tree, &directory);
        let parent = commit(directory_id, &[]);
        let parent_id = oid(K::Commit, &parent);
        let child = commit(directory_id, &[parent_id]);
        let child_id = oid(K::Commit, &child);
        let tag = format!("object {}\ntype commit\ntag inner\n\n", hex(&child_id)).into_bytes();
        let tag_id = oid(K::Tag, &tag);
        let outer = format!("object {}\ntype tag\ntag outer\n\n", hex(&tag_id)).into_bytes();
        let outer_id = oid(K::Tag, &outer);
        let objects = vec![
            (K::Blob, blob.clone()),
            (K::Tree, leaf),
            (K::Tree, directory),
            (K::Commit, parent),
            (K::Commit, child),
            (K::Tag, tag),
            (K::Tag, outer),
        ];
        let r = run(&pack(&objects), &[(outer_id, K::Tag)]).unwrap();
        assert_eq!(r.reachable_objects(), 7);
        assert_eq!(r.checked_edges(), 7);
        let without_parent: Vec<_> = objects
            .into_iter()
            .filter(|(k, b)| !(*k == K::Commit && oid(*k, b) == parent_id))
            .collect();
        assert_eq!(
            run(&pack(&without_parent), &[(outer_id, K::Tag)])
                .unwrap_err()
                .kind(),
            ErrorKind::NotFound
        );
    }
    let t = tree("100644", blob_id);
    let t_id = oid(K::Tree, &t);
    let wrong = format!("object {}\ntype tree\ntag wrong\n\n", hex(&blob_id)).into_bytes();
    let wrong_id = oid(K::Tag, &wrong);
    let p = pack(&[(K::Blob, blob), (K::Tree, t), (K::Tag, wrong)]);
    // The blob has already been visited via a valid root: its wrong tag type must still fail.
    assert_eq!(
        run(
            &p,
            &[(blob_id, K::Blob), (t_id, K::Tree), (wrong_id, K::Tag)]
        )
        .unwrap_err()
        .kind(),
        ErrorKind::InvalidData
    );
}
