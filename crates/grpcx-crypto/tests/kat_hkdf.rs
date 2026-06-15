//! HKDF known-answer tests — RFC 5869 Appendix A.

use grpcx_crypto::{Hkdf, HkdfError, Sha256, Sha384, Sha512};

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn from_hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

// RFC 5869 A.1 — basic test case with SHA-256.
#[test]
fn rfc5869_a1_sha256() {
    let ikm = from_hex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
    let salt = from_hex("000102030405060708090a0b0c");
    let info = from_hex("f0f1f2f3f4f5f6f7f8f9");

    let hkdf = Hkdf::<Sha256>::extract(&salt, &ikm);
    let mut okm = [0u8; 42];
    hkdf.expand(&info, &mut okm).expect("expand");
    assert_eq!(
        hex(&okm),
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
    );
}

// RFC 5869 A.2 — longer inputs/outputs with SHA-256.
#[test]
fn rfc5869_a2_sha256() {
    let ikm = from_hex(
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
         202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f\
         404142434445464748494a4b4c4d4e4f",
    );
    let salt = from_hex(
        "606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f\
         808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f\
         a0a1a2a3a4a5a6a7a8a9aaabacadaeaf",
    );
    let info = from_hex(
        "b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecf\
         d0d1d2d3d4d5d6d7d8d9dadbdcdddedfe0e1e2e3e4e5e6e7e8e9eaebecedeeef\
         f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
    );

    let hkdf = Hkdf::<Sha256>::extract(&salt, &ikm);
    let mut okm = [0u8; 82];
    hkdf.expand(&info, &mut okm).expect("expand");
    assert_eq!(
        hex(&okm),
        "b11e398dc80327a1c8e7f78c596a49344f012eda2d4efad8a050cc4c19afa97c\
         59045a99cac7827271cb41c65e590e09da3275600c2f09b8367793a9aca3db71\
         cc30c58179ec3e87c14c01d5c1f3434f1d87"
    );
}

// RFC 5869 A.3 — zero-length salt + info.
#[test]
fn rfc5869_a3_sha256() {
    let ikm = from_hex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
    let hkdf = Hkdf::<Sha256>::extract(&[], &ikm);
    let mut okm = [0u8; 42];
    hkdf.expand(&[], &mut okm).expect("expand");
    assert_eq!(
        hex(&okm),
        "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8"
    );
}

// Cross-hash dispatch — confirms `hkdf_alg<H>` reaches the Sha384 /
// Sha512 arms. Smoke round-trip (extract then expand to a fixed
// length); the output bytes aren't checked against a vector here.
#[test]
fn hkdf_sha384_round_trip() {
    let h = Hkdf::<Sha384>::extract(&[1; 8], &[2; 32]);
    let mut out = [0u8; 48];
    h.expand(b"info", &mut out).expect("expand");
    assert!(out.iter().any(|&b| b != 0));
}

#[test]
fn hkdf_sha512_round_trip() {
    let h = Hkdf::<Sha512>::extract(&[1; 8], &[2; 32]);
    let mut out = [0u8; 64];
    h.expand(b"info", &mut out).expect("expand");
    assert!(out.iter().any(|&b| b != 0));
}

#[test]
fn expand_label_rejects_overlong_output() {
    let h = Hkdf::<Sha256>::extract(&[], &[1u8; 32]);
    let mut out = vec![0u8; (u16::MAX as usize) + 1];
    assert_eq!(
        h.expand_label(b"x", &[], &mut out),
        Err(HkdfError::OutputTooLong)
    );
}

// HKDF-Expand-Label sanity — RFC 8446 §7.1 wire-format.
//
// Manually-constructed expected OKM by computing what HKDF-Expand
// would do given our explicit info string. Cross-checks the
// `expand_label` info-encoding against ring's `expand`.
#[test]
fn expand_label_matches_hand_built_info() {
    let prk_ikm = from_hex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
    let salt = from_hex("000102030405060708090a0b0c");

    // 1) expand_label
    let hkdf = Hkdf::<Sha256>::extract(&salt, &prk_ikm);
    let mut okm_label = [0u8; 32];
    hkdf.expand_label(b"key", b"context-bytes", &mut okm_label)
        .expect("expand_label");

    // 2) hand-built info: u16 len || u8 (6 + label.len()) || "tls13 " || label
    //                   || u8 context.len() || context.
    let mut info: Vec<u8> = Vec::new();
    info.extend_from_slice(&32u16.to_be_bytes());
    info.push((6 + 3) as u8);
    info.extend_from_slice(b"tls13 ");
    info.extend_from_slice(b"key");
    info.push(13);
    info.extend_from_slice(b"context-bytes");

    let hkdf2 = Hkdf::<Sha256>::extract(&salt, &prk_ikm);
    let mut okm_hand = [0u8; 32];
    hkdf2.expand(&info, &mut okm_hand).expect("expand");

    assert_eq!(okm_label, okm_hand);
}
