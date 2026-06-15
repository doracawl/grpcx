//! L4 1.1.0 minor — uni-stream open + peer-stream enumeration.
//!
//! Verifies the new `open_uni` / `accept_uni` / `accept_bi` API
//! shipped for L5's H/3 control + QPACK streams.

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
    let mut a_out: Vec<Vec<u8>> = Vec::new();
    for dg in a_in {
        a_out.extend(a.pump(Some(dg)).unwrap());
    }
    a_out.extend(a.pump(None).unwrap());

    let mut b_out: Vec<Vec<u8>> = Vec::new();
    for dg in b_in {
        b_out.extend(b.pump(Some(dg)).unwrap());
    }
    b_out.extend(b.pump(None).unwrap());

    (a_out, b_out)
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

#[test]
fn client_open_uni_then_server_accept_uni() {
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

    // Client opens a uni and pushes one byte + FIN.
    let sid = client.open_uni();
    // RFC 9000 §2.1: first client-initiated uni id = 2.
    assert_eq!(sid.0, 2);
    client.write_stream(sid, b"X", true).unwrap();

    // Pump until the server has the byte buffered.
    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_client, &mut server, &to_server);
        to_server = a;
        to_client = b;
        if server.streams_have_data(sid) {
            break;
        }
    }

    // Server should see the stream in its peer-initiated-uni queue.
    let accepted = server
        .accept_uni()
        .expect("server must see the new uni stream");
    assert_eq!(accepted.0, sid.0);
    // No more peer streams to drain.
    assert!(server.accept_uni().is_none());

    // Bi queue must be untouched.
    assert!(server.accept_bi().is_none());

    // Read the byte + FIN.
    let mut out = Vec::new();
    let (n, fin) = server.read_stream(accepted, &mut out);
    assert_eq!(n, 1);
    assert_eq!(&out[..], b"X");
    assert!(fin);
}

#[test]
fn server_open_uni_then_client_accept_uni() {
    let (cert_der, key_der) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);

    let mut client = QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).unwrap();
    let mut server = QuicConnection::new_server(&server_cfg, b"s".to_vec()).unwrap();
    drive_to_established(&mut client, &mut server);

    let sid = server.open_uni();
    // First server-initiated uni id = 3.
    assert_eq!(sid.0, 3);
    server.write_stream(sid, b"Y", true).unwrap();

    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_client, &mut server, &to_server);
        to_server = a;
        to_client = b;
        if client.streams_have_data(sid) {
            break;
        }
    }

    let accepted = client
        .accept_uni()
        .expect("client must see the new uni stream");
    assert_eq!(accepted.0, sid.0);
    assert!(client.accept_uni().is_none());
}

#[test]
fn accept_bi_queue_separate_from_uni() {
    let (cert_der, key_der) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);

    let mut client = QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).unwrap();
    let mut server = QuicConnection::new_server(&server_cfg, b"s".to_vec()).unwrap();
    drive_to_established(&mut client, &mut server);

    // Open one bi + one uni from the client.
    let bi = client.open_bi();
    let uni = client.open_uni();
    client.write_stream(bi, b"B", true).unwrap();
    client.write_stream(uni, b"U", true).unwrap();

    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_client, &mut server, &to_server);
        to_server = a;
        to_client = b;
        if server.streams_have_data(bi) && server.streams_have_data(uni) {
            break;
        }
    }

    let got_bi = server.accept_bi().expect("server must see the new bi");
    let got_uni = server.accept_uni().expect("server must see the new uni");
    assert_eq!(got_bi.0, bi.0);
    assert_eq!(got_uni.0, uni.0);
    assert!(server.accept_bi().is_none());
    assert!(server.accept_uni().is_none());
}

#[test]
fn second_open_uni_advances_id_by_4() {
    // RFC 9000 §2.1: ids of one (initiator, direction) class go
    // 0, 4, 8, … — the +4 stride.
    let (cert_der, key_der) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);

    let mut client = QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).unwrap();

    let s1 = client.open_uni();
    let s2 = client.open_uni();
    let s3 = client.open_uni();
    assert_eq!(s1.0, 2);
    assert_eq!(s2.0, 6);
    assert_eq!(s3.0, 10);
}
