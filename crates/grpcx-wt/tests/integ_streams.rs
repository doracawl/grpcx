//! v0.4 — Session stream dispatch end-to-end.
//!
//! Drives client + server H/3 + WT through:
//! 1. QUIC + H/3 SETTINGS exchange (with
//!    `SETTINGS_ENABLE_CONNECT_PROTOCOL = 1`).
//! 2. Extended CONNECT bootstrap to establish one WT session.
//! 3. Locally-initiated bidi + uni WT streams from each side.
//! 4. Peer classifies via `Session::poll`, accepts via
//!    `Session::accept_bi` / `Session::accept_uni`, reads via
//!    `Session::read_stream`.

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

/// Stand-up: returns (client, server) H/3 connections already
/// settled into is_ready, plus an established WT session pair.
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
    assert!(client_session.is_ready());
    assert!(server_session.is_ready());
    assert_eq!(client_session.session_id().0, server_session.session_id().0);

    (client, server, client_session, server_session)
}

#[test]
fn client_initiated_bidi_round_trip() {
    let (mut client, mut server, mut cs, mut ss) = established_session_pair();

    // Client opens a WT bidi, writes a small payload.
    let stream = cs.open_bi(&mut client).unwrap();
    let bytes_written = cs
        .write_stream(&mut client, stream.stream_id(), b"ping", false)
        .unwrap();
    assert_eq!(bytes_written, 4);

    // Drive pumps until the server's poll classifies + queues the
    // bidi for accept_bi.
    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    let mut server_stream = None;
    for _ in 0..16 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        ss.poll(&mut server).unwrap();
        if let Some(s) = ss.accept_bi().unwrap() {
            server_stream = Some(s);
            break;
        }
    }
    let server_stream = server_stream.expect("server must classify + queue the WT bidi");
    assert_eq!(server_stream.stream_id().0, stream.stream_id().0);

    // Drain server's view of the payload.
    let mut got = Vec::new();
    for _ in 0..8 {
        let (n, _fin) = ss.read_stream(&mut server, server_stream.stream_id(), &mut got);
        if !got.is_empty() {
            let _ = n;
            break;
        }
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        ss.poll(&mut server).unwrap();
    }
    assert_eq!(got, b"ping");
}

#[test]
fn client_initiated_uni_round_trip() {
    let (mut client, mut server, mut cs, mut ss) = established_session_pair();

    let stream = cs.open_uni(&mut client).unwrap();
    cs.write_stream(&mut client, stream.stream_id(), b"alert", true)
        .unwrap();

    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    let mut server_stream = None;
    for _ in 0..16 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        ss.poll(&mut server).unwrap();
        if let Some(s) = ss.accept_uni().unwrap() {
            server_stream = Some(s);
            break;
        }
    }
    let server_stream = server_stream.expect("server must classify + queue the WT uni");
    assert_eq!(server_stream.stream_id().0, stream.stream_id().0);

    let mut got = Vec::new();
    let mut fin_seen = false;
    for _ in 0..8 {
        let (_, fin) = ss.read_stream(&mut server, server_stream.stream_id(), &mut got);
        if fin {
            fin_seen = true;
            break;
        }
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        ss.poll(&mut server).unwrap();
    }
    assert_eq!(got, b"alert");
    assert!(fin_seen);
}

#[test]
fn two_bidis_on_one_session_arrive_in_order() {
    let (mut client, mut server, mut cs, mut ss) = established_session_pair();

    let a_stream = cs.open_bi(&mut client).unwrap();
    cs.write_stream(&mut client, a_stream.stream_id(), b"first", false)
        .unwrap();
    let b_stream = cs.open_bi(&mut client).unwrap();
    cs.write_stream(&mut client, b_stream.stream_id(), b"second", false)
        .unwrap();

    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    let mut a_server = None;
    let mut b_server = None;
    for _ in 0..32 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        ss.poll(&mut server).unwrap();
        if a_server.is_none() {
            a_server = ss.accept_bi().unwrap();
        } else if b_server.is_none() {
            b_server = ss.accept_bi().unwrap();
        }
        if a_server.is_some() && b_server.is_some() {
            break;
        }
    }
    let a_server = a_server.expect("first bidi");
    let b_server = b_server.expect("second bidi");
    assert_eq!(a_server.stream_id().0, a_stream.stream_id().0);
    assert_eq!(b_server.stream_id().0, b_stream.stream_id().0);

    // Drain both payloads.
    let mut a_got = Vec::new();
    let mut b_got = Vec::new();
    for _ in 0..8 {
        ss.read_stream(&mut server, a_server.stream_id(), &mut a_got);
        ss.read_stream(&mut server, b_server.stream_id(), &mut b_got);
        if !a_got.is_empty() && !b_got.is_empty() {
            break;
        }
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        ss.poll(&mut server).unwrap();
    }
    assert_eq!(a_got, b"first");
    assert_eq!(b_got, b"second");
}

#[test]
fn bad_stream_prefix_rejected_by_poll() {
    let (mut client, mut server, _cs, mut ss) = established_session_pair();

    // Bypass `Session::open_bi` — emit a malformed prefix (`0x53`
    // instead of `0x54`) directly on a raw bidi so `poll` faces a
    // bad-WT-type-byte case.
    let sid = client.open_bi_raw();
    client.write_stream_raw(sid, &[0x53, 0x00], false).unwrap();

    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    let mut err = None;
    for _ in 0..16 {
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
        matches!(err, Some(grpcx_wt::Error::BadStreamFrame)),
        "poll must reject a bad WT stream prefix; got {err:?}"
    );
}
