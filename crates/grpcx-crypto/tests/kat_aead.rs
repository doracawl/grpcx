//! AEAD known-answer tests.
//!
//! - AES-128-GCM: NIST GCM test vector (one round-trip + tamper check).
//! - AES-256-GCM: NIST GCM test vector.
//! - ChaCha20-Poly1305: RFC 8439 § Appendix A.5 vector.
//!
//! v0.2 ships one canonical vector per suite; the full KAT sweep
//! lands at v0.6.

use grpcx_crypto::{Aead, AeadError, Aes128Gcm, Aes256Gcm, ChaCha20Poly1305};

fn from_hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// Round-trip: encrypt → decrypt yields original plaintext.
fn round_trip<A: Aead>(key: &[u8], nonce: &[u8], aad: &[u8], plaintext: &[u8]) {
    let aead = A::new(key).expect("new");

    // Encrypt: reserve TAG_LEN trailing scratch.
    let mut buf = Vec::with_capacity(plaintext.len() + A::TAG_LEN);
    buf.extend_from_slice(plaintext);
    buf.extend_from_slice(&vec![0u8; A::TAG_LEN]);
    let written = aead
        .seal_in_place(nonce, aad, &mut buf, plaintext.len())
        .expect("seal");
    assert_eq!(written, plaintext.len() + A::TAG_LEN);

    // Decrypt: in-place strip + verify.
    let pt_len = aead.open_in_place(nonce, aad, &mut buf).expect("open");
    assert_eq!(pt_len, plaintext.len());
    assert_eq!(&buf[..pt_len], plaintext);
}

// Tamper a byte of ciphertext → BadTag.
fn tamper_rejects<A: Aead>(key: &[u8], nonce: &[u8], aad: &[u8], plaintext: &[u8]) {
    let aead = A::new(key).expect("new");
    let mut buf = Vec::with_capacity(plaintext.len() + A::TAG_LEN);
    buf.extend_from_slice(plaintext);
    buf.extend_from_slice(&vec![0u8; A::TAG_LEN]);
    aead.seal_in_place(nonce, aad, &mut buf, plaintext.len())
        .expect("seal");

    if !buf.is_empty() {
        buf[0] ^= 0x01;
    }
    assert_eq!(
        aead.open_in_place(nonce, aad, &mut buf),
        Err(AeadError::BadTag)
    );
}

// Bad key length → BadLength on construction.
#[test]
fn aes128gcm_bad_key_len() {
    let r = Aes128Gcm::new(&[0u8; 8]);
    assert_eq!(r.err(), Some(AeadError::BadLength));
}

// AES-128-GCM round-trip + tamper.
#[test]
fn aes128gcm_round_trip_and_tamper() {
    let key = vec![0x42u8; 16];
    let nonce = vec![0x12u8; 12];
    let aad = b"associated-data";
    let plaintext = b"the quick brown fox jumps over the lazy dog";
    round_trip::<Aes128Gcm>(&key, &nonce, aad, plaintext);
    tamper_rejects::<Aes128Gcm>(&key, &nonce, aad, plaintext);
}

// AES-128-GCM NIST CAVP vector — gcmEncryptExtIV128.rsp Count = 0.
// Key=11754cd72aec309bf52f7687212e8957  IV=3c819d9a9bed087615030b65
// PT="" (empty) AAD="" CT="" Tag=250327c674aaf477aef2675748cf6971
#[test]
fn aes128gcm_nist_empty_pt() {
    let key = from_hex("11754cd72aec309bf52f7687212e8957");
    let nonce = from_hex("3c819d9a9bed087615030b65");
    let aead = Aes128Gcm::new(&key).expect("new");
    let mut buf = vec![0u8; 16];
    let n = aead.seal_in_place(&nonce, &[], &mut buf, 0).expect("seal");
    assert_eq!(n, 16);
    assert_eq!(hex(&buf), "250327c674aaf477aef2675748cf6971");
}

// AES-256-GCM round-trip + tamper.
#[test]
fn aes256gcm_round_trip_and_tamper() {
    let key = vec![0x42u8; 32];
    let nonce = vec![0x12u8; 12];
    let aad = b"associated-data";
    let plaintext = b"the quick brown fox";
    round_trip::<Aes256Gcm>(&key, &nonce, aad, plaintext);
    tamper_rejects::<Aes256Gcm>(&key, &nonce, aad, plaintext);
}

// AES-256-GCM NIST CAVP vector — gcmEncryptExtIV256.rsp Count = 0.
// Key=b52c505a37d78eda5dd34f20c22540ea1b58963cf8e5bf8ffa85f9f2492505b4
// IV=516c33929df5a3284ff463d7
// PT="" AAD="" Tag=bdc1ac884d332457a1d2664f168c76f0
#[test]
fn aes256gcm_nist_empty_pt() {
    let key = from_hex("b52c505a37d78eda5dd34f20c22540ea1b58963cf8e5bf8ffa85f9f2492505b4");
    let nonce = from_hex("516c33929df5a3284ff463d7");
    let aead = Aes256Gcm::new(&key).expect("new");
    let mut buf = vec![0u8; 16];
    let n = aead.seal_in_place(&nonce, &[], &mut buf, 0).expect("seal");
    assert_eq!(n, 16);
    assert_eq!(hex(&buf), "bdc1ac884d332457a1d2664f168c76f0");
}

// ChaCha20-Poly1305 round-trip + tamper.
#[test]
fn chacha20poly1305_round_trip_and_tamper() {
    let key = vec![0x42u8; 32];
    let nonce = vec![0x12u8; 12];
    let aad = b"associated-data";
    let plaintext = b"ladies and gentlemen of the class of '99";
    round_trip::<ChaCha20Poly1305>(&key, &nonce, aad, plaintext);
    tamper_rejects::<ChaCha20Poly1305>(&key, &nonce, aad, plaintext);
}

// ChaCha20-Poly1305 RFC 8439 § 2.8.2 — IETF AEAD test vector.
//
// Plaintext (114 bytes): the "Ladies and Gentlemen..." string.
// Nonce = constant (07 00 00 00) || IV (40 41 ... 47).
#[test]
fn chacha20poly1305_rfc8439_2_8_2() {
    let key = from_hex(
        "808182838485868788898a8b8c8d8e8f\
         909192939495969798999a9b9c9d9e9f",
    );
    let nonce = from_hex("070000004041424344454647");
    let aad = from_hex("50515253c0c1c2c3c4c5c6c7");
    let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    assert_eq!(plaintext.len(), 114);
    let expected_ct = from_hex(
        "d31a8d34648e60db7b86afbc53ef7ec2\
         a4aded51296e08fea9e2b5a736ee62d6\
         3dbea45e8ca9671282fafb69da92728b\
         1a71de0a9e060b2905d6a5b67ecd3b36\
         92ddbd7f2d778b8c9803aee328091b58\
         fab324e4fad675945585808b4831d7bc\
         3ff4def08e4b7a9de576d26586cec64b\
         6116",
    );
    let expected_tag = from_hex("1ae10b594f09e26a7e902ecbd0600691");

    let aead = ChaCha20Poly1305::new(&key).expect("new");
    let mut buf = Vec::with_capacity(plaintext.len() + 16);
    buf.extend_from_slice(plaintext);
    buf.extend_from_slice(&[0u8; 16]);
    let n = aead
        .seal_in_place(&nonce, &aad, &mut buf, plaintext.len())
        .expect("seal");
    assert_eq!(n, plaintext.len() + 16);
    assert_eq!(&buf[..plaintext.len()], &expected_ct[..]);
    assert_eq!(&buf[plaintext.len()..n], &expected_tag[..]);

    // Decrypt + verify.
    let pt_len = aead.open_in_place(&nonce, &aad, &mut buf).expect("open");
    assert_eq!(&buf[..pt_len], plaintext);
}
