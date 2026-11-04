use crate::{git_sha1_object_id as hash, CancellationHandle, GitObjectKind};
use std::{
    io::ErrorKind,
    time::{Duration, Instant},
};
fn run(kind: GitObjectKind, content: &[u8]) -> std::io::Result<[u8; 20]> {
    hash(
        kind,
        content,
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5),
    )
}
#[test]
fn independent_git_hash_vectors_bind_type_and_exact_bytes() {
    assert_eq!(
        run(GitObjectKind::Blob, &[]).unwrap(),
        [
            230, 157, 226, 155, 178, 209, 214, 67, 75, 139, 41, 174, 119, 90, 216, 194, 228, 140,
            83, 145
        ]
    );
    assert_eq!(
        run(GitObjectKind::Tree, &[]).unwrap(),
        [
            75, 130, 93, 198, 66, 203, 110, 185, 160, 96, 229, 75, 248, 214, 146, 136, 251, 238,
            73, 4
        ]
    );
    assert_eq!(
        run(
            GitObjectKind::Blob,
            &[116, 101, 115, 116, 32, 99, 111, 110, 116, 101, 110, 116, 10]
        )
        .unwrap(),
        [
            214, 112, 70, 11, 75, 74, 236, 229, 145, 92, 175, 92, 104, 209, 47, 86, 10, 159, 227,
            228
        ]
    );
    assert_eq!(
        run(
            GitObjectKind::Blob,
            &[104, 101, 108, 108, 111, 32, 119, 111, 114, 108, 100, 33]
        )
        .unwrap(),
        [
            188, 119, 116, 167, 177, 141, 235, 29, 123, 208, 33, 45, 52, 36, 106, 155, 18, 96, 174,
            23
        ]
    );
    assert_eq!(
        run(
            GitObjectKind::Commit,
            &[
                105, 110, 118, 97, 108, 105, 100, 32, 99, 111, 109, 109, 105, 116, 32, 115, 121,
                110, 116, 97, 120
            ]
        )
        .unwrap(),
        [
            20, 155, 118, 111, 0, 177, 99, 109, 148, 127, 86, 62, 132, 95, 232, 216, 240, 247, 109,
            165
        ]
    );
    assert_eq!(
        run(
            GitObjectKind::Tag,
            &[105, 110, 118, 97, 108, 105, 100, 32, 116, 97, 103, 32, 115, 121, 110, 116, 97, 120]
        )
        .unwrap(),
        [
            241, 241, 33, 191, 8, 143, 193, 104, 96, 95, 189, 114, 248, 6, 196, 223, 235, 110, 74,
            212
        ]
    );
    assert_ne!(
        run(GitObjectKind::Blob, b"").unwrap(),
        run(GitObjectKind::Tree, b"").unwrap()
    );
    assert_ne!(
        run(GitObjectKind::Blob, b"a").unwrap(),
        run(GitObjectKind::Blob, b"a\0").unwrap()
    );
}
#[test]
fn chunk_and_decimal_length_boundaries_match_independent_hashes() {
    assert_eq!(
        run(GitObjectKind::Blob, &[0xab; 9]).unwrap(),
        [
            164, 19, 235, 111, 102, 124, 161, 185, 40, 170, 22, 99, 192, 108, 157, 21, 73, 158,
            170, 134
        ]
    );
    assert_eq!(
        run(GitObjectKind::Blob, &[0xab; 10]).unwrap(),
        [
            168, 102, 130, 6, 116, 62, 229, 235, 50, 181, 22, 216, 210, 205, 229, 8, 180, 123, 209,
            185
        ]
    );
    assert_eq!(
        run(GitObjectKind::Blob, &[0xab; 99]).unwrap(),
        [
            70, 255, 37, 246, 202, 242, 170, 113, 195, 42, 234, 42, 188, 236, 46, 112, 166, 83, 34,
            195
        ]
    );
    assert_eq!(
        run(GitObjectKind::Blob, &[0xab; 100]).unwrap(),
        [
            145, 3, 35, 58, 129, 51, 20, 240, 135, 8, 110, 15, 115, 124, 124, 143, 32, 248, 155,
            134
        ]
    );
    assert_eq!(
        run(GitObjectKind::Blob, &vec![0xab; 8191]).unwrap(),
        [
            38, 131, 224, 156, 250, 76, 127, 70, 93, 222, 84, 150, 10, 18, 154, 144, 18, 33, 177,
            181
        ]
    );
    assert_eq!(
        run(GitObjectKind::Blob, &vec![0xab; 8192]).unwrap(),
        [
            41, 131, 160, 226, 142, 97, 66, 77, 223, 36, 233, 192, 150, 168, 48, 230, 212, 219, 31,
            19
        ]
    );
    assert_eq!(
        run(GitObjectKind::Blob, &vec![0xab; 8193]).unwrap(),
        [234, 28, 85, 110, 179, 53, 48, 63, 86, 55, 73, 23, 180, 228, 9, 23, 175, 167, 56, 80]
    );
    assert_eq!(
        run(GitObjectKind::Blob, &vec![0xab; 16384]).unwrap(),
        [2, 144, 20, 33, 147, 181, 245, 71, 187, 99, 4, 246, 189, 157, 186, 95, 136, 35, 132, 18]
    );
}
#[test]
fn oversized_canceled_and_expired_hashes_do_not_return_ids() {
    let cancel = CancellationHandle::new();
    cancel.cancel();
    assert_eq!(
        hash(
            GitObjectKind::Blob,
            b"",
            &cancel,
            Instant::now() + Duration::from_secs(1)
        )
        .unwrap_err()
        .kind(),
        ErrorKind::ConnectionAborted
    );
    assert_eq!(
        hash(
            GitObjectKind::Blob,
            b"",
            &CancellationHandle::new(),
            Instant::now()
        )
        .unwrap_err()
        .kind(),
        ErrorKind::TimedOut
    );
    assert_eq!(
        run(GitObjectKind::Blob, &vec![0; 16 * 1024 * 1024 + 1])
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidData
    );
}
