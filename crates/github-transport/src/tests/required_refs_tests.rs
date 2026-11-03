use crate::{
    CancellationHandle, GitObjectKind as K, RequiredRefInput as Input,
    RequiredRefManifest as Manifest,
};
use std::{
    io::ErrorKind,
    time::{Duration, Instant},
};
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
fn capture(input: &[Input<'_>]) -> std::io::Result<Manifest> {
    Manifest::capture(input, &CancellationHandle::new(), deadline())
}
fn entry(name: &[u8], oid: u8, kind: K) -> Input<'_> {
    Input {
        name,
        oid: [oid; 20],
        kind,
    }
}
#[test]
fn canonical_digest_is_order_independent_and_names_are_lossless() {
    let input = [
        entry(b"refs/tags/\xff", 2, K::Blob),
        entry(b"refs/heads/local", 1, K::Commit),
        entry(b"HEAD", 1, K::Commit),
    ];
    let manifest = capture(&input).unwrap();
    assert_eq!(
        manifest
            .entries()
            .iter()
            .map(|e| e.name())
            .collect::<Vec<_>>(),
        [b"HEAD".as_slice(), b"refs/heads/local", b"refs/tags/\xff"]
    );
    let digest: String = manifest
        .digest()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        digest,
        "ed23fcd505cc1f5b18448785bfb9f271e524c445e8855d6bd194520901df6877"
    );
    let reversed = input.into_iter().rev().collect::<Vec<_>>();
    let current = capture(&reversed).unwrap();
    assert_eq!(manifest.digest(), current.digest());
    manifest
        .check_unchanged(&current, &CancellationHandle::new(), deadline())
        .unwrap();
}
#[test]
fn changes_to_names_targets_kinds_or_membership_invalidate_snapshot() {
    let original = capture(&[entry(b"refs/tags/a", 1, K::Blob)]).unwrap();
    for entries in [
        vec![entry(b"refs/tags/b", 1, K::Blob)],
        vec![entry(b"refs/tags/a", 2, K::Blob)],
        vec![entry(b"refs/tags/a", 1, K::Tree)],
        vec![
            entry(b"refs/tags/a", 1, K::Blob),
            entry(b"refs/tags/b", 1, K::Blob),
        ],
    ] {
        let changed = capture(&entries).unwrap();
        assert_ne!(original.digest(), changed.digest());
        assert_eq!(
            original
                .check_unchanged(&changed, &CancellationHandle::new(), deadline())
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );
    }
}
#[test]
fn names_ids_types_and_duplicate_names_are_validated() {
    for name in [
        b"".as_slice(),
        b"local",
        b"ORIG_HEAD",
        b"refs/heads/HEAD",
        b"refs/heads/.hidden",
        b"refs/tags/a.lock",
        b"refs/tags/a..b",
        b"refs/tags/a\0b",
        b"refs/tags/a b",
        b"refs/tags/a@{b",
    ] {
        assert!(capture(&[entry(name, 1, K::Commit)]).is_err());
    }
    for input in [
        entry(b"HEAD", 1, K::Blob),
        entry(b"refs/heads/a", 1, K::Tag),
        entry(b"refs/tags/a", 0, K::Blob),
    ] {
        assert!(capture(&[input]).is_err());
    }
    assert!(capture(&[
        entry(b"refs/tags/a", 1, K::Blob),
        entry(b"refs/tags/a", 2, K::Blob)
    ])
    .is_err());
    assert_eq!(
        capture(&[
            entry(b"refs/tags/a", 1, K::Blob),
            entry(b"refs/tags/b", 1, K::Blob)
        ])
        .unwrap()
        .entries()
        .len(),
        2
    );
}
#[test]
fn request_name_and_aggregate_boundaries_are_enforced() {
    assert!(capture(&[]).is_err());
    let names = (0..1025)
        .map(|i| format!("refs/tags/n{i}").into_bytes())
        .collect::<Vec<_>>();
    let input = names
        .iter()
        .map(|n| entry(n, 1, K::Blob))
        .collect::<Vec<_>>();
    assert_eq!(capture(&input[..1024]).unwrap().entries().len(), 1024);
    assert!(capture(&input).is_err());
    let names = (0..65)
        .map(|i| {
            let mut n = format!("refs/tags/{i:02}").into_bytes();
            n.resize(1024, b'x');
            n
        })
        .collect::<Vec<_>>();
    let input = names
        .iter()
        .map(|n| entry(n, 1, K::Blob))
        .collect::<Vec<_>>();
    assert_eq!(capture(&input[..64]).unwrap().entries().len(), 64);
    assert!(capture(&input).is_err());
    let mut too_long = b"refs/tags/".to_vec();
    too_long.resize(1025, b'x');
    assert!(capture(&[entry(&too_long, 1, K::Blob)]).is_err());
}
#[test]
fn cancellation_and_deadline_refuse_capture_and_comparison() {
    let input = [entry(b"HEAD", 1, K::Commit)];
    let manifest = capture(&input).unwrap();
    let cancel = CancellationHandle::new();
    cancel.cancel();
    assert_eq!(
        Manifest::capture(&input, &cancel, deadline())
            .unwrap_err()
            .kind(),
        ErrorKind::ConnectionAborted
    );
    assert_eq!(
        Manifest::capture(&input, &CancellationHandle::new(), Instant::now())
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
    assert_eq!(
        manifest
            .check_unchanged(&manifest, &cancel, deadline())
            .unwrap_err()
            .kind(),
        ErrorKind::ConnectionAborted
    );
    assert_eq!(
        manifest
            .check_unchanged(&manifest, &CancellationHandle::new(), Instant::now())
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
}
