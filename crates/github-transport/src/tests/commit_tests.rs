use crate::{parse_sha1_commit as parse, CancellationHandle};
use std::{
    io::ErrorKind,
    time::{Duration, Instant},
};
const TREE: &str = "1111111111111111111111111111111111111111";
const PARENT: &str = "abababababababababababababababababababab";
fn make(parents: usize, optional: &[u8], message: &[u8]) -> Vec<u8> {
    let mut out = format!("tree {TREE}\n").into_bytes();
    for _ in 0..parents {
        out.extend(format!("parent {PARENT}\n").as_bytes());
    }
    out.extend(b"author A <a@b> 0 +0000\ncommitter C <c@d> 0 +0000\n");
    out.extend(optional);
    out.push(b'\n');
    out.extend(message);
    out
}
fn run(input: &[u8], cap: usize) -> std::io::Result<crate::GitCommitLinks> {
    parse(
        input,
        cap,
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5),
    )
}
#[test]
fn root_merge_headers_and_opaque_message_keep_link_metadata() {
    let root = make(0, b"", b"root\n");
    let links = run(&root, 0).unwrap();
    assert_eq!(links.tree(), &[0x11; 20]);
    assert!(links.parents().is_empty());
    let raw = make(
        2,
        b"encoding UTF-8\ngpgsig signature\n continuation\n \nmergetag object\n parent fake\n",
        b"parent fake\n\0\xff\rmessage",
    );
    let links = run(&raw, 2).unwrap();
    assert_eq!(links.parents(), &[[0xab; 20], [0xab; 20]]);
    assert_eq!(
        &raw[links.header_bytes()..],
        b"parent fake\n\0\xff\rmessage"
    );
}
#[test]
fn missing_misplaced_duplicate_and_invalid_core_headers_fail() {
    let valid = make(1, b"", b"");
    for prefix in [
        b"".as_slice(),
        b"tree ",
        b"parent ",
        b"tree 1111111111111111111111111111111111111111\n",
    ] {
        assert!(run(prefix, 2).is_err());
    }
    for (from, to) in [
        ("tree ", "Tree "),
        ("author ", "other "),
        ("committer ", "other "),
        (TREE, "0000000000000000000000000000000000000000"),
        (PARENT, "ABABABABABABABABABABABABABABABABABABABAB"),
    ] {
        let mutated = String::from_utf8(valid.clone()).unwrap().replace(from, to);
        assert!(run(mutated.as_bytes(), 2).is_err());
    }
    for extra in [
        format!("tree {TREE}\n"),
        format!("parent {PARENT}\n"),
        "author duplicate\n".into(),
        "committer duplicate\n".into(),
        " continuation-without-optional\n".into(),
        "novalue\n".into(),
        "x \n".into(),
        "bad_key value\n".into(),
        "x value\r\n".into(),
        "x value\0\n".into(),
    ] {
        assert!(run(&make(0, extra.as_bytes(), b""), 2).is_err());
    }
    let no_separator = &valid[..valid.len() - 1];
    assert!(run(no_separator, 2).is_err());
}
#[test]
fn limits_bound_parents_lines_headers_and_input() {
    let max = make(1024, b"", b"");
    assert_eq!(run(&max, 1024).unwrap().parents().len(), 1024);
    assert!(run(&make(1025, b"", b""), 1024).is_err());
    assert!(run(&make(1, b"", b""), 0).is_err());
    assert_eq!(run(b"", 1025).unwrap_err().kind(), ErrorKind::InvalidInput);
    let mut line = b"x ".to_vec();
    line.extend(vec![b'v'; 8190]);
    line.push(b'\n');
    assert!(run(&make(0, &line, b""), 0).is_ok());
    line.insert(2, b'v');
    assert!(run(&make(0, &line, b""), 0).is_err());
    let large_header = vec![b"x v\n".as_slice(); 2048].concat();
    assert!(run(&make(0, &large_header, b""), 0).is_err());
    let mut header = b"x ".to_vec();
    header.extend(vec![b'v'; 7998]);
    header.push(b'\n');
    let over = [header.as_slice(); 9].concat();
    assert!(run(&make(0, &over, b""), 0).is_err());
    assert!(run(&vec![0; 16 * 1024 * 1024 + 1], 0).is_err());
}
#[test]
fn attempt_controls_and_truncation_do_not_return_partial_links() {
    let raw = make(2, b"", b"");
    for cut in 0..raw.len() {
        assert!(run(&raw[..cut], 2).is_err());
    }
    let cancel = CancellationHandle::new();
    cancel.cancel();
    assert_eq!(
        parse(&raw, 2, &cancel, Instant::now() + Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        ErrorKind::ConnectionAborted
    );
    assert_eq!(
        parse(&raw, 2, &CancellationHandle::new(), Instant::now())
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
}
