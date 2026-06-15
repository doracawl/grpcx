//! Minimum-size TLS 1.3 loopback for the BINSIZE benchmark.
//!
//! Builds a server config, a client config trusting the same
//! self-signed cert, runs a handshake in-memory, and exchanges one
//! application-data record each direction. Touches every public type
//! the wrapper ships so `cargo bloat` sees a representative
//! footprint.
//!
//! `cargo bloat --release --example min_tls --crates` is the
//! BINSIZE source-of-truth for `BUDGETS.md`.

use grpcx_crypto::{ClientHandshake, ServerHandshake, TlsConfig};

const SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).expect("CertificateParams");
    let kp = KeyPair::generate_for(&PKCS_ED25519).expect("kp");
    let cert = params.self_signed(&kp).expect("self_signed");
    (cert.der().to_vec(), kp.serialize_der())
}

fn main() {
    let (cert, key) = mint_cert();
    let server_cfg = TlsConfig::server(&[&cert], &key).expect("server cfg");
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert).expect("trust");

    let mut client = ClientHandshake::new(&client_cfg, SERVER_NAME).expect("client handshake");
    let mut server = ServerHandshake::new(&server_cfg).expect("server handshake");

    for _ in 0..32 {
        let c2s = client.take_handshake_bytes();
        if !c2s.is_empty() {
            server.process_handshake_bytes(&c2s).expect("server feed");
        }
        let s2c = server.take_handshake_bytes();
        if !s2c.is_empty() {
            client.process_handshake_bytes(&s2c).expect("client feed");
        }
        if c2s.is_empty() && s2c.is_empty() {
            break;
        }
    }
    let mut client_conn = client.into_connection().expect("client promote");
    let mut server_conn = server.into_connection().expect("server promote");
    let wire = client_conn.encrypt_app_data(b"hi").expect("encrypt");
    let _ = server_conn.decrypt_app_data(&wire).expect("decrypt");
    std::hint::black_box(client_conn);
    std::hint::black_box(server_conn);
}
