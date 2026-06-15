//! ECDH known-answer tests — round-trip per curve + bad-public.
//!
//! ring's `agree_ephemeral` consumes the private key by value, so we
//! generate two keypairs per round, exchange publics, and assert the
//! two derived secrets are equal. Negative cases feed garbage bytes
//! and check the `BadPublicKey` arm.

use grpcx_crypto::{Kx, KxError, P256, P384, X25519};

fn round_trip<K: Kx>() {
    let (alice, alice_pub) = K::keypair().expect("alice keypair");
    let (bob, bob_pub) = K::keypair().expect("bob keypair");
    assert_eq!(alice_pub.len(), K::PUBLIC_LEN);
    assert_eq!(bob_pub.len(), K::PUBLIC_LEN);

    let s1 = alice.agree(&bob_pub).expect("alice agree");
    let s2 = bob.agree(&alice_pub).expect("bob agree");
    assert_eq!(s1.len(), K::SECRET_LEN);
    assert_eq!(s2.len(), K::SECRET_LEN);
    assert_eq!(s1, s2, "DH secrets diverge");
}

fn bad_public_rejects<K: Kx>() {
    let (alice, _) = K::keypair().expect("keypair");
    let too_short = vec![0u8; K::PUBLIC_LEN - 1];
    assert_eq!(alice.agree(&too_short), Err(KxError::BadPublicKey));

    // Wrong-length public: also rejected at our length-check before
    // ring sees the bytes.
    let (alice, _) = K::keypair().expect("keypair");
    let too_long = vec![0u8; K::PUBLIC_LEN + 1];
    assert_eq!(alice.agree(&too_long), Err(KxError::BadPublicKey));
}

#[test]
fn x25519_round_trip() {
    round_trip::<X25519>();
}

#[test]
fn x25519_bad_public() {
    bad_public_rejects::<X25519>();
}

#[test]
fn p256_round_trip() {
    round_trip::<P256>();
}

#[test]
fn p256_bad_public() {
    bad_public_rejects::<P256>();
}

#[test]
fn p384_round_trip() {
    round_trip::<P384>();
}

#[test]
fn p384_bad_public() {
    bad_public_rejects::<P384>();
}

// public_key(&self) is cheap + idempotent: returns the same bytes
// before agree consumes the private side.
#[test]
fn x25519_public_key_matches_keypair_emit() {
    let (alice, emitted) = X25519::keypair().expect("keypair");
    let cached = alice.public_key();
    assert_eq!(cached, emitted);
    // Second call still identical.
    assert_eq!(alice.public_key(), emitted);
}
