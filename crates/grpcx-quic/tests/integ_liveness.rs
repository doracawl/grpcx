//! Link-liveness acceptance (1.8.0, synthetic clock): the
//! liveness atom lives INSIDE the transport — keepalive
//! PINGs keep a quiet-but-alive link up; a link with no
//! inbound traffic past the idle timeout dies with
//! `Error::Timeout`; a handshake against a dead peer is
//! bounded by the same timeout.

use grpcx_crypto::TlsConfig;
use grpcx_quic::QuicConnection;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";
const KEEPALIVE_US: u64 = 50_000;
const IDLE_TIMEOUT_US: u64 = 200_000;

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).expect("CertificateParams");
    let kp = KeyPair::generate_for(&PKCS_ED25519).expect("kp");
    let cert = params.self_signed(&kp).expect("self_signed");
    (cert.der().to_vec(), kp.serialize_der())
}

fn fresh_pair() -> (QuicConnection, QuicConnection) {
    let (cert, key) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert], &key).expect("server cfg");
    server_cfg.set_alpn(&[b"hq"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert).expect("root");
    client_cfg.set_alpn(&[b"hq"]);
    let client =
        QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).expect("client");
    let server = QuicConnection::new_server(&server_cfg, b"s".to_vec()).expect("server");
    (client, server)
}

fn established_pair() -> (QuicConnection, QuicConnection, u64) {
    let (mut client, mut server) = fresh_pair();
    let mut clock = 0u64;
    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..64 {
        let mut next_server: Vec<Vec<u8>> = Vec::new();
        let mut next_client: Vec<Vec<u8>> = Vec::new();
        for dg in &to_client {
            next_server.extend(client.pump_at(Some(dg), false, clock).expect("client"));
        }
        next_server.extend(client.pump_at(None, true, clock).expect("client idle"));
        for dg in &to_server {
            next_client.extend(server.pump_at(Some(dg), false, clock).expect("server"));
        }
        next_client.extend(server.pump_at(None, true, clock).expect("server idle"));
        to_server = next_server;
        to_client = next_client;
        clock += 500;
        if client.is_established()
            && server.is_established()
            && to_server.is_empty()
            && to_client.is_empty()
        {
            break;
        }
    }
    assert!(client.is_established() && server.is_established());
    (client, server, clock)
}

#[test]
fn quiet_link_stays_up_via_keepalive() {
    let (mut client, mut server, mut clock) = established_pair();
    client.set_keepalive_us(KEEPALIVE_US);
    client.set_idle_timeout_us(IDLE_TIMEOUT_US);
    server.set_keepalive_us(KEEPALIVE_US);
    server.set_idle_timeout_us(IDLE_TIMEOUT_US);

    // Idle for 20 × the keepalive interval (5 × the idle
    // timeout) with the link DELIVERING — pings must flow
    // and neither side may die.
    let mut ping_datagrams = 0usize;
    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..1_000 {
        clock += 1_000;
        let mut next_server: Vec<Vec<u8>> = Vec::new();
        let mut next_client: Vec<Vec<u8>> = Vec::new();
        for dg in &to_client {
            next_server.extend(
                client
                    .pump_at(Some(dg), false, clock)
                    .expect("alive client"),
            );
        }
        next_server.extend(
            client
                .pump_at(None, true, clock)
                .expect("alive client idle"),
        );
        for dg in &to_server {
            next_client.extend(
                server
                    .pump_at(Some(dg), false, clock)
                    .expect("alive server"),
            );
        }
        next_client.extend(
            server
                .pump_at(None, true, clock)
                .expect("alive server idle"),
        );
        ping_datagrams += next_server.len() + next_client.len();
        to_server = next_server;
        to_client = next_client;
    }
    assert!(
        ping_datagrams > 0,
        "keepalive pings must have crossed the quiet link"
    );

    // The link is still usable.
    let sid = client.open_bi();
    let data: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
    client.write_stream(sid, &data, true).expect("write");
    for _ in 0..64 {
        clock += 500;
        let mut next_server: Vec<Vec<u8>> = Vec::new();
        let mut next_client: Vec<Vec<u8>> = Vec::new();
        for dg in &to_client {
            next_server.extend(client.pump_at(Some(dg), false, clock).expect("client"));
        }
        next_server.extend(client.pump_at(None, true, clock).expect("client idle"));
        for dg in &to_server {
            next_client.extend(server.pump_at(Some(dg), false, clock).expect("server"));
        }
        next_client.extend(server.pump_at(None, true, clock).expect("server idle"));
        to_server = next_server;
        to_client = next_client;
        let mut probe = Vec::new();
        if server.peek_stream(sid, &mut probe).1 {
            break;
        }
    }
    let mut got = Vec::new();
    let (_, fin) = server.read_stream(sid, &mut got);
    assert!(fin, "post-idle round trip must complete");
    assert_eq!(got, data);
}

#[test]
fn blackholed_link_dies_within_idle_timeout() {
    let (mut client, _server, mut clock) = established_pair();
    client.set_keepalive_us(KEEPALIVE_US);
    client.set_idle_timeout_us(IDLE_TIMEOUT_US);

    // Nothing inbound ever again; the client keeps pumping
    // on its drive cadence. It must die with Timeout, and
    // within the idle window (+ one pump slice of slack).
    let t0 = clock;
    let death = loop {
        clock += 1_000;
        match client.pump_at(None, true, clock) {
            Ok(_) => {
                assert!(
                    clock - t0 <= IDLE_TIMEOUT_US + 2_000,
                    "link must be declared dead within the idle timeout"
                );
            }
            Err(e) => break e,
        }
    };
    assert_eq!(death, grpcx_quic::Error::Timeout);
}

#[test]
fn handshake_against_dead_peer_is_bounded() {
    let (mut client, _server) = fresh_pair();
    client.set_idle_timeout_us(IDLE_TIMEOUT_US);

    let mut clock = 0u64;
    let death = loop {
        clock += 1_000;
        match client.pump_at(None, true, clock) {
            Ok(_) => {
                assert!(
                    clock <= IDLE_TIMEOUT_US + 2_000,
                    "dead-server handshake must be bounded by the idle timeout"
                );
            }
            Err(e) => break e,
        }
    };
    assert_eq!(death, grpcx_quic::Error::Timeout);
}
