use crate::{inflate_pack_representation as inflate, CancellationHandle};
use std::{
    io::ErrorKind,
    time::{Duration, Instant},
};
fn run(
    input: &[u8],
    expected: u64,
    limit: usize,
) -> std::io::Result<crate::InflatedRepresentation> {
    inflate(
        input,
        expected,
        limit,
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5),
    )
}
const EMPTY: &[u8] = &[120, 156, 3, 0, 0, 0, 0, 1];
const HELLO: &[u8] = &[
    120, 156, 203, 72, 205, 201, 201, 87, 40, 207, 47, 202, 73, 81, 4, 0, 30, 137, 4, 126,
];
const LARGE: &[u8] = &[
    120, 156, 237, 193, 49, 1, 0, 0, 0, 194, 160, 218, 139, 111, 13, 15, 160, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 87, 3, 126, 42,
    37, 186,
];

#[test]
fn independently_encoded_streams_have_exact_output_and_consumption() {
    for (input, expected) in [(EMPTY, &b""[..]), (HELLO, &b"hello world!"[..])] {
        let result = run(input, expected.len() as u64, expected.len()).unwrap();
        assert_eq!(result.bytes(), expected);
        assert_eq!(result.compressed_bytes(), input.len());
        assert_eq!(result.into_bytes(), expected);
    }
    let result = run(LARGE, 100_000, 100_000).unwrap();
    assert_eq!(result.bytes(), vec![b'x'; 100_000]);
    assert_eq!(result.compressed_bytes(), LARGE.len());
}
#[test]
fn adjacent_streams_and_pack_tail_remain_unconsumed() {
    let mut input = HELLO.to_vec();
    input.extend(EMPTY);
    input.extend(b"PACK-tail");
    let result = run(&input, 12, 12).unwrap();
    assert_eq!(result.compressed_bytes(), HELLO.len());
    let rest = &input[result.compressed_bytes()..];
    let next = run(rest, 0, 0).unwrap();
    assert_eq!(next.compressed_bytes(), EMPTY.len());
    assert_eq!(&rest[next.compressed_bytes()..], b"PACK-tail");
}
#[test]
fn truncated_invalid_checksum_and_wrong_declared_size_are_rejected() {
    for cut in 0..HELLO.len() {
        assert!(run(&HELLO[..cut], 12, 12).is_err());
    }
    let mut damaged = HELLO.to_vec();
    *damaged.last_mut().unwrap() ^= 1;
    assert_eq!(
        run(&damaged, 12, 12).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    for expected in [0, 1, 11, 13, 100] {
        assert!(run(HELLO, expected, 100).is_err());
    }
    assert!(run(&[0, 0, 0, 0], 0, 0).is_err());
    // A tiny declared result cannot grow to match a highly compressible stream.
    assert!(run(LARGE, 1, 1).is_err());
}
#[test]
fn resource_limits_cancel_and_deadline_fail_before_returning_output() {
    assert_eq!(
        run(EMPTY, 0, 16 * 1024 * 1024 + 1).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert!(run(EMPTY, 16 * 1024 * 1024 + 1, 16 * 1024 * 1024).is_err());
    let cancel = CancellationHandle::new();
    cancel.cancel();
    assert_eq!(
        inflate(
            HELLO,
            12,
            12,
            &cancel,
            Instant::now() + Duration::from_secs(1)
        )
        .unwrap_err()
        .kind(),
        ErrorKind::ConnectionAborted
    );
    assert_eq!(
        inflate(HELLO, 12, 12, &CancellationHandle::new(), Instant::now())
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
}

#[test]
fn non_zlib_wrappers_dictionaries_and_oversized_input_are_refused() {
    assert!(run(
        &[203, 72, 205, 201, 201, 87, 40, 207, 47, 202, 73, 81, 4, 0],
        12,
        12
    )
    .is_err());
    assert!(run(
        &[
            31, 139, 8, 0, 0, 0, 0, 0, 0, 255, 203, 72, 205, 201, 201, 87, 40, 207, 47, 202, 73,
            81, 4, 0, 109, 194, 180, 3, 12, 0, 0, 0
        ],
        12,
        12
    )
    .is_err());
    assert!(run(
        &[120, 187, 30, 137, 4, 126, 203, 64, 98, 3, 0, 30, 137, 4, 126],
        12,
        12
    )
    .is_err());
    let oversized = vec![0; 16 * 1024 * 1024 + 1];
    assert_eq!(
        run(&oversized, 0, 0).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}
