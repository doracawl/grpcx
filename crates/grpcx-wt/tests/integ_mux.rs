//! v0.6 — `SessionMux` multi-session dispatch.
//!
//! Two clients (single client-side H/3 conn opens TWO WT
//! sessions to the same server-side conn) ride the same
//! `grpcx_h3::Connection` pair; the server uses `SessionMux` to
//! route streams + datagrams by session id.

use grpcx_crypto::TlsConfig;
use grpcx_h3::{Connection as H3Connection, Settings};
use grpcx_quic::QuicConnection;
use grpcx_wt::{Session, SessionMux};
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

#[test]
fn two_sessions_isolate_streams_and_datagrams() {
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

    // Client opens two sessions; server accepts both into a mux.
    let mut cs_a = Session::open(&mut client, SERVER_NAME.as_bytes(), b"/wt/a").unwrap();
    let mut cs_b = Session::open(&mut client, SERVER_NAME.as_bytes(), b"/wt/b").unwrap();
    let mut server_mux = SessionMux::new();

    for _ in 0..48 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        // Accept all pending server-side CONNECT bidis.
        while let Some(ss) = Session::accept(&mut server).unwrap() {
            server_mux.insert(ss);
        }
        // Drive client-side bootstrap to ready.
        if !cs_a.is_ready() {
            let _ = cs_a.poll_open(&mut client).unwrap();
        }
        if !cs_b.is_ready() {
            let _ = cs_b.poll_open(&mut client).unwrap();
        }
        if server_mux.len() == 2 && cs_a.is_ready() && cs_b.is_ready() {
            break;
        }
    }
    assert_eq!(server_mux.len(), 2);
    assert!(cs_a.is_ready() && cs_b.is_ready());

    // Open WT bidis on each session.
    let stream_a = cs_a.open_bi(&mut client).unwrap();
    cs_a.write_stream(&mut client, stream_a.stream_id(), b"alpha", false)
        .unwrap();
    let stream_b = cs_b.open_bi(&mut client).unwrap();
    cs_b.write_stream(&mut client, stream_b.stream_id(), b"bravo", false)
        .unwrap();
    // And a datagram on each.
    cs_a.send_datagram(&mut client, b"a-dg").unwrap();
    cs_b.send_datagram(&mut client, b"b-dg").unwrap();

    // Drive pumps; let the server's mux classify.
    let id_a = cs_a.session_id().0;
    let id_b = cs_b.session_id().0;
    let mut got_a_stream: Option<Vec<u8>> = None;
    let mut got_b_stream: Option<Vec<u8>> = None;
    let mut got_a_dg: Option<Vec<u8>> = None;
    let mut got_b_dg: Option<Vec<u8>> = None;
    for _ in 0..32 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        server_mux.poll(&mut server).unwrap();

        if got_a_stream.is_none() {
            let session_a = server_mux.get_mut(id_a).unwrap();
            if let Some(sbi) = session_a.accept_bi().unwrap() {
                let mut buf = Vec::new();
                session_a.read_stream(&mut server, sbi.stream_id(), &mut buf);
                if !buf.is_empty() {
                    got_a_stream = Some(buf);
                }
            }
        }
        if got_b_stream.is_none() {
            let session_b = server_mux.get_mut(id_b).unwrap();
            if let Some(sbi) = session_b.accept_bi().unwrap() {
                let mut buf = Vec::new();
                session_b.read_stream(&mut server, sbi.stream_id(), &mut buf);
                if !buf.is_empty() {
                    got_b_stream = Some(buf);
                }
            }
        }
        if got_a_dg.is_none() {
            got_a_dg = server_mux.get_mut(id_a).unwrap().recv_datagram();
        }
        if got_b_dg.is_none() {
            got_b_dg = server_mux.get_mut(id_b).unwrap().recv_datagram();
        }

        if got_a_stream.is_some()
            && got_b_stream.is_some()
            && got_a_dg.is_some()
            && got_b_dg.is_some()
        {
            break;
        }
    }

    assert_eq!(got_a_stream.as_deref(), Some(&b"alpha"[..]));
    assert_eq!(got_b_stream.as_deref(), Some(&b"bravo"[..]));
    assert_eq!(got_a_dg.as_deref(), Some(&b"a-dg"[..]));
    assert_eq!(got_b_dg.as_deref(), Some(&b"b-dg"[..]));
}

#[test]
fn mux_drops_datagram_for_unknown_session() {
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

    // Open + register one session on each side so the mux has
    // something to compare against.
    let mut cs = Session::open(&mut client, SERVER_NAME.as_bytes(), b"/wt").unwrap();
    let mut server_mux = SessionMux::new();
    for _ in 0..32 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if let Some(ss) = Session::accept(&mut server).unwrap() {
            server_mux.insert(ss);
        }
        if !cs.is_ready() {
            let _ = cs.poll_open(&mut client).unwrap();
        }
        if server_mux.len() == 1 && cs.is_ready() {
            break;
        }
    }
    let registered = cs.session_id().0;
    let stranger = registered + 4_000; // any nonexistent session id (4-stride)

    // Send a datagram on the registered session AND one
    // direct-to-L5 on a stranger session id. Mux must route the
    // first to the right session and silently drop the second.
    cs.send_datagram(&mut client, b"good").unwrap();
    client
        .send_h3_datagram(grpcx_quic::StreamId(stranger), b"stray")
        .unwrap();

    let mut got: Option<Vec<u8>> = None;
    for _ in 0..16 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        // Must not return Err for the stranger.
        server_mux.poll(&mut server).unwrap();
        if got.is_none() {
            got = server_mux
                .get_mut(registered)
                .and_then(|s| s.recv_datagram());
        }
        if got.is_some() {
            break;
        }
    }
    assert_eq!(got.as_deref(), Some(&b"good"[..]));
    // Stranger session: still has no presence in the mux.
    assert!(server_mux.get(stranger).is_none());
}
