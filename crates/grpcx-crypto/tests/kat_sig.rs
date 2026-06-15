//! Signature known-answer tests.
//!
//! - Ed25519: RFC 8032 §7.1 Test 1 (deterministic signature over the
//!   empty message). Plus a tamper check.
//! - ECDSA-P-256: round-trip generate-sign-verify (ECDSA is
//!   non-deterministic by design; sign + verify against a freshly-
//!   generated keypair + tamper check is the v0.3 KAT).

use grpcx_crypto::{EcdsaP256, Ed25519, Sig, SigError};

fn from_hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

// RFC 8032 §7.1 Test 1 (SECRET KEY / PUBLIC KEY / MESSAGE / SIGNATURE).
// SECRET = 9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60
// PUBLIC = d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
// MESSAGE = "" (empty)
// SIGNATURE = e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e0650
//             3540f2925ad26ee2adb1ff6cdd9c54260bb7c30dabe2cd5cca1c2050a04d05
//             (split for readability)
#[test]
fn ed25519_rfc8032_test1() {
    let seed = from_hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
    let public = from_hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
    let expected_sig = from_hex(
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065\
         22490155 5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    );

    let sig = Ed25519::sign(&seed, b"").expect("sign");
    assert_eq!(sig.len(), 64);
    assert_eq!(sig, expected_sig);

    Ed25519::verify(&public, b"", &sig).expect("verify");
}

#[test]
fn ed25519_tamper_rejected() {
    let seed = from_hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
    let public = from_hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
    let mut sig = Ed25519::sign(&seed, b"hello").expect("sign");
    sig[0] ^= 0x01;
    assert_eq!(
        Ed25519::verify(&public, b"hello", &sig),
        Err(SigError::BadSignature)
    );
}

#[test]
fn ed25519_bad_seed_len() {
    assert_eq!(
        Ed25519::sign(&[0u8; 16], b"x").err(),
        Some(SigError::BadKey)
    );
}

#[test]
fn ed25519_bad_public_len() {
    let seed = from_hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
    let sig = Ed25519::sign(&seed, b"x").expect("sign");
    assert_eq!(
        Ed25519::verify(&[0u8; 16], b"x", &sig).err(),
        Some(SigError::BadKey)
    );
}

// ECDSA P-256 round-trip via ring's pkcs8 keypair gen.
#[test]
fn ecdsa_p256_round_trip() {
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
    let rng = SystemRandom::new();
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).expect("gen pkcs8");
    let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
        .expect("from pkcs8");
    let public = kp.public_key().as_ref().to_vec();

    let msg = b"the quick brown fox jumps over the lazy dog";
    let sig = EcdsaP256::sign(pkcs8.as_ref(), msg).expect("sign");
    assert_eq!(sig.len(), 64, "ECDSA P256 FIXED sig is 64 bytes");
    EcdsaP256::verify(&public, msg, &sig).expect("verify");
}

#[test]
fn ecdsa_p256_tamper_rejected() {
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
    let rng = SystemRandom::new();
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).expect("gen pkcs8");
    let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
        .expect("from pkcs8");
    let public = kp.public_key().as_ref().to_vec();

    let msg = b"hello world";
    let mut sig = EcdsaP256::sign(pkcs8.as_ref(), msg).expect("sign");
    sig[0] ^= 0x01;
    assert_eq!(
        EcdsaP256::verify(&public, msg, &sig),
        Err(SigError::BadSignature)
    );
}

#[test]
fn ecdsa_p256_bad_key() {
    let r = EcdsaP256::sign(&[0u8; 8], b"x");
    assert_eq!(r.err(), Some(SigError::BadKey));
}
