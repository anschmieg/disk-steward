use crate::{parse_sha1_tree as parse, CancellationHandle, GitTreeMode};
use std::{
    io::ErrorKind,
    time::{Duration, Instant},
};
fn run(input: &[u8], cap: usize) -> std::io::Result<Vec<crate::GitTreeEntry<'_>>> {
    parse(
        input,
        cap,
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5),
    )
}
fn entry(mode: &[u8], name: &[u8], id: u8) -> Vec<u8> {
    let mut out = mode.to_vec();
    out.push(b' ');
    out.extend(name);
    out.push(0);
    out.extend([id; 20]);
    out
}
#[test]
fn canonical_modes_and_raw_names_are_lossless_metadata() {
    for (mode, kind) in [
        (b"40000".as_slice(), GitTreeMode::Directory),
        (b"100644", GitTreeMode::Regular),
        (b"100755", GitTreeMode::Executable),
        (b"120000", GitTreeMode::Symlink),
        (b"160000", GitTreeMode::Gitlink),
    ] {
        let bytes = entry(mode, b"raw\xff\nname", 0xab);
        let parsed = run(&bytes, 1).unwrap();
        assert_eq!(parsed[0].mode(), kind);
        assert_eq!(parsed[0].name(), b"raw\xff\nname");
        assert_eq!(parsed[0].oid(), &[0xab; 20]);
        assert_eq!(parsed[0].name().as_ptr(), bytes[mode.len() + 1..].as_ptr());
    }
    assert!(run(b"", 0).unwrap().is_empty());
}
#[test]
fn git_directory_order_and_duplicate_basenames_are_enforced() {
    let mut correct = entry(b"100644", b"foo.bar", 1);
    correct.extend(entry(b"40000", b"foo", 2));
    correct.extend(entry(b"100644", b"foo0", 3));
    assert_eq!(run(&correct, 3).unwrap().len(), 3);
    let mut bad = entry(b"40000", b"foo", 2);
    bad.extend(entry(b"100644", b"foo.bar", 1));
    assert!(run(&bad, 2).is_err());
    // Duplicate foo basenames can be separated by foo.bar in Git sort order.
    let mut duplicate = entry(b"100644", b"foo", 1);
    duplicate.extend(entry(b"100644", b"foo.bar", 2));
    duplicate.extend(entry(b"40000", b"foo", 3));
    assert!(run(&duplicate, 3).is_err());
    let mut repeated = entry(b"100644", b"a", 1);
    repeated.extend(entry(b"100755", b"a", 2));
    assert!(run(&repeated, 2).is_err());
}
#[test]
fn invalid_names_modes_ids_and_truncated_entries_never_return_partial_links() {
    for name in [b"".as_slice(), b".", b"..", b".git", b".GiT", b"a/b"] {
        assert!(run(&entry(b"100644", name, 1), 1).is_err());
    }
    for mode in [
        b"0".as_slice(),
        b"040000",
        b"100664",
        b"100600",
        b"1006444",
        b"text",
    ] {
        assert!(run(&entry(mode, b"x", 1), 1).is_err());
    }
    assert!(run(&entry(b"100644", b"x", 0), 1).is_err());
    let valid = entry(b"100644", b"x", 1);
    for cut in 1..valid.len() {
        assert!(run(&valid[..cut], 1).is_err());
    }
    let mut trailing = valid.clone();
    trailing.push(1);
    assert!(run(&trailing, 2).is_err());
}
#[test]
fn entry_name_input_caps_and_attempt_controls_are_bounded() {
    let at = entry(b"100644", &vec![b'a'; 1024], 1);
    assert_eq!(run(&at, 1).unwrap().len(), 1);
    assert!(run(&entry(b"100644", &vec![b'a'; 1025], 1), 1).is_err());
    assert!(run(&at, 0).is_err());
    assert_eq!(run(b"", 4097).unwrap_err().kind(), ErrorKind::InvalidInput);
    let mut many = Vec::new();
    for n in 0..4096 {
        many.extend(entry(b"100644", format!("{n:04}").as_bytes(), 1));
    }
    assert_eq!(run(&many, 4096).unwrap().len(), 4096);
    many.extend(entry(b"100644", b"zzzz", 1));
    assert!(run(&many, 4096).is_err());
    assert!(run(&vec![0; 16 * 1024 * 1024 + 1], 1).is_err());
    let cancel = CancellationHandle::new();
    cancel.cancel();
    assert_eq!(
        parse(b"", 0, &cancel, Instant::now() + Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        ErrorKind::ConnectionAborted
    );
    assert_eq!(
        parse(b"", 0, &CancellationHandle::new(), Instant::now())
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
}
