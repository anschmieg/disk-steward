use crate::{reconstruct_git_delta as apply, CancellationHandle};
use std::{
    io::ErrorKind,
    time::{Duration, Instant},
};

fn run(base: &[u8], delta: &[u8], limit: usize) -> std::io::Result<Vec<u8>> {
    apply(
        base,
        delta,
        limit,
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5),
    )
}
fn size(mut value: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 127) as u8;
        value >>= 7;
        out.push(byte | if value != 0 { 128 } else { 0 });
        if value == 0 {
            return out;
        }
    }
}

#[test]
fn reconstructs_mixed_copy_and_literal_instructions() {
    // Base/result lengths, copy offset=0 size=6, insert 5, copy offset=6 size=6.
    assert_eq!(
        run(
            b"hello world!",
            &[12, 17, 0x90, 6, 5, b'b', b'r', b'a', b'v', b'e', 0x91, 6, 6],
            17
        )
        .unwrap(),
        b"hello braveworld!"
    );
    assert_eq!(run(b"", &[0, 3, 3, b'a', b'b', b'c'], 3).unwrap(), b"abc");
    assert_eq!(run(b"", &[0, 0], 0).unwrap(), b"");
}

#[test]
fn copy_zero_size_means_64k_and_sparse_offset_bytes_keep_positions() {
    let base: Vec<u8> = (0..131_072).map(|n| (n % 251) as u8).collect();
    let mut delta = size(base.len());
    delta.extend(size(65536));
    delta.push(0x80);
    assert_eq!(run(&base, &delta, 65536).unwrap(), base[..65536]);
    // Only offset byte 2 present: 1 << 16. Explicit one-byte size.
    let mut delta = size(base.len());
    delta.extend(size(1));
    delta.extend([0x94, 1, 1]);
    assert_eq!(run(&base, &delta, 1).unwrap(), base[65536..65537]);
}

#[test]
fn rejects_wrong_base_invalid_ranges_reserved_opcodes_and_sizes() {
    for delta in [
        &[2, 0][..],
        &[1, 1, 0],
        &[1, 2, 0x90, 2],
        &[1, 1, 0x91, 1, 1],
        &[1, 1, 2, b'a', b'b'],
        &[1, 0, 1, b'x'],
        &[1, 2, 1, b'x'],
    ] {
        assert!(run(b"a", delta, 10).is_err(), "{delta:?}");
    }
    assert!(run(b"", &[0, 1, 1, b'x'], 0).is_err());
    assert_eq!(
        run(b"", &[0, 0], 16 * 1024 * 1024 + 1).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    let mut huge = size(0);
    huge.extend(size(16 * 1024 * 1024 + 1));
    assert!(run(b"", &huge, 16 * 1024 * 1024).is_err());
}

#[test]
fn truncated_and_overflowing_varints_and_instructions_fail() {
    for delta in [
        &[][..],
        &[0],
        &[0x80],
        &[0, 0x80],
        &[1, 1, 0x91],
        &[1, 1, 0x91, 0],
        &[1, 2, 2, b'x'],
    ] {
        assert_eq!(
            run(b"a", delta, 1024).unwrap_err().kind(),
            ErrorKind::UnexpectedEof
        );
    }
    assert_eq!(
        run(b"", &[0xff; 20], 1024).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}

#[test]
fn canceled_and_expired_attempts_do_not_return_results() {
    let cancel = CancellationHandle::new();
    cancel.cancel();
    assert_eq!(
        apply(
            b"",
            &[0, 0],
            0,
            &cancel,
            Instant::now() + Duration::from_secs(1)
        )
        .unwrap_err()
        .kind(),
        ErrorKind::ConnectionAborted
    );
    assert_eq!(
        apply(b"", &[0, 0], 0, &CancellationHandle::new(), Instant::now())
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
}

#[test]
fn adversarial_instruction_prefixes_are_bounded() {
    for a in 0..=255 {
        for b in 0..=255 {
            let delta = [1, 1, a, b];
            if let Ok(result) = run(b"x", &delta, 1) {
                assert_eq!(result.len(), 1);
            }
        }
    }
}

#[test]
fn instruction_count_is_bounded_even_with_tiny_output_steps() {
    let count = 1_048_577;
    let mut delta = size(0);
    delta.extend(size(count));
    for _ in 0..count {
        delta.extend([1, b'x']);
    }
    let error = run(b"", &delta, count).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "delta instruction limit exceeded");
}
