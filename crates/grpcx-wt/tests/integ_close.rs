//! v0.6 — `CLOSE_WEBTRANSPORT_SESSION` capsule round-trip.

use grpcx_crypto::TlsConfig;
use grpcx_h3::{Connection as H3Connection, Settings};
use grpcx_quic::QuicConnection;
use grpcx_wt::Session;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    (cert.der().to_vec(), kp.serialize_der())
}

fn pump_pair(
    a: &mut H3Connection,
    a_in: &[Vec<u8>],
    b: &mut H3Connection,
    b_in: &[Vec<u8>],
) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut ao: Vec<Vec<u8>> = Vec::new();
    for d in a_in {
        ao.extend(a.pump(Some(d)).unwrap());
    }
    ao.extend(a.pump(None).unwrap());
    let mut bo: Vec<Vec<u8>> = Vec::new();
    for d in b_in {
        bo.extend(b.pump(Some(d)).unwrap());
    }
    bo.extend(b.pump(None).unwrap());
    (ao, bo)
}

fn established_session_pair() -> (H3Connection, H3Connection, Session, Session) {
    let (cert_der, key_der) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);
    let cq = QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).unwrap();
    let sq = QuicConnection::new_server(&server_cfg, b"s".to_vec()).unwrap();
    let s = Settings {
        enable_connect_protocol: true,
        ..Settings::default()
    };
    let mut client = H3Connection::new_client(cq, s.clone()).unwrap();
    let mut server = H3Connection::new_server(sq, s).unwrap();

    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    for _ in 0..32 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if client.is_ready() && server.is_ready() {
            break;
        }
    }
    assert!(client.is_ready() && server.is_ready());

    let mut client_session = Session::open(&mut client, SERVER_NAME.as_bytes(), b"/wt").unwrap();
    let mut server_session = None;
    for _ in 0..32 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if server_session.is_none()
            && let Some(s) = Session::accept(&mut server).unwrap()
        {
            server_session = Some(s);
        }
        if !client_session.is_ready() {
            let _ = client_session.poll_open(&mut client).unwrap();
        }
        if server_session.is_some() && client_session.is_ready() {
            break;
        }
    }
    let server_session = server_session.unwrap();
    assert!(client_session.is_ready() && server_session.is_ready());
    (client, server, client_session, server_session)
}

#[test]
fn client_close_propagates_to_server() {
    let (mut client, mut server, mut cs, mut ss) = established_session_pair();
    assert!(!cs.is_closed());
    assert!(!ss.is_closed());

    cs.close(&mut client, 42, b"client done").unwrap();
    assert!(cs.is_closed());

    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        ss.poll(&mut server).unwrap();
        if ss.is_closed() {
            break;
        }
    }
    assert!(ss.is_closed(), "server-side session must observe close");
    let (code, reason) = ss.close_info().unwrap();
    assert_eq!(code, 42);
    assert_eq!(reason, b"client done");
}

#[test]
fn server_close_propagates_to_client() {
    let (mut client, mut server, mut cs, mut ss) = established_session_pair();
    ss.close(&mut server, 7, b"by-server").unwrap();
    assert!(ss.is_closed());

    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        cs.poll(&mut client).unwrap();
        if cs.is_closed() {
            break;
        }
    }
    let (code, reason) = cs.close_info().unwrap();
    assert_eq!(code, 7);
    assert_eq!(reason, b"by-server");
}

#[test]
fn close_is_idempotent() {
    let (mut client, _server, mut cs, _ss) = established_session_pair();
    cs.close(&mut client, 1, b"first").unwrap();
    // Second call must NOT panic and must NOT re-emit.
    cs.close(&mut client, 99, b"ignored").unwrap();
    assert!(cs.is_closed());
}

#[test]
fn close_oversize_reason_rejected() {
    let (mut client, _server, mut cs, _ss) = established_session_pair();
    let huge = vec![b'x'; 2048];
    let err = cs.close(&mut client, 0, &huge).unwrap_err();
    assert!(matches!(err, grpcx_wt::Error::BadStreamFrame));
    assert!(!cs.is_closed());
}
