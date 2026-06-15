//! Minimal WebTransport loopback for BINSIZE measurement.
//!
//! Drives one extended-CONNECT bootstrap + one WT bidi
//! round-trip + one datagram + a close capsule between two
//! `grpcx_h3::Connection`s on a shared loopback bus.
//! `cargo build --release --example min_wt` produces the binary
//! whose stripped size fills the `BUDGETS.md` BINSIZE entry.

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

fn main() {
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

    // 1. Drive QUIC + H/3 SETTINGS exchange.
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

    // 2. WT bootstrap (extended CONNECT).
    let mut cs = Session::open(&mut client, SERVER_NAME.as_bytes(), b"/wt").unwrap();
    let mut ss = None;
    for _ in 0..32 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if ss.is_none()
            && let Some(s) = Session::accept(&mut server).unwrap()
        {
            ss = Some(s);
        }
        if !cs.is_ready() {
            let _ = cs.poll_open(&mut client).unwrap();
        }
        if ss.is_some() && cs.is_ready() {
            break;
        }
    }
    let mut ss = ss.expect("server CONNECT accept");

    // 3. One WT bidi round-trip.
    let stream = cs.open_bi(&mut client).unwrap();
    cs.write_stream(&mut client, stream.stream_id(), b"hi", false)
        .unwrap();

    // 4. One datagram.
    cs.send_datagram(&mut client, b"ping").unwrap();

    // 5. Pump until server sees the stream + datagram.
    for _ in 0..16 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        ss.poll(&mut server).unwrap();
        if ss.accept_bi().unwrap().is_some() && ss.recv_datagram().is_some() {
            break;
        }
    }

    // 6. Client closes.
    cs.close(&mut client, 0, b"done").unwrap();
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        ss.poll(&mut server).unwrap();
        if ss.is_closed() {
            break;
        }
    }
}
