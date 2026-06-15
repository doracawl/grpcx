//! QUIC v1 key-derivation known-answer tests — RFC 9001 Appendix A.1.
//!
//! Verifies the initial-secret + per-direction (key, IV, HP) chain
//! against the exact byte values quoted in the RFC for the
//! reference connection ID `0x8394c8f03e515708`.

use grpcx_crypto::{Hkdf, Sha256, quic};

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn from_hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

// RFC 9001 § Appendix A reference connection ID.
const CID: [u8; 8] = [0x83, 0x94, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];

// RFC 9001 § Appendix A.1 reference outputs.
const INITIAL_SECRET: &str = "7db5df06e7a69e432496adedb0085192\
     3595221596ae2ae9fb8115c1e9ed0a44";
const CLIENT_INITIAL_SECRET: &str = "c00cf151ca5be075ed0ebfb5c80323c4\
     2d6b7db67881289af4008f1f6c357aea";
const CLIENT_KEY: &str = "1f369613dd76d5467730efcbe3b1a22d";
const CLIENT_IV: &str = "fa044b2f42a3fd3b46fb255c";
const CLIENT_HP: &str = "9f50449e04a0e810283a1e9933adedd2";
const SERVER_INITIAL_SECRET: &str = "3c199828fd139efd216c155ad844cc81\
     fb82fa8d7446fa7d78be803acdda951b";
const SERVER_KEY: &str = "cf3a5331653c364c88f0f379b6067e37";
const SERVER_IV: &str = "0ac1493ca1905853b0bba03e";
const SERVER_HP: &str = "c206b8d9b9f0f37644430b490eeaa314";

#[test]
fn rfc9001_a1_initial_secret() {
    let got = quic::initial_secret(&CID);
    assert_eq!(hex(&got), INITIAL_SECRET);
}

#[test]
fn rfc9001_a1_client_initial_secret() {
    let prk = quic::initial_secret(&CID);
    let hkdf = Hkdf::<Sha256>::from_prk(&prk);
    let mut out = [0u8; 32];
    hkdf.expand_label(b"client in", &[], &mut out)
        .expect("expand client in");
    assert_eq!(hex(&out), CLIENT_INITIAL_SECRET);
}

#[test]
fn rfc9001_a1_server_initial_secret() {
    let prk = quic::initial_secret(&CID);
    let hkdf = Hkdf::<Sha256>::from_prk(&prk);
    let mut out = [0u8; 32];
    hkdf.expand_label(b"server in", &[], &mut out)
        .expect("expand server in");
    assert_eq!(hex(&out), SERVER_INITIAL_SECRET);
}

#[test]
fn rfc9001_a1_client_packet_keys() {
    let client_secret = from_hex(CLIENT_INITIAL_SECRET);
    let keys = quic::key_iv_hp(&client_secret, 16, 12, 16).expect("key_iv_hp");
    assert_eq!(hex(&keys.key), CLIENT_KEY);
    assert_eq!(hex(&keys.iv), CLIENT_IV);
    assert_eq!(hex(&keys.hp), CLIENT_HP);
}

#[test]
fn rfc9001_a1_server_packet_keys() {
    let server_secret = from_hex(SERVER_INITIAL_SECRET);
    let keys = quic::key_iv_hp(&server_secret, 16, 12, 16).expect("key_iv_hp");
    assert_eq!(hex(&keys.key), SERVER_KEY);
    assert_eq!(hex(&keys.iv), SERVER_IV);
    assert_eq!(hex(&keys.hp), SERVER_HP);
}

#[test]
fn hp_secret_matches_key_iv_hp_hp_field() {
    let client_secret = from_hex(CLIENT_INITIAL_SECRET);
    let solo = quic::hp_secret(&client_secret, 16).expect("hp_secret");
    let keys = quic::key_iv_hp(&client_secret, 16, 12, 16).expect("key_iv_hp");
    assert_eq!(solo, keys.hp);
}
