//! Minimal QUIC loopback for BINSIZE measurement.
//!
//! Drives an in-process client + server through a full handshake
//! to 1-RTT and one bidirectional stream round-trip. Build with
//! `cargo build --release --example min_quic` and measure the
//! stripped binary size to fill in `BUDGETS.md` BINSIZE for L4.

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

fn pump(
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

fn main() {
    let (cert_der, key_der) = mint_cert();

    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);

    let mut client =
        QuicConnection::new_client(&client_cfg, SERVER_NAME, b"client-tp".to_vec()).unwrap();
    let mut server = QuicConnection::new_server(&server_cfg, b"server-tp".to_vec()).unwrap();

    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..32 {
        let (a, b) = pump(&mut client, &to_client, &mut server, &to_server);
        to_server = a;
        to_client = b;
        if client.is_established()
            && server.is_established()
            && to_server.is_empty()
            && to_client.is_empty()
        {
            break;
        }
    }
    assert!(client.is_established());
    assert!(server.is_established());

    let sid = client.open_bi();
    client.write_stream(sid, b"GET /index", true).unwrap();
    for _ in 0..16 {
        let (a, b) = pump(&mut client, &to_client, &mut server, &to_server);
        to_server = a;
        to_client = b;
        if server.streams_have_data(sid) {
            break;
        }
    }
    let mut recv = Vec::new();
    let (_, fin) = server.read_stream(sid, &mut recv);
    assert_eq!(&recv[..], b"GET /index");
    assert!(fin);
    println!("min_quic ok ({} bytes round-trip)", recv.len());
}
