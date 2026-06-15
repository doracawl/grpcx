//! Cover the Deserialize path on every shape — round-trip via
//! both the zero-copy `from_bytes -> &Archived` AND the owned
//! `Deserialize::deserialize -> T`.

use grpcx_idl::{
    ArchivedTuple4, ArchivedTuple5, ArchivedTuple6, ArchivedTuple7, Deserialize, from_bytes,
    to_bytes,
};

fn rt<T>(v: T) -> T
where
    T: PartialEq + core::fmt::Debug + grpcx_idl::Serialize<Vec<u8>> + Deserialize<()>,
{
    let bytes = to_bytes(&v).unwrap();
    let arch = <T as grpcx_idl::Archive>::validate(&bytes).unwrap();
    <T as Deserialize<()>>::deserialize(arch, &mut ()).unwrap()
}

#[test]
fn primitives_deserialize_round_trip() {
    assert_eq!(rt(42u8), 42);
    assert_eq!(rt(0xDEAD_BEEFu32), 0xDEAD_BEEF);
    assert_eq!(rt(-2i16), -2);
    assert!(rt(true));
    assert!(!rt(false));
}

#[test]
fn tuple_deserialize_round_trip() {
    assert_eq!(rt((0xAB_u8, 0xCD_u8)), (0xAB, 0xCD));
    assert_eq!(rt((1u32, 2u8, 3u16)), (1, 2, 3));
    assert_eq!(rt((1u8, 2u16, 3u32, 4u64)), (1, 2, 3, 4),);
}

#[test]
fn tuple_arities_4_through_7_compile_and_round_trip() {
    // Touch tuple impls 4-7 so their codegen is exercised.
    let t4: (u8, u8, u8, u8) = (1, 2, 3, 4);
    let bytes = to_bytes(&t4).unwrap();
    let arch: &ArchivedTuple4<u8, u8, u8, u8> = from_bytes::<(u8, u8, u8, u8)>(&bytes).unwrap();
    assert_eq!((arch.0, arch.1, arch.2, arch.3), (1, 2, 3, 4));

    let t5: (u8, u8, u8, u8, u8) = (1, 2, 3, 4, 5);
    let bytes = to_bytes(&t5).unwrap();
    let arch: &ArchivedTuple5<u8, u8, u8, u8, u8> =
        from_bytes::<(u8, u8, u8, u8, u8)>(&bytes).unwrap();
    assert_eq!(arch.4, 5);

    let t6: (u8, u8, u8, u8, u8, u8) = (1, 2, 3, 4, 5, 6);
    let bytes = to_bytes(&t6).unwrap();
    let arch: &ArchivedTuple6<u8, u8, u8, u8, u8, u8> =
        from_bytes::<(u8, u8, u8, u8, u8, u8)>(&bytes).unwrap();
    assert_eq!(arch.5, 6);

    let t7: (u8, u8, u8, u8, u8, u8, u8) = (1, 2, 3, 4, 5, 6, 7);
    let bytes = to_bytes(&t7).unwrap();
    let arch: &ArchivedTuple7<u8, u8, u8, u8, u8, u8, u8> =
        from_bytes::<(u8, u8, u8, u8, u8, u8, u8)>(&bytes).unwrap();
    assert_eq!(arch.6, 7);
}

#[test]
fn array_deserialize_round_trip() {
    assert_eq!(rt([1u32, 2, 3]), [1, 2, 3]);
    assert_eq!(rt([true, false, true, false]), [true, false, true, false]);
}

#[test]
fn varlen_deserialize_round_trip() {
    assert_eq!(rt(vec![0xAB_u8, 0xCD, 0xEF]), vec![0xAB, 0xCD, 0xEF]);
    assert_eq!(rt(String::from("hello")), String::from("hello"));
    assert_eq!(rt(String::new()), String::new());
    assert_eq!(rt(Vec::<u8>::new()), Vec::<u8>::new());
}

#[test]
fn option_deserialize_round_trip() {
    assert_eq!(rt(None::<u32>), None);
    assert_eq!(rt(Some(42_u32)), Some(42));
    assert_eq!(rt(Some(true)), Some(true));
    assert_eq!(
        rt(Some(0xCAFE_BABE_DEAD_BEEFu64)),
        Some(0xCAFE_BABE_DEAD_BEEF)
    );
}

#[test]
fn result_deserialize_round_trip() {
    let ok: Result<u32, u32> = Ok(42);
    assert_eq!(rt(ok), Ok(42));
    let err: Result<u32, u32> = Err(99);
    assert_eq!(rt(err), Err(99));
    let asymm_ok: Result<u8, u64> = Ok(0x7F);
    assert_eq!(rt(asymm_ok), Ok(0x7F));
    let asymm_err: Result<u8, u64> = Err(0xDEAD_BEEF);
    assert_eq!(rt(asymm_err), Err(0xDEAD_BEEF));
}

#[test]
fn archived_string_accessors() {
    let s = String::from("hello");
    let bytes = to_bytes(&s).unwrap();
    let arch = from_bytes::<String>(&bytes).unwrap();
    assert_eq!(arch.len(), 5);
    assert!(!arch.is_empty());

    let empty = String::new();
    let bytes = to_bytes(&empty).unwrap();
    let arch = from_bytes::<String>(&bytes).unwrap();
    assert!(arch.is_empty());
}

#[test]
fn archived_bytes_is_empty_accessor() {
    let v: Vec<u8> = Vec::new();
    let bytes = to_bytes(&v).unwrap();
    let arch = from_bytes::<Vec<u8>>(&bytes).unwrap();
    assert!(arch.is_empty());
}
