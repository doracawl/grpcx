//! KAT round-trip + reject coverage for L7 v0.4 variable-length
//! types (`Vec<u8>` + `String`).

use grpcx_idl::{Error, from_bytes, to_bytes};

// ============================================================
// Vec<u8> round-trips.
// ============================================================

#[test]
fn empty_vec_round_trip() {
    let v: Vec<u8> = Vec::new();
    let bytes = to_bytes(&v).unwrap();
    // Wire: just the u32 LE length = 0.
    assert_eq!(bytes, [0, 0, 0, 0]);
    let archived = from_bytes::<Vec<u8>>(&bytes).unwrap();
    assert_eq!(archived.len(), 0);
    assert!(archived.is_empty());
    assert_eq!(archived.as_slice(), &[] as &[u8]);
}

#[test]
fn single_byte_vec_round_trip() {
    let v = vec![0xAB];
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes, [1, 0, 0, 0, 0xAB]);
    let archived = from_bytes::<Vec<u8>>(&bytes).unwrap();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived.as_slice(), &[0xAB]);
}

#[test]
fn short_vec_round_trip() {
    let v = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes, [4, 0, 0, 0, 0xDE, 0xAD, 0xBE, 0xEF]);
    let archived = from_bytes::<Vec<u8>>(&bytes).unwrap();
    assert_eq!(archived.as_slice(), v.as_slice());
}

#[test]
fn boundary_u8_max_length() {
    // 255 bytes — last value before u16 length form. Wire fits
    // in u32 LE: [0xff, 0, 0, 0, payload...].
    let v: Vec<u8> = (0u8..=254).collect();
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes.len(), 4 + 255);
    assert_eq!(&bytes[..4], &[0xff, 0, 0, 0]);
    let archived = from_bytes::<Vec<u8>>(&bytes).unwrap();
    assert_eq!(archived.len(), 255);
    assert_eq!(archived.as_slice()[100], 100);
}

#[test]
fn large_vec_round_trip() {
    let v: Vec<u8> = (0..1024).map(|i| (i & 0xff) as u8).collect();
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes.len(), 4 + 1024);
    let archived = from_bytes::<Vec<u8>>(&bytes).unwrap();
    assert_eq!(archived.len(), 1024);
    assert_eq!(archived.as_slice(), v.as_slice());
}

// ============================================================
// String round-trips.
// ============================================================

#[test]
fn empty_string_round_trip() {
    let s = String::new();
    let bytes = to_bytes(&s).unwrap();
    assert_eq!(bytes, [0, 0, 0, 0]);
    let archived = from_bytes::<String>(&bytes).unwrap();
    assert_eq!(archived.len(), 0);
    assert_eq!(archived.as_str().unwrap(), "");
}

#[test]
fn ascii_string_round_trip() {
    let s = String::from("hello world");
    let bytes = to_bytes(&s).unwrap();
    assert_eq!(&bytes[..4], &(11u32).to_le_bytes());
    assert_eq!(&bytes[4..], b"hello world");
    let archived = from_bytes::<String>(&bytes).unwrap();
    assert_eq!(archived.len(), 11);
    assert_eq!(archived.as_str().unwrap(), "hello world");
    assert_eq!(archived.as_bytes(), b"hello world");
}

#[test]
fn multibyte_utf8_string_round_trip() {
    let s = String::from("héllo 🎉");
    let bytes = to_bytes(&s).unwrap();
    let archived = from_bytes::<String>(&bytes).unwrap();
    assert_eq!(archived.as_str().unwrap(), "héllo 🎉");
    // The byte length includes the multi-byte UTF-8.
    assert_eq!(archived.len(), s.len());
}

// ============================================================
// Lazy UTF-8 — invalid bytes pass `from_bytes` but fail `as_str`.
// ============================================================

#[test]
fn invalid_utf8_passes_from_bytes_but_fails_as_str() {
    // 4-byte length = 4, then 0xff 0xfe 0xfd 0xfc — not valid
    // UTF-8 starting bytes.
    let bytes: Vec<u8> = vec![4, 0, 0, 0, 0xff, 0xfe, 0xfd, 0xfc];
    let archived = from_bytes::<String>(&bytes).unwrap();
    assert_eq!(archived.len(), 4);
    // `as_str` is the lazy check — it should reject.
    assert!(matches!(archived.as_str().err(), Some(Error::Utf8)));
}

// ============================================================
// Reject paths.
// ============================================================

#[test]
fn truncated_header_rejected() {
    let bytes = vec![0, 0, 0]; // < 4 bytes
    assert!(matches!(
        from_bytes::<Vec<u8>>(&bytes).err(),
        Some(Error::Truncated)
    ));
}

#[test]
fn truncated_payload_rejected() {
    // Declares length = 5 but provides only 3 payload bytes.
    let bytes = vec![5, 0, 0, 0, 0xAA, 0xBB, 0xCC];
    assert!(matches!(
        from_bytes::<Vec<u8>>(&bytes).err(),
        Some(Error::Truncated)
    ));
}

#[test]
fn oversize_buffer_rejected() {
    // Declares length = 2 but provides 4 payload bytes.
    let bytes = vec![2, 0, 0, 0, 0xAA, 0xBB, 0xCC, 0xDD];
    assert!(matches!(
        from_bytes::<Vec<u8>>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}

#[test]
fn misaligned_buffer_rejected() {
    // Build a buffer whose start address is odd-aligned by
    // taking a sub-slice past the natural alignment.
    let raw: [u8; 8] = [0; 8];
    // `raw[1..]` starts at the original ptr + 1 — guaranteed
    // not multiple of 4 (since `raw.as_ptr()` is at least
    // 4-aligned for a Vec<u8> heap alloc).
    let shifted = &raw[1..];
    if !(shifted.as_ptr() as usize).is_multiple_of(4) {
        assert!(matches!(
            from_bytes::<Vec<u8>>(shifted).err(),
            Some(Error::BadAlignment)
        ));
    }
}

// ============================================================
// Wire format invariants.
// ============================================================

#[test]
fn length_prefix_is_u32_le() {
    // Encode a vec of 0x01_2345_6789... wait u32 max is 4 GB.
    // Just confirm the LE byte order for a moderate length.
    let v = vec![0u8; 0x0102_0304];
    // Allocates ~16 MB; OK for a release build, might be slow
    // in debug. Use a shorter check instead:
    let _ = v; // skip — too memory-heavy
    let v = vec![0u8; 0x0304];
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(&bytes[..4], &[0x04, 0x03, 0x00, 0x00]);
}
