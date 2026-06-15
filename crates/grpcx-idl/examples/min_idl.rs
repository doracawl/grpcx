//! Minimal IDL round-trip for BINSIZE measurement.
//!
//! Exercises the major wire shapes: primitive, composite,
//! varlen, tagged union, derived struct.
//! `cargo build --release --example min_idl` produces the
//! binary whose stripped size fills `BUDGETS.md` BINSIZE.

use grpcx_idl::{from_bytes, to_bytes};
use grpcx_idl_derive::GrpcxIdl;

#[derive(GrpcxIdl)]
struct Record {
    id: u64,
    flag: bool,
    score: u32,
}

fn main() {
    // Primitive.
    let n = 0xDEAD_BEEFu32;
    let bytes = to_bytes(&n).unwrap();
    assert_eq!(*from_bytes::<u32>(&bytes).unwrap(), n);

    // Composite (tuple).
    let t = (0x11u8, 0xCAFEu16, 0xDEAD_BEEFu32);
    let bytes = to_bytes(&t).unwrap();
    let arch = from_bytes::<(u8, u16, u32)>(&bytes).unwrap();
    assert_eq!((arch.0, arch.1, arch.2), t);

    // Varlen.
    let s = String::from("hello grpcx-idl");
    let bytes = to_bytes(&s).unwrap();
    let arch = from_bytes::<String>(&bytes).unwrap();
    assert_eq!(arch.as_str().unwrap(), "hello grpcx-idl");

    // Tagged union.
    let opt: Option<u64> = Some(0xCAFE_BABE_0000_0001);
    let bytes = to_bytes(&opt).unwrap();
    let _ = from_bytes::<Option<u64>>(&bytes).unwrap();

    // Derived struct.
    let r = Record {
        id: 0xDEAD_BEEF_CAFE_BABE,
        flag: true,
        score: 9001,
    };
    let bytes = to_bytes(&r).unwrap();
    let arch = from_bytes::<Record>(&bytes).unwrap();
    assert_eq!(arch.id, r.id);
    assert!(arch.flag);
    assert_eq!(arch.score, 9001);

    println!("min_idl ok ({} bytes for the Record archive)", bytes.len());
}
