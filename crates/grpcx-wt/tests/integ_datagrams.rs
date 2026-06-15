//! v0.5 — Session datagram dispatch end-to-end.
//!
//! Drives client + server H/3 + WT through:
//! 1. QUIC + H/3 SETTINGS exchange (with
//!    `SETTINGS_ENABLE_CONNECT_PROTOCOL = 1`).
//! 2. CONNECT bootstrap to establish one WT session.
//! 3. Datagrams in both directions, parsed by
//!    `Session::poll` and drained via `Session::recv_datagram`.

use grpcx_crypto::TlsConfig;
use grpcx_h3::{Connection as H3Connection, Settings};
use grpcx_quic::{QuicConnection, StreamId};
use grpcx_wt::{Error, Session};
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
    let server_session = server_session.expect("server must accept the CONNECT");
    assert!(client_session.is_ready() && server_session.is_ready());
    (client, server, client_session, server_session)
}

#[test]
fn client_to_server_datagram_round_trip() {
    let (mut client, mut server, mut cs, mut ss) = established_session_pair();
    cs.send_datagram(&mut client, b"hello-wt-dg").unwrap();

    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        ss.poll(&mut server).unwrap();
        if let Some(payload) = ss.recv_datagram() {
            assert_eq!(payload, b"hello-wt-dg");
            return;
        }
    }
    panic!("server-side WT session must surface the datagram");
}

#[test]
fn server_to_client_datagram_round_trip() {
    let (mut client, mut server, mut cs, mut ss) = established_session_pair();
    ss.send_datagram(&mut server, b"response-dg").unwrap();

    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        cs.poll(&mut client).unwrap();
        if let Some(payload) = cs.recv_datagram() {
            assert_eq!(payload, b"response-dg");
            return;
        }
    }
    panic!("client-side WT session must surface the datagram");
}

#[test]
fn multiple_datagrams_fifo_on_session() {
    let (mut client, mut server, mut cs, mut ss) = established_session_pair();
    cs.send_datagram(&mut client, b"one").unwrap();
    cs.send_datagram(&mut client, b"two").unwrap();
    cs.send_datagram(&mut client, b"three").unwrap();

    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    let mut got: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        ss.poll(&mut server).unwrap();
        while let Some(payload) = ss.recv_datagram() {
            got.push(payload);
        }
        if got.len() >= 3 {
            break;
        }
    }
    assert_eq!(got.len(), 3);
    assert_eq!(got[0], b"one");
    assert_eq!(got[1], b"two");
    assert_eq!(got[2], b"three");
}

#[test]
fn cross_session_datagram_rejected_by_poll() {
    let (mut client, mut server, _cs, mut ss) = established_session_pair();
    // Inject a datagram with a quarter-stream-id that doesn't
    // belong to our session — go around `Session::send_datagram`
    // and call L5 directly with a sid offset by 4 from ours.
    let wrong_sid = StreamId(ss.session_id().0 + 4);
    client.send_h3_datagram(wrong_sid, b"x").unwrap();

    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    let mut err = None;
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        match ss.poll(&mut server) {
            Ok(()) => {}
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    assert!(
        matches!(err, Some(Error::Internal)),
        "cross-session datagram must surface as Error::Internal; got {err:?}"
    );
}
