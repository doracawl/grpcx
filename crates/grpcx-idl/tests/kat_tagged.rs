//! KAT round-trip + reject coverage for L7 v0.5 tagged unions
//! (`Option<T>` + `Result<T, E>`).

use grpcx_idl::{ArchivedOption, ArchivedResult, Error, from_bytes, to_bytes};

// ============================================================
// Option<T> round-trips.
// ============================================================

#[test]
fn option_u32_none_round_trip() {
    let v: Option<u32> = None;
    let bytes = to_bytes(&v).unwrap();
    // disc=0 LE + 4 bytes of padding (where Some's u32 payload
    // would live).
    assert_eq!(bytes, [0, 0, 0, 0, 0, 0, 0, 0]);
    let archived = from_bytes::<Option<u32>>(&bytes).unwrap();
    assert!(matches!(archived, ArchivedOption::None));
}

#[test]
fn option_u32_some_round_trip() {
    let v: Option<u32> = Some(0xDEAD_BEEF);
    let bytes = to_bytes(&v).unwrap();
    // disc=1 LE + u32 LE payload.
    assert_eq!(bytes, [1, 0, 0, 0, 0xEF, 0xBE, 0xAD, 0xDE]);
    let archived = from_bytes::<Option<u32>>(&bytes).unwrap();
    match archived {
        ArchivedOption::Some(v) => assert_eq!(*v, 0xDEAD_BEEF),
        _ => panic!("expected Some"),
    }
}

#[test]
fn option_u8_round_trip() {
    let v: Option<u8> = Some(0x7F);
    let bytes = to_bytes(&v).unwrap();
    // disc=1, u8 payload at offset 4, trailing pad to enum size 8.
    assert_eq!(bytes.len(), 8);
    assert_eq!(&bytes[..4], &[1, 0, 0, 0]);
    assert_eq!(bytes[4], 0x7F);
    let archived = from_bytes::<Option<u8>>(&bytes).unwrap();
    match archived {
        ArchivedOption::Some(v) => assert_eq!(*v, 0x7F),
        _ => panic!("expected Some"),
    }
}

#[test]
fn option_u64_some_round_trip() {
    let v: Option<u64> = Some(0xCAFE_BABE_DEAD_BEEF);
    let bytes = to_bytes(&v).unwrap();
    // disc u32 at 0, pad 4 bytes, u64 payload at offset 8. Total 16.
    assert_eq!(bytes.len(), 16);
    assert_eq!(&bytes[..4], &[1, 0, 0, 0]);
    assert_eq!(&bytes[4..8], &[0, 0, 0, 0]);
    assert_eq!(&bytes[8..], &0xCAFE_BABE_DEAD_BEEFu64.to_le_bytes());
    let archived = from_bytes::<Option<u64>>(&bytes).unwrap();
    match archived {
        ArchivedOption::Some(v) => assert_eq!(*v, 0xCAFE_BABE_DEAD_BEEF),
        _ => panic!("expected Some"),
    }
}

#[test]
fn option_u64_none_round_trip() {
    let v: Option<u64> = None;
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes.len(), 16);
    assert_eq!(&bytes, &[0u8; 16]);
    let archived = from_bytes::<Option<u64>>(&bytes).unwrap();
    assert!(matches!(archived, ArchivedOption::None));
}

// ============================================================
// Result<T, E> round-trips.
// ============================================================

#[test]
fn result_symmetric_ok_round_trip() {
    let v: Result<u32, u32> = Ok(0x1122_3344);
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes.len(), 8);
    assert_eq!(&bytes[..4], &[0, 0, 0, 0]);
    assert_eq!(&bytes[4..], &0x1122_3344u32.to_le_bytes());
    let archived = from_bytes::<Result<u32, u32>>(&bytes).unwrap();
    match archived {
        ArchivedResult::Ok(v) => assert_eq!(*v, 0x1122_3344),
        _ => panic!("expected Ok"),
    }
}

#[test]
fn result_symmetric_err_round_trip() {
    let v: Result<u32, u32> = Err(0x99);
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes.len(), 8);
    assert_eq!(&bytes[..4], &[1, 0, 0, 0]);
    assert_eq!(&bytes[4..], &[0x99, 0, 0, 0]);
    let archived = from_bytes::<Result<u32, u32>>(&bytes).unwrap();
    match archived {
        ArchivedResult::Err(v) => assert_eq!(*v, 0x99),
        _ => panic!("expected Err"),
    }
}

#[test]
fn result_asymmetric_ok_round_trip() {
    // Result<u8, u64>: enum align = 8, size = 16.
    // Ok variant: disc + u8 at offset 4, trailing pad to 16.
    let v: Result<u8, u64> = Ok(0x42);
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes.len(), 16);
    assert_eq!(&bytes[..4], &[0, 0, 0, 0]);
    assert_eq!(bytes[4], 0x42);
    let archived = from_bytes::<Result<u8, u64>>(&bytes).unwrap();
    match archived {
        ArchivedResult::Ok(v) => assert_eq!(*v, 0x42),
        _ => panic!("expected Ok"),
    }
}

#[test]
fn result_asymmetric_err_round_trip() {
    let v: Result<u8, u64> = Err(0xDEAD_BEEF);
    let bytes = to_bytes(&v).unwrap();
    assert_eq!(bytes.len(), 16);
    assert_eq!(&bytes[..4], &[1, 0, 0, 0]);
    assert_eq!(&bytes[4..8], &[0, 0, 0, 0]);
    assert_eq!(&bytes[8..], &0xDEAD_BEEFu64.to_le_bytes());
    let archived = from_bytes::<Result<u8, u64>>(&bytes).unwrap();
    match archived {
        ArchivedResult::Err(v) => assert_eq!(*v, 0xDEAD_BEEF),
        _ => panic!("expected Err"),
    }
}

// ============================================================
// Reject paths.
// ============================================================

#[test]
fn option_bad_discriminant_rejected() {
    // Heap Vec<u8> for ≥ 4-aligned buffer.
    let bytes: Vec<u8> = vec![2, 0, 0, 0, 0, 0, 0, 0]; // disc=2, invalid
    assert!(matches!(
        from_bytes::<Option<u32>>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}

#[test]
fn option_truncated_rejected() {
    let bytes: Vec<u8> = vec![0u8; 7]; // need 8
    assert!(matches!(
        from_bytes::<Option<u32>>(&bytes).err(),
        Some(Error::Truncated)
    ));
}

#[test]
fn option_oversize_rejected() {
    let bytes: Vec<u8> = vec![0u8; 9]; // need exactly 8
    assert!(matches!(
        from_bytes::<Option<u32>>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}

#[test]
fn option_inner_bool_byte_rejected() {
    // Option<bool>: disc=1, then bool byte must be {0,1}.
    // Enum size: disc 4 + bool 1 = 5, pad to align 4 → 8.
    let bytes: Vec<u8> = vec![1, 0, 0, 0, 5, 0, 0, 0];
    assert!(matches!(
        from_bytes::<Option<bool>>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}

#[test]
fn result_bad_discriminant_rejected() {
    let bytes: Vec<u8> = vec![3, 0, 0, 0, 0, 0, 0, 0]; // disc=3, invalid (only 0,1)
    assert!(matches!(
        from_bytes::<Result<u32, u32>>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}

#[test]
fn result_truncated_rejected() {
    let bytes: Vec<u8> = vec![0u8; 7]; // need 8
    assert!(matches!(
        from_bytes::<Result<u32, u32>>(&bytes).err(),
        Some(Error::Truncated)
    ));
}

// ============================================================
// Composition smoke — Option of a tuple.
// ============================================================

#[test]
fn option_of_tuple_round_trip() {
    let v: Option<(u32, u8)> = Some((0xCAFE_BABE, 0x42));
    let bytes = to_bytes(&v).unwrap();
    let archived = from_bytes::<Option<(u32, u8)>>(&bytes).unwrap();
    match archived {
        ArchivedOption::Some(t) => {
            assert_eq!(t.0, 0xCAFE_BABE);
            assert_eq!(t.1, 0x42);
        }
        _ => panic!("expected Some"),
    }
}
