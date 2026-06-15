//! L7 v0.2 — primitive codec known-answer tests.
//!
//! Locked wire vectors per `WIRE_FORMAT.md`. Any drift here is
//! a wire-breaking change.

use grpcx_idl::{Error, from_bytes, to_bytes};

// ============================================================
// Unsigned integers.
// ============================================================

#[test]
fn kat_u8() {
    for (value, wire) in [(0u8, &[0u8][..]), (1, &[1]), (127, &[127]), (255, &[0xff])] {
        let got = to_bytes(&value).unwrap();
        assert_eq!(got, wire);
        assert_eq!(*from_bytes::<u8>(wire).unwrap(), value);
    }
}

#[test]
fn kat_u16() {
    let cases: &[(u16, [u8; 2])] = &[
        (0, [0x00, 0x00]),
        (0x0102, [0x02, 0x01]),
        (0xFF00, [0x00, 0xFF]),
        (u16::MAX, [0xFF, 0xFF]),
    ];
    for (v, wire) in cases {
        assert_eq!(to_bytes(v).unwrap(), wire);
        assert_eq!(*from_bytes::<u16>(wire).unwrap(), *v);
    }
}

#[test]
fn kat_u32() {
    let cases: &[(u32, [u8; 4])] = &[
        (0, [0, 0, 0, 0]),
        (1, [1, 0, 0, 0]),
        (0xDEAD_BEEF, [0xEF, 0xBE, 0xAD, 0xDE]),
        (u32::MAX, [0xFF; 4]),
    ];
    for (v, wire) in cases {
        assert_eq!(to_bytes(v).unwrap(), wire);
        assert_eq!(*from_bytes::<u32>(wire).unwrap(), *v);
    }
}

#[test]
fn kat_u64() {
    let v: u64 = 0x0102_0304_0506_0708;
    let wire: [u8; 8] = [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01];
    assert_eq!(to_bytes(&v).unwrap(), wire);
    assert_eq!(*from_bytes::<u64>(&wire).unwrap(), v);
    assert_eq!(*from_bytes::<u64>(&[0; 8]).unwrap(), 0);
    assert_eq!(*from_bytes::<u64>(&[0xFF; 8]).unwrap(), u64::MAX);
}

#[test]
fn kat_u128() {
    let v: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
    let wire: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
        0x0F,
    ];
    assert_eq!(to_bytes(&v).unwrap(), wire);
    assert_eq!(*from_bytes::<u128>(&wire).unwrap(), v);
}

// ============================================================
// Signed integers.
// ============================================================

#[test]
fn kat_i8() {
    let cases: &[(i8, [u8; 1])] = &[
        (0, [0x00]),
        (1, [0x01]),
        (-1, [0xFF]),
        (-2, [0xFE]),
        (i8::MAX, [0x7F]),
        (i8::MIN, [0x80]),
    ];
    for (v, wire) in cases {
        assert_eq!(to_bytes(v).unwrap(), wire);
        assert_eq!(*from_bytes::<i8>(wire).unwrap(), *v);
    }
}

#[test]
fn kat_i16() {
    let cases: &[(i16, [u8; 2])] = &[
        (0, [0x00, 0x00]),
        (-2, [0xFE, 0xFF]),
        (i16::MAX, [0xFF, 0x7F]),
        (i16::MIN, [0x00, 0x80]),
    ];
    for (v, wire) in cases {
        assert_eq!(to_bytes(v).unwrap(), wire);
        assert_eq!(*from_bytes::<i16>(wire).unwrap(), *v);
    }
}

#[test]
fn kat_i32() {
    assert_eq!(to_bytes(&-1i32).unwrap(), [0xFF; 4]);
    assert_eq!(to_bytes(&i32::MIN).unwrap(), [0x00, 0x00, 0x00, 0x80]);
    assert_eq!(*from_bytes::<i32>(&[0xFF; 4]).unwrap(), -1);
}

#[test]
fn kat_i64() {
    let v: i64 = -2;
    let wire: [u8; 8] = [0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
    assert_eq!(to_bytes(&v).unwrap(), wire);
    assert_eq!(*from_bytes::<i64>(&wire).unwrap(), v);
}

#[test]
fn kat_i128() {
    assert_eq!(to_bytes(&-1i128).unwrap(), [0xFF; 16]);
    let mut min = [0u8; 16];
    min[15] = 0x80;
    assert_eq!(to_bytes(&i128::MIN).unwrap(), min);
}

// ============================================================
// Floats.
// ============================================================

#[test]
fn kat_f32() {
    let cases: &[(f32, [u8; 4])] = &[
        (0.0, [0x00, 0x00, 0x00, 0x00]),
        (1.0, [0x00, 0x00, 0x80, 0x3F]),
        (-1.0, [0x00, 0x00, 0x80, 0xBF]),
        (f32::INFINITY, [0x00, 0x00, 0x80, 0x7F]),
    ];
    for (v, wire) in cases {
        assert_eq!(to_bytes(v).unwrap(), wire);
        assert_eq!(*from_bytes::<f32>(wire).unwrap(), *v);
    }
    // NaN preserves its bit pattern through the wire.
    let nan_bytes = f32::NAN.to_le_bytes();
    let serialised = to_bytes(&f32::NAN).unwrap();
    assert_eq!(serialised, nan_bytes);
    assert!(from_bytes::<f32>(&serialised).unwrap().is_nan());
}

#[test]
fn kat_f64() {
    let cases: &[(f64, [u8; 8])] = &[
        (0.0, [0; 8]),
        (1.0, [0, 0, 0, 0, 0, 0, 0xF0, 0x3F]),
        (-2.0, [0, 0, 0, 0, 0, 0, 0x00, 0xC0]),
    ];
    for (v, wire) in cases {
        assert_eq!(to_bytes(v).unwrap(), wire);
        assert_eq!(*from_bytes::<f64>(wire).unwrap(), *v);
    }
}

// ============================================================
// Bool.
// ============================================================

#[test]
fn kat_bool() {
    assert_eq!(to_bytes(&true).unwrap(), [0x01]);
    assert_eq!(to_bytes(&false).unwrap(), [0x00]);
    assert!(*from_bytes::<bool>(&[0x01]).unwrap());
    assert!(!*from_bytes::<bool>(&[0x00]).unwrap());
}

#[test]
fn bool_rejects_non_canonical_bytes() {
    assert_eq!(from_bytes::<bool>(&[2]), Err(Error::BadLayout));
    assert_eq!(from_bytes::<bool>(&[0x80]), Err(Error::BadLayout));
    assert_eq!(from_bytes::<bool>(&[0xFF]), Err(Error::BadLayout));
}

// ============================================================
// Length validation.
// ============================================================

#[test]
fn truncated_inputs() {
    assert_eq!(from_bytes::<u16>(&[0]), Err(Error::Truncated));
    assert_eq!(from_bytes::<u32>(&[0; 3]), Err(Error::Truncated));
    assert_eq!(from_bytes::<u64>(&[0; 7]), Err(Error::Truncated));
    assert_eq!(from_bytes::<u128>(&[0; 15]), Err(Error::Truncated));
    assert_eq!(from_bytes::<i32>(&[0; 3]), Err(Error::Truncated));
    assert_eq!(from_bytes::<f32>(&[0; 3]), Err(Error::Truncated));
    assert_eq!(from_bytes::<f64>(&[0; 7]), Err(Error::Truncated));
    assert_eq!(from_bytes::<bool>(&[]), Err(Error::Truncated));
}

#[test]
fn oversize_inputs() {
    assert_eq!(from_bytes::<u8>(&[0, 0]), Err(Error::BadLayout));
    assert_eq!(from_bytes::<u32>(&[0; 5]), Err(Error::BadLayout));
    assert_eq!(from_bytes::<bool>(&[0, 0]), Err(Error::BadLayout));
}

#[test]
fn round_trip_smoke_every_primitive() {
    // 13 primitives × 1 sample value each — keeps the KAT
    // matrix from rotting if anyone reshuffles the macro.
    macro_rules! check {
        ($t:ty, $v:expr) => {{
            let v: $t = $v;
            let b = to_bytes(&v).unwrap();
            assert_eq!(*from_bytes::<$t>(&b).unwrap(), v);
        }};
    }
    check!(u8, 7);
    check!(u16, 0xC0DE);
    check!(u32, 0xCAFE_F00D);
    check!(u64, 0x0123_4567_89AB_CDEF);
    check!(u128, (0xFEED_FACE_C0DE_0BADu128) << 64);
    check!(i8, -42);
    check!(i16, -12345);
    check!(i32, -1_000_000);
    check!(i64, -10_000_000_000);
    check!(i128, -(1i128 << 100));
    check!(f32, core::f32::consts::PI);
    check!(f64, core::f64::consts::E);
    check!(bool, true);
}
