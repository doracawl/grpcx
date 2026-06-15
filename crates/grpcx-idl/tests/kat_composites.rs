//! KAT round-trip + reject coverage for L7 v0.3 fixed-size
//! composites (tuples 2-8 + arrays).

use grpcx_idl::{ArchivedTuple2, ArchivedTuple3, Error, from_bytes, to_bytes};

// ============================================================
// Tuple round-trips.
// ============================================================

#[test]
fn tuple2_no_padding_round_trip() {
    // `(u8, u8)` — align 1, size 2, no padding.
    let v: (u8, u8) = (0xAB, 0xCD);
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes, [0xAB, 0xCD]);
    let archived: &ArchivedTuple2<u8, u8> = from_bytes::<(u8, u8)>(&bytes).unwrap();
    assert_eq!(archived.0, 0xAB);
    assert_eq!(archived.1, 0xCD);
}

#[test]
fn tuple2_aligned_fields_no_padding() {
    // `(u16, u32)` — align 4, size 8 (no padding between fields
    // since u16 ends at offset 2, u32 needs offset 4, so 2 pad
    // bytes between).
    let v: (u16, u32) = (0x1122, 0xDEAD_BEEF);
    let bytes = to_bytes(&v).unwrap();
    // u16 LE = [0x22, 0x11]; 2 padding bytes [0, 0]; u32 LE =
    // [0xEF, 0xBE, 0xAD, 0xDE].
    assert_eq!(bytes, [0x22, 0x11, 0x00, 0x00, 0xEF, 0xBE, 0xAD, 0xDE]);
    let archived = from_bytes::<(u16, u32)>(&bytes).unwrap();
    assert_eq!(archived.0, 0x1122);
    assert_eq!(archived.1, 0xDEAD_BEEF);
}

#[test]
fn tuple2_field_order_packs_correctly() {
    // `(u32, u8)` — u32 at offset 0, u8 at offset 4, trailing
    // pad to size 8. Wire: [u32 LE][u8][3 pad bytes].
    let v: (u32, u8) = (0xDEAD_BEEF, 0x42);
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes.len(), 8);
    assert_eq!(&bytes[..4], &[0xEF, 0xBE, 0xAD, 0xDE]);
    assert_eq!(bytes[4], 0x42);
    assert_eq!(&bytes[5..], &[0u8; 3]);
    let archived = from_bytes::<(u32, u8)>(&bytes).unwrap();
    assert_eq!(archived.0, 0xDEAD_BEEF);
    assert_eq!(archived.1, 0x42);
}

#[test]
fn tuple3_round_trip() {
    let v: (u32, u16, u8) = (0xCAFE_BABE, 0x1234, 0xFF);
    let bytes = to_bytes(&v).unwrap();
    let archived: &ArchivedTuple3<u32, u16, u8> = from_bytes::<(u32, u16, u8)>(&bytes).unwrap();
    assert_eq!(archived.0, 0xCAFE_BABE);
    assert_eq!(archived.1, 0x1234);
    assert_eq!(archived.2, 0xFF);
    // Reverse the round-trip: encode the archived's fields
    // again should land the same bytes.
    let rt = to_bytes(&(archived.0, archived.1, archived.2)).unwrap();
    assert_eq!(rt, bytes);
}

#[test]
fn tuple_with_bool_round_trip() {
    let v: (bool, u32) = (true, 0xAABB_CCDD);
    let bytes = to_bytes(&v).unwrap();
    // bool at offset 0 (1 byte); 3 pad; u32 at offset 4.
    assert_eq!(bytes.len(), 8);
    assert_eq!(bytes[0], 0x01);
    assert_eq!(&bytes[1..4], &[0u8; 3]);
    assert_eq!(&bytes[4..], &[0xDD, 0xCC, 0xBB, 0xAA]);
    let archived = from_bytes::<(bool, u32)>(&bytes).unwrap();
    assert!(archived.0);
    assert_eq!(archived.1, 0xAABB_CCDD);
}

#[test]
fn nested_tuple_round_trip() {
    // `((u32, u8), u16)` — inner tuple is align-4, size 8;
    // outer is align-4, size 12 (8 + 2 + 2 trailing pad).
    let v: ((u32, u8), u16) = ((0x1122_3344, 0x55), 0x66_77);
    let bytes = to_bytes(&v).unwrap();
    let archived = from_bytes::<((u32, u8), u16)>(&bytes).unwrap();
    let inner = &archived.0;
    assert_eq!(inner.0, 0x1122_3344);
    assert_eq!(inner.1, 0x55);
    assert_eq!(archived.1, 0x66_77);
}

#[test]
fn tuple8_max_arity_round_trip() {
    let v: (u8, u8, u8, u8, u8, u8, u8, u8) = (1, 2, 3, 4, 5, 6, 7, 8);
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes, [1, 2, 3, 4, 5, 6, 7, 8]);
    let archived = from_bytes::<(u8, u8, u8, u8, u8, u8, u8, u8)>(&bytes).unwrap();
    assert_eq!(archived.0, 1);
    assert_eq!(archived.7, 8);
}

// ============================================================
// Tuple reject paths.
// ============================================================

#[test]
fn tuple_truncated_rejected() {
    // `(u32, u8)` wire size = 8; supplying 7 bytes truncates.
    let bytes = vec![0u8; 7];
    assert!(matches!(
        from_bytes::<(u32, u8)>(&bytes).err(),
        Some(Error::Truncated)
    ));
}

#[test]
fn tuple_oversize_rejected() {
    let bytes = vec![0u8; 9];
    assert!(matches!(
        from_bytes::<(u32, u8)>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}

#[test]
fn tuple_inner_bool_byte_rejected() {
    // `(bool, u32)`: the bool byte at offset 0 must be 0 or 1.
    let bytes = vec![0x02, 0, 0, 0, 0, 0, 0, 0];
    assert!(matches!(
        from_bytes::<(bool, u32)>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}

// ============================================================
// Array round-trips.
// ============================================================

#[test]
fn array_u8_round_trip() {
    let v: [u8; 4] = [0xAB, 0xCD, 0xEF, 0x01];
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes, v);
    let archived = from_bytes::<[u8; 4]>(&bytes).unwrap();
    assert_eq!(archived, &v);
}

#[test]
fn array_u32_round_trip() {
    let v: [u32; 3] = [0x1111_1111, 0x2222_2222, 0x3333_3333];
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes.len(), 12);
    let archived = from_bytes::<[u32; 3]>(&bytes).unwrap();
    assert_eq!(archived[0], 0x1111_1111);
    assert_eq!(archived[2], 0x3333_3333);
}

#[test]
fn array_of_tuple_round_trip() {
    // `[(u16, u8); 2]` — each element align 2, size 4 (2 + 1 + 1 pad).
    let v: [(u16, u8); 2] = [(0x1234, 0x56), (0x789A, 0xBC)];
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes.len(), 8);
    let archived = from_bytes::<[(u16, u8); 2]>(&bytes).unwrap();
    assert_eq!(archived[0].0, 0x1234);
    assert_eq!(archived[0].1, 0x56);
    assert_eq!(archived[1].0, 0x789A);
    assert_eq!(archived[1].1, 0xBC);
}

#[test]
fn array_bool_round_trip() {
    let v: [bool; 8] = [true, false, true, false, true, true, false, true];
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes, [1, 0, 1, 0, 1, 1, 0, 1]);
    let archived = from_bytes::<[bool; 8]>(&bytes).unwrap();
    assert!(archived[0]);
    assert!(!archived[3]);
    assert!(archived[7]);
}

#[test]
fn array_zero_length_handled() {
    // `[u8; 0]` is layout-equivalent to () — wire size 0.
    let v: [u8; 0] = [];
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes, Vec::<u8>::new());
    // from_bytes with empty input must accept it (length matches).
    let archived = from_bytes::<[u8; 0]>(&bytes).unwrap();
    assert_eq!(archived.len(), 0);
}

// ============================================================
// Array reject paths.
// ============================================================

#[test]
fn array_truncated_rejected() {
    // `[u32; 3]` wire size = 12; 11 bytes truncates.
    let bytes = vec![0u8; 11];
    assert!(matches!(
        from_bytes::<[u32; 3]>(&bytes).err(),
        Some(Error::Truncated)
    ));
}

#[test]
fn array_oversize_rejected() {
    let bytes = vec![0u8; 13];
    assert!(matches!(
        from_bytes::<[u32; 3]>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}

#[test]
fn array_bad_bool_byte_rejected() {
    let bytes = vec![1, 0, 5, 1];
    assert!(matches!(
        from_bytes::<[bool; 4]>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}
