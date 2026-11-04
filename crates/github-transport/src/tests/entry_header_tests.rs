use crate::{parse_sha1_pack_entry_header as parse, PackEntryKind};
use std::io::ErrorKind;

#[test]
fn ordinary_types_sizes_and_tail_are_preserved() {
    for (code, kind) in [
        (1, PackEntryKind::Commit),
        (2, PackEntryKind::Tree),
        (3, PackEntryKind::Blob),
        (4, PackEntryKind::Tag),
    ] {
        let input = [0x80 | (code << 4) | 1, 0x02, 0x78, 0x9c];
        let header = parse(&input, 12, 33).unwrap();
        assert_eq!(header.kind(), kind);
        assert_eq!(header.inflated_bytes(), 33);
        assert_eq!(header.header_bytes(), 2);
        assert_eq!(&input[header.header_bytes()..], &[0x78, 0x9c]);
    }
    assert_eq!(parse(&[0x30], 12, 0).unwrap().inflated_bytes(), 0);
    assert_eq!(
        parse(&[0xb0, 0x80, 0x80, 0x80, 0x40], 12, 2 << 30)
            .unwrap()
            .inflated_bytes(),
        2 << 30
    );
}

#[test]
fn delta_bases_are_bounded_metadata_only() {
    // Git offset encoding 0x80,0 means 128, rather than ordinary varint zero.
    let header = parse(&[0x61, 0x80, 0x00, 0x78], 140, 1).unwrap();
    assert_eq!(
        header.kind(),
        PackEntryKind::OffsetDelta { base_offset: 12 }
    );
    assert_eq!(header.header_bytes(), 3);
    assert_eq!(
        parse(&[0x60, 1], 13, 0).unwrap().kind(),
        PackEntryKind::OffsetDelta { base_offset: 12 }
    );
    let mut input = vec![0x72];
    input.extend([0xab; 20]);
    input.push(0x78);
    let header = parse(&input, 12, 2).unwrap();
    assert_eq!(
        header.kind(),
        PackEntryKind::ReferenceDelta {
            base_oid: [0xab; 20]
        }
    );
    assert_eq!(header.header_bytes(), 21);
}

#[test]
fn invalid_types_bounds_and_bases_fail_closed() {
    for input in [&[0x00][..], &[0x50], &[0x61, 0], &[0x61, 2]] {
        assert_eq!(
            parse(input, 13, 10).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }
    let mut zero_base = [0; 21];
    zero_base[0] = 0x70;
    assert_eq!(
        parse(&zero_base, 12, 0).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert!(parse(&[0x31], 12, 0).is_err());
    for offset in [0, 11, (2 << 30) + 1, u64::MAX] {
        assert_eq!(
            parse(&[0x30], offset, 1).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        parse(&[0x30], 12, (2 << 30) + 1).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn truncated_and_overflowing_headers_never_read_tail_or_panic() {
    for input in [&[][..], &[0xb0], &[0x60], &[0x60, 0x80], &[0x70, 1, 2]] {
        assert_eq!(
            parse(input, 1024, 1024).unwrap_err().kind(),
            ErrorKind::UnexpectedEof
        );
    }
    let mut overflowing_size = vec![0xb0];
    overflowing_size.extend([0xff; 20]);
    assert_eq!(
        parse(&overflowing_size, 1024, 2 << 30).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    let mut overflowing_offset = vec![0x60];
    overflowing_offset.extend([0xff; 20]);
    assert_eq!(
        parse(&overflowing_offset, 2 << 30, 1).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    // Exhaustive short byte pairs exercise every first-byte type/continuation.
    for first in 0..=255 {
        for second in 0..=255 {
            if let Ok(header) = parse(&[first, second], 1024, 1024) {
                assert!(header.header_bytes() <= 2);
                assert!(header.inflated_bytes() <= 1024);
            }
        }
    }
}
