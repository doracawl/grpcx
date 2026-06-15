//! QUIC packet protection KAT — AEAD seal/open + header protection
//! round-trip across all three suites, plus RFC 9001 §A.2
//! client-side partial vectors.

use grpcx_quic::{
    AeadSuite, apply_header_protection, protect_payload, remove_header_protection,
    unprotect_payload,
};

fn from_hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn round_trip_payload(suite: AeadSuite, key_len: usize) {
    let key = vec![0x42u8; key_len];
    let iv = vec![0x11u8; 12];
    let aad = b"unprotected-header-bytes";
    let plaintext = b"a payload of frames containing CRYPTO / STREAM / ACK ...";
    let pn = 0x1234_5678_9abc_def0u64;
    let tag_len = suite.tag_len();
    let mut buf = Vec::with_capacity(plaintext.len() + tag_len);
    buf.extend_from_slice(plaintext);
    buf.extend_from_slice(&vec![0u8; tag_len]);

    let n = protect_payload(suite, &key, &iv, pn, aad, &mut buf, plaintext.len()).expect("seal");
    assert_eq!(n, plaintext.len() + tag_len);

    let pt_len = unprotect_payload(suite, &key, &iv, pn, aad, &mut buf).expect("open");
    assert_eq!(pt_len, plaintext.len());
    assert_eq!(&buf[..pt_len], plaintext);
}

#[test]
fn aes128gcm_payload_round_trip() {
    round_trip_payload(AeadSuite::Aes128Gcm, 16);
}

#[test]
fn aes256gcm_payload_round_trip() {
    round_trip_payload(AeadSuite::Aes256Gcm, 32);
}

#[test]
fn chacha20poly1305_payload_round_trip() {
    round_trip_payload(AeadSuite::ChaCha20Poly1305, 32);
}

#[test]
fn header_protection_round_trip_long() {
    // Construct a packet shape long enough to host a 16-byte
    // sample at pn_offset + 4. The exact bytes past PN don't
    // matter for the round trip — they get sampled, masked, and
    // unmasked symmetrically.
    let suite = AeadSuite::Aes128Gcm;
    let hp_key = vec![0x33u8; 16];
    let pn_offset = 10;
    let pn_len = 4;
    let mut packet = vec![0u8; 60];
    packet[0] = 0xc3; // Long header, fixed bit set, Initial, pn_len=4
    let original = packet.clone();

    apply_header_protection(suite, &hp_key, &mut packet, pn_offset, pn_len, true).expect("apply");

    // First byte and PN bytes are masked; everything else
    // unchanged.
    assert_ne!(packet[0], original[0]);
    assert!(packet[pn_offset..pn_offset + pn_len] != original[pn_offset..pn_offset + pn_len]);

    let recovered_pn_len =
        remove_header_protection(suite, &hp_key, &mut packet, pn_offset, true).expect("remove");
    assert_eq!(recovered_pn_len, pn_len as u8);
    assert_eq!(packet, original);
}

#[test]
fn header_protection_round_trip_short() {
    let suite = AeadSuite::ChaCha20Poly1305;
    let hp_key = vec![0x77u8; 32];
    let pn_offset = 5;
    let pn_len = 2;
    let mut packet = vec![0u8; 40];
    packet[0] = 0x41; // Short header, fixed bit set, pn_len=2
    let original = packet.clone();

    apply_header_protection(suite, &hp_key, &mut packet, pn_offset, pn_len, false).expect("apply");
    let recovered_pn_len =
        remove_header_protection(suite, &hp_key, &mut packet, pn_offset, false).expect("remove");
    assert_eq!(recovered_pn_len, pn_len as u8);
    assert_eq!(packet, original);
}

// RFC 9001 § Appendix A.2 client-side partial KAT.
//
// The reference connection ID is `0x8394c8f03e515708`. The RFC
// derives:
//   - initial_secret
//   - client_initial_secret
//   - client packet key  = 1f369613dd76d5467730efcbe3b1a22d
//   - client packet IV   = fa044b2f42a3fd3b46fb255c
//   - client HP key      = 9f50449e04a0e810283a1e9933adedd2
//
// We pin those keys here (proven RFC-correct in
// `grpcx-crypto/tests/kat_quic.rs`) and verify the wrapper's
// `protect_payload` + `apply_header_protection` round-trip
// composes correctly: encrypt a small frame, unmask, decrypt,
// match.
#[test]
fn rfc9001_a2_client_round_trip() {
    let key = from_hex("1f369613dd76d5467730efcbe3b1a22d");
    let iv = from_hex("fa044b2f42a3fd3b46fb255c");
    let hp_key = from_hex("9f50449e04a0e810283a1e9933adedd2");
    let suite = AeadSuite::Aes128Gcm;

    // A minimal unprotected long header (Initial, pn_len=4, pn=2),
    // followed by some bytes for the payload (small for the test;
    // the real A.2 has the ClientHello). We just need enough bytes
    // so that the 16-byte sample at pn_offset+4 is valid after
    // sealing.
    let mut packet = Vec::new();
    let first_byte = 0xc3; // long header, fixed bit, type=Initial, pn_len=4
    packet.push(first_byte);
    packet.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // version v1
    packet.push(8); // dcid len
    packet.extend_from_slice(&from_hex("8394c8f03e515708")); // dcid
    packet.push(0); // scid len
    packet.push(0); // token len varint (1-byte form: 0)
    packet.push(0x40); // length varint placeholder (16-bit form), low byte
    packet.push(0x60); // ... will be plaintext_len (32) + tag_len (16) + pn_len(4) → varint(52) = 0x40 0x34; rounded for test
    let pn_offset = packet.len();
    let packet_number = 2u64;
    packet.extend_from_slice(&(packet_number as u32).to_be_bytes()); // pn = 2 (4 bytes)

    let aad = packet.clone();
    let plaintext = b"this is a minimal CRYPTO frame test payload bytes";
    let pt_len = plaintext.len();
    let tag_len = suite.tag_len();

    let aad_len = aad.len();
    packet.extend_from_slice(plaintext);
    packet.extend_from_slice(&vec![0u8; tag_len]);

    let payload_slice = &mut packet[aad_len..];
    let n = protect_payload(suite, &key, &iv, packet_number, &aad, payload_slice, pt_len)
        .expect("seal");
    assert_eq!(n, pt_len + tag_len);

    apply_header_protection(suite, &hp_key, &mut packet, pn_offset, 4, true).expect("apply HP");

    // Now go the other way: HP-unmask → AEAD-open.
    let recovered_pn_len =
        remove_header_protection(suite, &hp_key, &mut packet, pn_offset, true).expect("remove HP");
    assert_eq!(recovered_pn_len, 4);
    // Header bytes (including the now-recovered first byte and PN)
    // are the AAD again.
    let aad2 = packet[..aad_len].to_vec();
    assert_eq!(aad2, aad);
    let payload_slice = &mut packet[aad_len..];
    let pt_len_back =
        unprotect_payload(suite, &key, &iv, packet_number, &aad2, payload_slice).expect("open");
    assert_eq!(pt_len_back, pt_len);
    assert_eq!(&payload_slice[..pt_len_back], plaintext);
}
