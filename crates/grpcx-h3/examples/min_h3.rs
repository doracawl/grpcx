//! Minimal HTTP/3 loopback for BINSIZE measurement.
//!
//! Drives a client + server `Connection` through SETTINGS
//! exchange + one GET `/` → 200 OK round-trip.
//! `cargo build --release --example min_h3` produces the binary
//! whose stripped size fills the `BUDGETS.md` BINSIZE entry.

use grpcx_crypto::TlsConfig;
use grpcx_h3::{Connection as H3Connection, Header, Settings};
use grpcx_quic::QuicConnection;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    (cert.der().to_vec(), kp.serialize_der())
}

fn h(n: &[u8], v: &[u8]) -> Header {
    Header {
        name: n.to_vec(),
        value: v.to_vec(),
    }
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

    let mut client = H3Connection::new_client(cq, Settings::default()).unwrap();
    let mut server = H3Connection::new_server(sq, Settings::default()).unwrap();

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

    let req = client
        .send_request(&[
            h(b":method", b"GET"),
            h(b":scheme", b"https"),
            h(b":path", b"/"),
            h(b":authority", b"loopback.example"),
        ])
        .unwrap();
    client.finish(&req).unwrap();

    for _ in 0..16 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if server.pending_request_count() > 0 {
            break;
        }
    }
    let srv_req = server.accept_request().unwrap().unwrap();
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if server.received_headers_ready(&srv_req) {
            break;
        }
    }
    let _ = server.read_headers(&srv_req).unwrap().unwrap();
    server
        .write_headers(&srv_req, &[h(b":status", b"200")])
        .unwrap();
    server.write_data(&srv_req, b"ok").unwrap();
    server.finish(&srv_req).unwrap();

    for _ in 0..16 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        let (_, fin) = client.data_buffer_status(&req);
        if client.received_headers_ready(&req) && fin {
            break;
        }
    }
    let _ = client.read_headers(&req).unwrap().unwrap();
    let mut body = Vec::new();
    let (n, fin) = client.read_data(&req, &mut body).unwrap();
    assert_eq!(n, body.len());
    assert_eq!(&body[..], b"ok");
    assert!(fin);
    println!("min_h3 ok ({} bytes)", n);
}
