//! KAT for `#[derive(GrpcxIdl)] enum …` round-trips +
//! rejects.

use grpcx_idl::{Error, from_bytes, to_bytes};
use grpcx_idl_derive::GrpcxIdl;

#[derive(GrpcxIdl)]
enum Status {
    Idle,
    Running(u32),
    Done,
}

#[derive(GrpcxIdl)]
enum Wrapped {
    Empty,
    Pair((u32, u8)),
}

// ============================================================
// Round-trips.
// ============================================================

#[test]
fn status_idle_round_trip() {
    let s = Status::Idle;
    let bytes = to_bytes(&s).unwrap();
    // Disc 0; total size = max(disc-only=4, disc+u32=8) → 8.
    assert_eq!(bytes.len(), 8);
    assert_eq!(&bytes[..4], &[0, 0, 0, 0]);
    let archived = from_bytes::<Status>(&bytes).unwrap();
    assert!(matches!(archived, ArchivedStatus::Idle));
}

#[test]
fn status_running_round_trip() {
    let s = Status::Running(0xDEAD_BEEF);
    let bytes = to_bytes(&s).unwrap();
    assert_eq!(bytes.len(), 8);
    assert_eq!(&bytes[..4], &[1, 0, 0, 0]);
    assert_eq!(&bytes[4..], &0xDEAD_BEEFu32.to_le_bytes());
    let archived = from_bytes::<Status>(&bytes).unwrap();
    match archived {
        ArchivedStatus::Running(v) => assert_eq!(*v, 0xDEAD_BEEF),
        _ => panic!("expected Running"),
    }
}

#[test]
fn status_done_round_trip() {
    let s = Status::Done;
    let bytes = to_bytes(&s).unwrap();
    assert_eq!(bytes.len(), 8);
    assert_eq!(&bytes[..4], &[2, 0, 0, 0]);
    let archived = from_bytes::<Status>(&bytes).unwrap();
    assert!(matches!(archived, ArchivedStatus::Done));
}

#[test]
fn wrapped_pair_round_trip() {
    let w = Wrapped::Pair((0xCAFE_BABE, 0x42));
    let bytes = to_bytes(&w).unwrap();
    let archived = from_bytes::<Wrapped>(&bytes).unwrap();
    match archived {
        ArchivedWrapped::Pair(t) => {
            assert_eq!(t.0, 0xCAFE_BABE);
            assert_eq!(t.1, 0x42);
        }
        _ => panic!("expected Pair"),
    }
}

#[test]
fn wrapped_empty_round_trip() {
    let w = Wrapped::Empty;
    let bytes = to_bytes(&w).unwrap();
    let archived = from_bytes::<Wrapped>(&bytes).unwrap();
    assert!(matches!(archived, ArchivedWrapped::Empty));
}

// ============================================================
// Reject paths.
// ============================================================

#[test]
fn status_bad_disc_rejected() {
    let bytes: Vec<u8> = vec![3, 0, 0, 0, 0, 0, 0, 0]; // disc=3 invalid
    assert!(matches!(
        from_bytes::<Status>(&bytes).err(),
        Some(Error::BadLayout)
    ));
}

#[test]
fn status_truncated_rejected() {
    let bytes: Vec<u8> = vec![0u8; 7];
    assert!(matches!(
        from_bytes::<Status>(&bytes).err(),
        Some(Error::Truncated)
    ));
}
