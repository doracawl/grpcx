//! L5 1.2.0 minor — RFC 9297 H/3 datagram round-trip.
//!
//! Verifies `Connection::send_h3_datagram` /
//! `Connection::recv_h3_datagram` over the L4 1.2.0
//! DATAGRAM-frame plumbing.

use grpcx_crypto::TlsConfig;
use grpcx_h3::{Connection as H3Connection, Error as H3Error, Settings};
use grpcx_quic::{QuicConnection, StreamId};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    (cert.der().to_vec(), kp.serialize_der())
}

fn established_pair() -> (H3Connection, H3Connection) {
    let (cert_der, key_der) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);
    let cq = QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).unwrap();
    let sq = QuicConnection::new_server(&server_cfg, b"s".to_vec()).unwrap();
    let mut client = H3Connection::new_client(cq, Settings::default()).unwrap();
    let mut server = H3Connection::new_server(sq, Settings::default()).unwrap();

    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    for _ in 0..32 {
        let mut ao = Vec::new();
        for d in &to_c {
            ao.extend(client.pump(Some(d)).unwrap());
        }
        ao.extend(client.pump(None).unwrap());
        let mut bo = Vec::new();
        for d in &to_s {
            bo.extend(server.pump(Some(d)).unwrap());
        }
        bo.extend(server.pump(None).unwrap());
        to_s = ao;
        to_c = bo;
        if client.is_ready() && server.is_ready() {
            break;
        }
    }
    assert!(client.is_ready() && server.is_ready());
    (client, server)
}

#[test]
fn h3_datagram_round_trip() {
    let (mut client, mut server) = established_pair();
    // Client-initiated bidi sid = 0 (first), divisible by 4.
    let stream = StreamId(0);
    client.send_h3_datagram(stream, b"wt-dg").unwrap();
    let to_s = client.pump(None).unwrap();
    for d in &to_s {
        let _ = server.pump(Some(d)).unwrap();
    }
    let (decoded_sid, payload) = server
        .recv_h3_datagram()
        .expect("server must receive the H/3 datagram");
    assert_eq!(decoded_sid.0, 0);
    assert_eq!(payload, b"wt-dg");
}

#[test]
fn h3_datagram_non_quarter_stream_id_rejected() {
    let (mut client, _server) = established_pair();
    // sid=1 isn't on the 4-stride boundary.
    let err = client.send_h3_datagram(StreamId(1), b"x").unwrap_err();
    assert!(matches!(err, H3Error::Internal));
}
