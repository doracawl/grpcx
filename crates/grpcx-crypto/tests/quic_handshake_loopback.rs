//! L3 v1.2 QUIC-mode handshake loopback — drives `QuicEndpoint`
//! through a full client ↔ server handshake purely via the
//! handshake-byte stream (no QUIC framing). Asserts both sides
//! reach `is_handshaking() == false` and agree on ALPN.
//!
//! Note: this is the **L3** test — it doesn't speak QUIC framing.
//! L4 (`grpcx-quic`) will compose this driver with packet
//! protection in v0.6.

use grpcx_crypto::TlsConfig;
use grpcx_crypto::quic_handshake::{QuicEndpoint, QuicKeyChange, QuicVersion};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).expect("CertificateParams");
    let kp = KeyPair::generate_for(&PKCS_ED25519).expect("kp");
    let cert = params.self_signed(&kp).expect("self_signed");
    (cert.der().to_vec(), kp.serialize_der())
}

#[test]
fn quic_loopback_handshake_completes() {
    let (cert_der, key_der) = mint_cert();

    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).expect("server cfg");
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).expect("trust");
    client_cfg.set_alpn(&[b"h3"]);

    let client_tp: Vec<u8> = b"client-tp-opaque".to_vec();
    let server_tp: Vec<u8> = b"server-tp-opaque".to_vec();

    let mut client =
        QuicEndpoint::new_client(&client_cfg, QuicVersion::V1, SERVER_NAME, client_tp.clone())
            .expect("client");
    let mut server =
        QuicEndpoint::new_server(&server_cfg, QuicVersion::V1, server_tp.clone()).expect("server");

    let mut client_key_changes: Vec<QuicKeyChange> = Vec::new();
    let mut server_key_changes: Vec<QuicKeyChange> = Vec::new();

    // Drive until both sides finish their handshake. Each round:
    // 1. drain client outbound; feed to server.
    // 2. drain server outbound; feed to client.
    // Record any KeyChange returned by either side.
    for _ in 0..16 {
        let (c2s, kc_c) = client.write_hs();
        if let Some(kc) = kc_c {
            client_key_changes.push(kc);
        }
        if !c2s.is_empty() {
            server.read_hs(&c2s).expect("server feed");
        }
        let (s2c, kc_s) = server.write_hs();
        if let Some(kc) = kc_s {
            server_key_changes.push(kc);
        }
        if !s2c.is_empty() {
            client.read_hs(&s2c).expect("client feed");
        }
        if c2s.is_empty() && s2c.is_empty() {
            break;
        }
    }
    // Both sides should have seen Handshake-level + 1-RTT keys.
    assert_eq!(
        client_key_changes.len(),
        2,
        "client must see Handshake + 1-RTT key changes"
    );
    assert_eq!(
        server_key_changes.len(),
        2,
        "server must see Handshake + 1-RTT key changes"
    );
    assert!(matches!(client_key_changes[0], QuicKeyChange::Handshake(_)));
    assert!(matches!(client_key_changes[1], QuicKeyChange::OneRtt(_)));

    assert!(!client.is_handshaking(), "client should be done");
    assert!(!server.is_handshaking(), "server should be done");
    assert_eq!(client.alpn_protocol().as_deref(), Some(b"h3".as_ref()));
    assert_eq!(server.alpn_protocol().as_deref(), Some(b"h3".as_ref()));
    // Peer transport params are visible after the handshake feed.
    assert_eq!(
        client.peer_transport_params().as_deref(),
        Some(server_tp.as_slice())
    );
    assert_eq!(
        server.peer_transport_params().as_deref(),
        Some(client_tp.as_slice())
    );
}
