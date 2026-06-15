//! L3 ↔ L4 integration: one Initial packet round-trip carrying a
//! real TLS 1.3 ClientHello (via `grpcx-crypto`'s `ClientHandshake`).
//!
//! Goal: prove the v0.2-v0.4 primitives (header codec + frame
//! codec + packet protection) compose correctly when fed real
//! handshake bytes from L3. The full multi-packet handshake loop
//! + 1-RTT promotion is the v0.6 deliverable.

use grpcx_crypto::{ClientHandshake, TlsConfig};
use grpcx_quic::{ConnectionId, Frame, Side, build_initial_packet, parse_initial_packet};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).expect("CertificateParams");
    let kp = KeyPair::generate_for(&PKCS_ED25519).expect("kp");
    let cert = params.self_signed(&kp).expect("self_signed");
    (cert.der().to_vec(), kp.serialize_der())
}

fn cid(bytes: &[u8]) -> ConnectionId {
    assert!(bytes.len() <= 20);
    let mut out = ConnectionId {
        bytes: [0u8; 20],
        len: bytes.len() as u8,
    };
    out.bytes[..bytes.len()].copy_from_slice(bytes);
    out
}

#[test]
fn initial_packet_round_trip_carries_client_hello() {
    // Set up the client side just enough to get ClientHello bytes.
    let (cert_der, _) = mint_cert();
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).expect("trust");
    let mut client = ClientHandshake::new(&client_cfg, SERVER_NAME).expect("client handshake");
    let client_hello = client.take_handshake_bytes();
    assert!(!client_hello.is_empty(), "ClientHello must have bytes");

    // QUIC v1 reference connection ID (we just need one; pick the
    // RFC 9001 §A.2 sample shape so the keys are deterministic).
    let dcid = cid(&[0x83, 0x94, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08]);
    let scid = cid(&[]);

    // Build the Initial packet — client side, pad to 1200 bytes
    // (RFC 9000 §14.1 minimum for first client flight).
    let packet = build_initial_packet(&dcid, &scid, &[], 0, &client_hello, Side::Client, 1200)
        .expect("build initial");
    assert!(
        packet.len() >= 1200,
        "first client Initial must be ≥ 1200 bytes (RFC 9000 §14.1), got {}",
        packet.len()
    );

    // Parse it back — same DCID is used to derive the keys.
    let mut packet_copy = packet.clone();
    let (header, frames) = parse_initial_packet(
        &mut packet_copy,
        &dcid.bytes[..dcid.len as usize],
        Side::Client,
    )
    .expect("parse initial");

    // Header sanity.
    assert_eq!(header.version, 0x0000_0001);
    assert_eq!(header.packet_number, 0);
    assert_eq!(header.pn_len, 4);
    assert_eq!(
        &header.dcid.bytes[..header.dcid.len as usize],
        &dcid.bytes[..dcid.len as usize]
    );

    // The first frame must be a CRYPTO frame at offset 0 with the
    // original ClientHello bytes. Padding frames follow.
    let crypto_frame = frames
        .iter()
        .find(|f| matches!(f, Frame::Crypto { .. }))
        .expect("CRYPTO frame present");
    if let Frame::Crypto { offset, data } = crypto_frame {
        assert_eq!(*offset, 0);
        assert_eq!(data, &client_hello, "ClientHello bytes survived round-trip");
    }

    // The remaining frames should all be PADDING.
    for f in frames.iter().filter(|f| !matches!(f, Frame::Crypto { .. })) {
        assert!(
            matches!(f, Frame::Padding),
            "non-CRYPTO frame should be Padding, got {f:?}"
        );
    }
}
