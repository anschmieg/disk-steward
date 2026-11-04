use crate::{parse_sha1_tag as parse, CancellationHandle, GitObjectKind};
use std::{
    io::ErrorKind,
    time::{Duration, Instant},
};
const ID: &str = "abababababababababababababababababababab";
fn make(kind: &str, name: &[u8], tagger: bool, message: &[u8]) -> Vec<u8> {
    let mut out = format!("object {ID}\ntype {kind}\ntag ").into_bytes();
    out.extend(name);
    out.push(b'\n');
    if tagger {
        out.extend(b"tagger T <t@e> 0 +0000\n");
    }
    out.push(b'\n');
    out.extend(message);
    out
}
fn run(input: &[u8]) -> std::io::Result<crate::GitTagLink<'_>> {
    parse(
        input,
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5),
    )
}
#[test]
fn target_kinds_borrowed_names_and_opaque_signature_message_are_preserved() {
    for (kind, expected) in [
        ("commit", GitObjectKind::Commit),
        ("tree", GitObjectKind::Tree),
        ("blob", GitObjectKind::Blob),
        ("tag", GitObjectKind::Tag),
    ] {
        for tagger in [false, true] {
            let raw = make(
                kind,
                b"releases/raw\xff",
                tagger,
                b"object fake\n-----BEGIN PGP SIGNATURE-----\n\0\r\xff",
            );
            let link = run(&raw).unwrap();
            assert_eq!(link.target(), &[0xab; 20]);
            assert_eq!(link.target_kind(), expected);
            assert_eq!(link.name(), b"releases/raw\xff");
            assert_eq!(
                link.name().as_ptr(),
                raw[raw.windows(4).position(|b| b == b"tag ").unwrap() + 4..].as_ptr()
            );
            assert_eq!(
                &raw[link.header_bytes()..],
                b"object fake\n-----BEGIN PGP SIGNATURE-----\n\0\r\xff"
            );
        }
    }
}
#[test]
fn malformed_core_headers_names_and_extra_headers_are_refused() {
    for name in [
        b"".as_slice(),
        b".bad",
        b"a..b",
        b"a.lock",
        b"a b",
        b"a//b",
        b"a/",
        b"a\0b",
        b"a\rb",
    ] {
        assert!(run(&make("commit", name, true, b"")).is_err());
    }
    let valid = String::from_utf8(make("commit", b"v1", true, b"")).unwrap();
    for (from, to) in [
        (ID, "0000000000000000000000000000000000000000"),
        (ID, "ABABABABABABABABABABABABABABABABABABABAB"),
        ("object ", "Object "),
        ("type commit", "type delta"),
        ("tag v1", "tag "),
        ("tagger T <t@e> 0 +0000", "tagger "),
    ] {
        assert!(run(valid.replace(from, to).as_bytes()).is_err());
    }
    for extra in [
        "object duplicate\n",
        "type blob\n",
        "tag other\n",
        " continuation\n",
        "other value\n",
    ] {
        let raw = valid.replace("\n\n", &format!("\n{extra}\n"));
        assert!(run(raw.as_bytes()).is_err());
    }
    let no_tagger = make("tag", b"v1", false, b"");
    assert!(run(&no_tagger[..no_tagger.len() - 1]).is_err());
}
#[test]
fn truncation_limits_and_attempt_controls_never_return_partial_links() {
    let raw = make("commit", b"v1", true, b"");
    for cut in 0..raw.len() {
        assert!(run(&raw[..cut]).is_err());
    }
    assert!(run(&make("commit", &vec![b'a'; 1024], true, b"")).is_ok());
    assert!(run(&make("commit", &vec![b'a'; 1025], true, b"")).is_err());
    let mut long = make("commit", b"v1", false, b"");
    long.truncate(long.len() - 1);
    long.extend(b"tagger ");
    long.extend(vec![b'x'; 8192]);
    long.extend(b"\n\n");
    assert!(run(&long).is_err());
    assert!(run(&vec![0; 16 * 1024 * 1024 + 1]).is_err());
    let cancel = CancellationHandle::new();
    cancel.cancel();
    assert_eq!(
        parse(&raw, &cancel, Instant::now() + Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        ErrorKind::ConnectionAborted
    );
    assert_eq!(
        parse(&raw, &CancellationHandle::new(), Instant::now())
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
}
