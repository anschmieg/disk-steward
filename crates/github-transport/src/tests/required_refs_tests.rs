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
        symref_target: None,
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
        "63df41ba50f8cb93c5a0e8982e6082241fc899788fa9dc700b25f7ee1f1cd644"
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

#[test]
fn symbolic_target_changes_and_detachment_change_manifest_identity() {
    let direct = entry(b"HEAD", 1, K::Commit);
    let symbolic = Input {
        symref_target: Some(b"refs/heads/local"),
        ..direct
    };
    let other = Input {
        symref_target: Some(b"refs/heads/other"),
        ..direct
    };
    let original = capture(&[symbolic]).unwrap();
    assert_eq!(
        original.entries()[0].symref_target(),
        Some(b"refs/heads/local".as_slice())
    );
    for changed in [direct, other] {
        let current = capture(&[changed]).unwrap();
        assert_ne!(original.digest(), current.digest());
        assert_eq!(
            original
                .check_unchanged(&current, &CancellationHandle::new(), deadline())
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );
    }
    let full = capture(&[
        symbolic,
        entry(b"refs/heads/local", 1, K::Commit),
        entry(b"refs/tags/\xff", 2, K::Blob),
    ])
    .unwrap();
    let digest: String = full.digest().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        digest,
        "38da3c42ac9e76bab39abcb7949cc09f0c4d5827db44cf5c1cf88cc44140ed94"
    );
    for target in [
        b"".as_slice(),
        b"HEAD",
        b"refs/heads/HEAD",
        b"refs/heads/.bad",
        b"refs/heads/a b",
        b"refs/heads/a.lock",
    ] {
        assert!(capture(&[Input {
            symref_target: Some(target),
            ..direct
        }])
        .is_err());
    }
    assert!(capture(&[Input {
        symref_target: Some(b"refs/tags/self"),
        ..entry(b"refs/tags/self", 1, K::Blob)
    }])
    .is_err());
    assert!(capture(&[Input {
        symref_target: Some(b"refs/heads/main"),
        ..entry(b"refs/tags/a", 1, K::Blob)
    }])
    .is_err());
    let raw = capture(&[Input {
        symref_target: Some(b"refs/tags/\xff"),
        ..entry(b"refs/tags/raw", 1, K::Blob)
    }])
    .unwrap();
    assert_eq!(
        raw.entries()[0].symref_target(),
        Some(b"refs/tags/\xff".as_slice())
    );
}

#[test]
fn symbolic_targets_share_the_aggregate_byte_budget() {
    let mut target = b"refs/tags/target".to_vec();
    target.resize(1024, b'x');
    let names = (0..33)
        .map(|i| {
            let mut n = format!("refs/tags/name{i:02}").into_bytes();
            n.resize(1024, b'x');
            n
        })
        .collect::<Vec<_>>();
    let input = names
        .iter()
        .map(|name| Input {
            symref_target: Some(&target),
            ..entry(name, 1, K::Blob)
        })
        .collect::<Vec<_>>();
    assert_eq!(capture(&input[..32]).unwrap().entries().len(), 32);
    assert!(capture(&input).is_err());
    target.push(b'x');
    assert!(capture(&[Input {
        symref_target: Some(&target),
        ..entry(b"refs/tags/a", 1, K::Blob)
    }])
    .is_err());
}
