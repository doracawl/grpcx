//! Loopback TLS 1.3 handshake + app data round-trip.
//!
//! Mints a self-signed Ed25519 cert with `rcgen`, builds a client +
//! server config in the same process, then ping-pongs handshake bytes
//! between the two sides until both sides are out of the handshake.
//! Promotes each into a `TlsConnection` and exchanges one application-
//! data record per direction.

use grpcx_crypto::{ClientHandshake, ServerHandshake, TlsConfig, TlsError};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

struct CertKeyPem {
    cert_der: Vec<u8>,
    private_pkcs8_der: Vec<u8>,
}

fn mint_self_signed() -> CertKeyPem {
    let mut params =
        CertificateParams::new(vec![SERVER_NAME.to_string()]).expect("CertificateParams");
    params.distinguished_name = rcgen::DistinguishedName::new();
    let key_pair = KeyPair::generate_for(&PKCS_ED25519).expect("Ed25519 keypair");
    let cert = params.self_signed(&key_pair).expect("self_signed");
    CertKeyPem {
        cert_der: cert.der().to_vec(),
        private_pkcs8_der: key_pair.serialize_der(),
    }
}

fn pump_handshake(client: &mut ClientHandshake, server: &mut ServerHandshake) {
    // Drive until neither side has pending output AND both have moved
    // past the handshake. Bounded to avoid an infinite loop on a bug.
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
}

#[test]
fn loopback_handshake_and_app_data() {
    let CertKeyPem {
        cert_der,
        private_pkcs8_der,
    } = mint_self_signed();

    let mut server_cfg = TlsConfig::server(&[&cert_der], &private_pkcs8_der).expect("server cfg");
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg
        .add_root_cert(&cert_der)
        .expect("trust self-signed");
    client_cfg.set_alpn(&[b"h3"]);

    let mut client = ClientHandshake::new(&client_cfg, SERVER_NAME).expect("client handshake");
    let mut server = ServerHandshake::new(&server_cfg).expect("server handshake");
    assert!(!client.is_complete(), "fresh handshake reports complete");
    assert!(!server.is_complete(), "fresh handshake reports complete");

    pump_handshake(&mut client, &mut server);

    assert!(client.is_complete(), "pumped handshake reports incomplete");
    assert!(server.is_complete(), "pumped handshake reports incomplete");
    let mut client_conn = client.into_connection().expect("client promote");
    let mut server_conn = server.into_connection().expect("server promote");

    assert_eq!(client_conn.alpn_protocol().as_deref(), Some(b"h3".as_ref()));
    assert_eq!(server_conn.alpn_protocol().as_deref(), Some(b"h3".as_ref()));

    // Client → Server
    let req = b"GET / HTTP/3";
    let wire = client_conn.encrypt_app_data(req).expect("encrypt c2s");
    let got = server_conn.decrypt_app_data(&wire).expect("decrypt c2s");
    assert_eq!(got, req);

    // Server → Client
    let resp = b"HTTP/3 200 OK";
    let wire = server_conn.encrypt_app_data(resp).expect("encrypt s2c");
    let got = client_conn.decrypt_app_data(&wire).expect("decrypt s2c");
    assert_eq!(got, resp);
}

#[test]
fn loopback_handshake_no_alpn() {
    let CertKeyPem {
        cert_der,
        private_pkcs8_der,
    } = mint_self_signed();

    let server_cfg = TlsConfig::server(&[&cert_der], &private_pkcs8_der).expect("server cfg");
    let mut client_cfg = TlsConfig::client();
    client_cfg
        .add_root_cert(&cert_der)
        .expect("trust self-signed");

    let mut client = ClientHandshake::new(&client_cfg, SERVER_NAME).expect("client handshake");
    let mut server = ServerHandshake::new(&server_cfg).expect("server handshake");
    pump_handshake(&mut client, &mut server);

    let client_conn = client.into_connection().expect("client promote");
    let server_conn = server.into_connection().expect("server promote");
    assert!(client_conn.alpn_protocol().is_none());
    assert!(server_conn.alpn_protocol().is_none());
}

#[test]
fn untrusted_cert_fails() {
    let CertKeyPem {
        cert_der,
        private_pkcs8_der,
    } = mint_self_signed();
    let server_cfg = TlsConfig::server(&[&cert_der], &private_pkcs8_der).expect("server cfg");

    // Client without the self-signed cert in its trust store.
    let client_cfg = TlsConfig::client();
    let mut client = ClientHandshake::new(&client_cfg, SERVER_NAME).expect("client handshake");
    let mut server = ServerHandshake::new(&server_cfg).expect("server handshake");

    // Pump until the client errors out on the server cert.
    let mut err: Option<TlsError> = None;
    for _ in 0..32 {
        let c2s = client.take_handshake_bytes();
        if !c2s.is_empty()
            && let Err(e) = server.process_handshake_bytes(&c2s)
        {
            err = Some(e);
            break;
        }
        let s2c = server.take_handshake_bytes();
        if !s2c.is_empty()
            && let Err(e) = client.process_handshake_bytes(&s2c)
        {
            err = Some(e);
            break;
        }
        if c2s.is_empty() && s2c.is_empty() {
            break;
        }
    }
    assert!(
        matches!(err, Some(TlsError::BadCert)),
        "expected BadCert, got {err:?}"
    );
}

#[test]
fn key_update_smoke() {
    let CertKeyPem {
        cert_der,
        private_pkcs8_der,
    } = mint_self_signed();
    let server_cfg = TlsConfig::server(&[&cert_der], &private_pkcs8_der).expect("server cfg");
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).expect("trust");

    let mut client = ClientHandshake::new(&client_cfg, SERVER_NAME).expect("c");
    let mut server = ServerHandshake::new(&server_cfg).expect("s");
    pump_handshake(&mut client, &mut server);
    let mut client_conn = client.into_connection().expect("c promote");
    let mut server_conn = server.into_connection().expect("s promote");

    // App data before key update.
    let wire = client_conn.encrypt_app_data(b"before").expect("e");
    assert_eq!(server_conn.decrypt_app_data(&wire).expect("d"), b"before");

    // Key update on the client. Drain the resulting wire bytes to the
    // server so it processes the KeyUpdate message; rustls returns the
    // updated traffic-secret state via `process_new_packets`.
    client_conn.key_update().expect("key_update");
    let ku_wire = client_conn.encrypt_app_data(b"after").expect("e");
    let plaintext = server_conn.decrypt_app_data(&ku_wire).expect("d");
    assert_eq!(plaintext, b"after");
}

#[test]
fn large_app_data_crosses_buffer_limit() {
    // Regression (1.3.1, found by the tonic-TLS interop
    // gate): rustls caps both plaintext buffers at 64 KiB
    // by default. `encrypt_app_data` of a >=64 KiB chunk
    // used to fail (write_all with no interleaved wire
    // drain), and `decrypt_app_data` of the resulting
    // burst used to silently DROP the part the deframer
    // couldn't take in one feed. 256 KiB each way proves
    // the interleaved-drain paths.
    let CertKeyPem {
        cert_der,
        private_pkcs8_der,
    } = mint_self_signed();

    let mut server_cfg = TlsConfig::server(&[&cert_der], &private_pkcs8_der).expect("server cfg");
    server_cfg.set_alpn(&[b"h2"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg
        .add_root_cert(&cert_der)
        .expect("trust self-signed");
    client_cfg.set_alpn(&[b"h2"]);

    let mut client = ClientHandshake::new(&client_cfg, SERVER_NAME).expect("client handshake");
    let mut server = ServerHandshake::new(&server_cfg).expect("server handshake");
    pump_handshake(&mut client, &mut server);
    let mut client_conn = client.into_connection().expect("client promote");
    let mut server_conn = server.into_connection().expect("server promote");

    let big: Vec<u8> = (0..262_144u32).map(|i| (i % 251) as u8).collect();

    let wire = client_conn.encrypt_app_data(&big).expect("encrypt big c2s");
    let got = server_conn
        .decrypt_app_data(&wire)
        .expect("decrypt big c2s");
    assert_eq!(got, big, "c2s 256 KiB round trip");

    let wire = server_conn.encrypt_app_data(&big).expect("encrypt big s2c");
    let got = client_conn
        .decrypt_app_data(&wire)
        .expect("decrypt big s2c");
    assert_eq!(got, big, "s2c 256 KiB round trip");
}
