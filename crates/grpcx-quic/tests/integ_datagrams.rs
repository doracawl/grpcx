//! L4 1.2.0 minor — RFC 9221 DATAGRAM frame round-trip.
//!
//! Verifies `QuicConnection::send_datagram` / `recv_datagram`
//! wired against the existing `Frame::Datagram` codec.

use grpcx_crypto::TlsConfig;
use grpcx_quic::QuicConnection;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    (cert.der().to_vec(), kp.serialize_der())
}

fn pump_pair(
    a: &mut QuicConnection,
    a_in: &[Vec<u8>],
    b: &mut QuicConnection,
    b_in: &[Vec<u8>],
) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut ao: Vec<Vec<u8>> = Vec::new();
    for dg in a_in {
        ao.extend(a.pump(Some(dg)).unwrap());
    }
    ao.extend(a.pump(None).unwrap());
    let mut bo: Vec<Vec<u8>> = Vec::new();
    for dg in b_in {
        bo.extend(b.pump(Some(dg)).unwrap());
    }
    bo.extend(b.pump(None).unwrap());
    (ao, bo)
}

fn drive_to_established(client: &mut QuicConnection, server: &mut QuicConnection) {
    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..16 {
        let (a, b) = pump_pair(client, &to_client, server, &to_server);
        to_server = a;
        to_client = b;
        if client.is_established()
            && server.is_established()
            && to_server.is_empty()
            && to_client.is_empty()
        {
            return;
        }
    }
}

fn established_pair() -> (QuicConnection, QuicConnection) {
    let (cert_der, key_der) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);
    let mut client = QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).unwrap();
    let mut server = QuicConnection::new_server(&server_cfg, b"s".to_vec()).unwrap();
    drive_to_established(&mut client, &mut server);
    assert!(client.is_established() && server.is_established());
    (client, server)
}

#[test]
fn datagram_client_to_server_round_trip() {
    let (mut client, mut server) = established_pair();
    client.send_datagram(b"hello-dg").unwrap();
    let to_s = client.pump(None).unwrap();
    for d in &to_s {
        let _ = server.pump(Some(d)).unwrap();
    }
    let payload = server
        .recv_datagram()
        .expect("server must have received the datagram");
    assert_eq!(payload, b"hello-dg");
}

#[test]
fn datagram_server_to_client_round_trip() {
    let (mut client, mut server) = established_pair();
    server.send_datagram(b"server-side").unwrap();
    let mut to_c = server.pump(None).unwrap();
    for _ in 0..4 {
        for d in to_c.drain(..) {
            let _ = client.pump(Some(&d)).unwrap();
        }
        if let Some(payload) = client.recv_datagram() {
            assert_eq!(payload, b"server-side");
            return;
        }
        to_c = server.pump(None).unwrap();
    }
    panic!("client must have received the datagram");
}

#[test]
fn multiple_datagrams_fifo() {
    let (mut client, mut server) = established_pair();
    client.send_datagram(b"a").unwrap();
    client.send_datagram(b"b").unwrap();
    client.send_datagram(b"c").unwrap();
    let to_s = client.pump(None).unwrap();
    for d in &to_s {
        let _ = server.pump(Some(d)).unwrap();
    }
    let g1 = server.recv_datagram().unwrap();
    let g2 = server.recv_datagram().unwrap();
    let g3 = server.recv_datagram().unwrap();
    assert!(server.recv_datagram().is_none());
    assert_eq!(g1, b"a");
    assert_eq!(g2, b"b");
    assert_eq!(g3, b"c");
}

#[test]
fn send_datagram_rejected_before_1rtt() {
    let (cert_der, _key_der) = mint_cert();
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);
    let mut client = QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).unwrap();
    assert!(client.send_datagram(b"x").is_err());
}
