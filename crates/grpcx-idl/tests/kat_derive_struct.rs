//! KAT for `#[derive(GrpcxIdl)] struct …` round-trips +
//! rejects.

use grpcx_idl::{Error, from_bytes, to_bytes};
use grpcx_idl_derive::GrpcxIdl;

#[derive(GrpcxIdl, Debug, PartialEq, Eq)]
struct Point {
    x: u32,
    y: u32,
}

#[derive(GrpcxIdl, Debug, PartialEq, Eq)]
struct Mixed {
    a: u8,
    b: u32,
    c: bool,
}

#[derive(GrpcxIdl, Debug, PartialEq, Eq)]
struct Wrap {
    inner: (u32, u8),
}

// ============================================================
// Round-trips.
// ============================================================

#[test]
fn point_round_trip() {
    let p = Point {
        x: 0xDEAD_BEEF,
        y: 0xCAFE_BABE,
    };
    let bytes = to_bytes(&p).unwrap();
    // size_of::<ArchivedPoint> = 8 (two u32 fields).
    assert_eq!(bytes.len(), 8);
    let archived = from_bytes::<Point>(&bytes).unwrap();
    assert_eq!(archived.x, 0xDEAD_BEEF);
    assert_eq!(archived.y, 0xCAFE_BABE);
}

#[test]
fn mixed_round_trip_with_padding() {
    let m = Mixed {
        a: 0xFF,
        b: 0x1122_3344,
        c: true,
    };
    let bytes = to_bytes(&m).unwrap();
    // size_of::<ArchivedMixed>: u8 at offset 0 (align 1),
    // pad 3 bytes, u32 at offset 4 (align 4), bool at offset 8
    // (align 1), pad 3 bytes (align to 4 for u32 inside).
    // Total 12, align 4.
    assert_eq!(bytes.len(), 12);
    assert_eq!(bytes[0], 0xFF);
    assert_eq!(&bytes[4..8], &[0x44, 0x33, 0x22, 0x11]);
    assert_eq!(bytes[8], 0x01);
    let archived = from_bytes::<Mixed>(&bytes).unwrap();
    assert_eq!(archived.a, 0xFF);
    assert_eq!(archived.b, 0x1122_3344);
    assert!(archived.c);
}

#[test]
fn wrap_nested_tuple_round_trip() {
    let w = Wrap {
        inner: (0xAABB_CCDD, 0x42),
    };
    let bytes = to_bytes(&w).unwrap();
    // ArchivedWrap holds an ArchivedTuple2<u32, u8> at offset 0,
    // which is align 4 size 8.
    assert_eq!(bytes.len(), 8);
    let archived = from_bytes::<Wrap>(&bytes).unwrap();
    assert_eq!(archived.inner.0, 0xAABB_CCDD);
    assert_eq!(archived.inner.1, 0x42);
}

#[test]
fn deserialize_round_trip() {
    use grpcx_idl::Deserialize;

    let p = Point { x: 1, y: 2 };
    let bytes = to_bytes(&p).unwrap();
    let archived = from_bytes::<Point>(&bytes).unwrap();
    let p2: Point = <Point as Deserialize<()>>::deserialize(archived, &mut ()).unwrap();
    assert_eq!(p2, p);
}

// ============================================================
// Reject paths.
// ============================================================

#[test]
fn truncated_rejected() {
    let bytes: Vec<u8> = vec![0u8; 7];
    assert!(matches!(
        from_bytes::<Point>(&bytes).err(),
        Some(Error::Truncated)
    ));
}

#[test]
fn oversize_rejected() {
    let bytes: Vec<u8> = vec![0u8; 9];
    assert!(matches!(
        from_bytes::<Point>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}

#[test]
fn inner_bool_rejected() {
    // Mixed: a=0, pad, b=0, c byte must be ∈ {0,1}.
    let bytes: Vec<u8> = vec![0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0];
    assert!(matches!(
        from_bytes::<Mixed>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}
