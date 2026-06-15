//! v0.3 — extended CONNECT bootstrap end-to-end.
//!
//! Drives client + server H/3 connections, has the client open
//! a WT session via `Session::open` (extended CONNECT), the
//! server accept it via `Session::accept`, and the client see
//! `:status = 200` come back via `Session::poll_open`.

use grpcx_crypto::TlsConfig;
use grpcx_h3::{Connection as H3Connection, Settings};
use grpcx_quic::QuicConnection;
use grpcx_wt::{
    Error, Session, build_connect_request_headers, parse_response_status, validate_connect_request,
};
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

// ============================================================
// Pure-function helpers.
// ============================================================

#[test]
fn build_connect_request_smoke() {
    let h = build_connect_request_headers(b"example.com", b"/wt");
    let method = h.iter().find(|h| h.name == b":method").unwrap();
    assert_eq!(&method.value[..], b"CONNECT");
    let proto = h.iter().find(|h| h.name == b":protocol").unwrap();
    assert_eq!(&proto.value[..], b"webtransport");
    let authority = h.iter().find(|h| h.name == b":authority").unwrap();
    assert_eq!(&authority.value[..], b"example.com");
    let path = h.iter().find(|h| h.name == b":path").unwrap();
    assert_eq!(&path.value[..], b"/wt");
}

#[test]
fn validate_connect_request_round_trip() {
    let h = build_connect_request_headers(b"api.example.com", b"/sessions");
    let (auth, path) = validate_connect_request(&h).unwrap();
    assert_eq!(&auth[..], b"api.example.com");
    assert_eq!(&path[..], b"/sessions");
}

#[test]
fn validate_connect_request_missing_protocol_rejected() {
    let h = vec![
        grpcx_h3::Header {
            name: b":method".to_vec(),
            value: b"CONNECT".to_vec(),
        },
        grpcx_h3::Header {
            name: b":scheme".to_vec(),
            value: b"https".to_vec(),
        },
        grpcx_h3::Header {
            name: b":authority".to_vec(),
            value: b"x".to_vec(),
        },
        grpcx_h3::Header {
            name: b":path".to_vec(),
            value: b"/".to_vec(),
        },
    ];
    assert_eq!(validate_connect_request(&h), Err(Error::BadStreamFrame));
}

#[test]
fn validate_connect_request_wrong_method_rejected() {
    let mut h = build_connect_request_headers(b"x", b"/");
    h[0].value = b"GET".to_vec();
    assert_eq!(validate_connect_request(&h), Err(Error::BadStreamFrame));
}

#[test]
fn validate_connect_request_wrong_scheme_rejected() {
    let mut h = build_connect_request_headers(b"x", b"/");
    for hdr in h.iter_mut() {
        if hdr.name == b":scheme" {
            hdr.value = b"http".to_vec();
        }
    }
    assert_eq!(validate_connect_request(&h), Err(Error::BadStreamFrame));
}

#[test]
fn parse_status_smoke() {
    let h = vec![grpcx_h3::Header {
        name: b":status".to_vec(),
        value: b"200".to_vec(),
    }];
    assert_eq!(parse_response_status(&h).unwrap(), 200);
}

#[test]
fn parse_status_404_smoke() {
    let h = vec![grpcx_h3::Header {
        name: b":status".to_vec(),
        value: b"404".to_vec(),
    }];
    assert_eq!(parse_response_status(&h).unwrap(), 404);
}

#[test]
fn parse_status_missing_rejected() {
    let h = vec![grpcx_h3::Header {
        name: b"x-foo".to_vec(),
        value: b"bar".to_vec(),
    }];
    assert_eq!(parse_response_status(&h), Err(Error::BadStreamFrame));
}

// ============================================================
// End-to-end CONNECT round-trip.
// ============================================================

#[test]
fn extended_connect_bootstrap_round_trip() {
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

    // Drive H/3 SETTINGS exchange.
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
    // Server must have advertised CONNECT-protocol via its
    // SETTINGS (the client-side gate in Session::open).
    assert!(client.peer_settings().unwrap().enable_connect_protocol);

    // Client opens a WT session.
    let mut session = Session::open(&mut client, b"loopback.example", b"/wt").unwrap();
    assert!(!session.is_ready());

    // Pump until the server has the CONNECT request HEADERS
    // decoded.
    for _ in 0..32 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if server.pending_request_count() > 0 {
            // Try to accept now — accept_request consumes one
            // bidi sid; if headers aren't there yet we get
            // Ok(None) back and the loop will retry next round.
            break;
        }
    }
    // Try to accept; pump if headers haven't decoded yet.
    let mut server_session = None;
    for _ in 0..16 {
        match Session::accept(&mut server).unwrap() {
            Some(s) => {
                server_session = Some(s);
                break;
            }
            None => {
                let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
                to_s = a;
                to_c = b;
            }
        }
    }
    let server_session = server_session.expect("server must accept the CONNECT");
    assert!(server_session.is_ready());
    // Session id = the client-initiated bidi the CONNECT used =
    // 0 (first client bidi).
    assert_eq!(server_session.session_id().0, session.session_id().0);
    assert_eq!(session.session_id().0, 0);

    // Drive the 200 response back; client's `poll_open` flips
    // `is_ready` once it lands.
    for _ in 0..16 {
        if session.poll_open(&mut client).unwrap() {
            break;
        }
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
    }
    assert!(session.is_ready());
}

#[test]
fn open_rejects_when_peer_doesnt_advertise_connect_protocol() {
    let (cert_der, key_der) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);

    let cq = QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).unwrap();
    let sq = QuicConnection::new_server(&server_cfg, b"s".to_vec()).unwrap();

    // Server's SETTINGS does NOT advertise enable_connect_protocol.
    let s = Settings::default();
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

    // Session::open should reject with
    // ConnectProtocolNotAdvertised.
    let err = Session::open(&mut client, b"loopback.example", b"/wt").unwrap_err();
    assert_eq!(err, Error::ConnectProtocolNotAdvertised);
}
