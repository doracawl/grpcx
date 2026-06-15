//! SHA-256 / 384 / 512 known-answer tests.
//!
//! Vectors:
//! - FIPS 180-4 "abc" — the canonical small input ("abc" → fixed hex).
//! - The empty string — boundary case (`finalize` of a freshly-init'd state).

use grpcx_crypto::{Hash, Sha256, Sha384, Sha512};

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[test]
fn sha256_abc() {
    let mut h = Sha256::new();
    h.update(b"abc");
    let mut out = [0u8; 32];
    h.finalize(&mut out);
    // FIPS 180-4 § Appendix B.1.
    assert_eq!(
        hex(&out),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn sha256_empty() {
    let h = Sha256::new();
    let mut out = [0u8; 32];
    h.finalize(&mut out);
    assert_eq!(
        hex(&out),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn sha384_abc() {
    let mut h = Sha384::new();
    h.update(b"abc");
    let mut out = [0u8; 48];
    h.finalize(&mut out);
    // FIPS 180-4 § Appendix B.
    assert_eq!(
        hex(&out),
        "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
    );
}

#[test]
fn sha512_abc() {
    let mut h = Sha512::new();
    h.update(b"abc");
    let mut out = [0u8; 64];
    h.finalize(&mut out);
    // FIPS 180-4 § Appendix B.
    assert_eq!(
        hex(&out),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
}

#[test]
fn sha256_incremental_matches_oneshot() {
    let msg = b"the quick brown fox jumps over the lazy dog";
    let mut h1 = Sha256::new();
    h1.update(msg);
    let mut a = [0u8; 32];
    h1.finalize(&mut a);

    let mut h2 = Sha256::new();
    for chunk in msg.chunks(7) {
        h2.update(chunk);
    }
    let mut b = [0u8; 32];
    h2.finalize(&mut b);

    assert_eq!(a, b);
}
