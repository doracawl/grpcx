//! Reliability phase 1 acceptance: 1-RTT loss recovery on
//! an intra-process loopback with deterministic datagram
//! loss, duplication, and reordering. Loss is injected
//! only into established-phase (short-header) packets and
//! never into a flight's tail — tail-loss recovery needs
//! the PTO timer (phase 2); packet-threshold detection
//! requires a later packet's ACK to fire.

use grpcx_crypto::TlsConfig;
use grpcx_quic::QuicConnection;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).expect("CertificateParams");
    let kp = KeyPair::generate_for(&PKCS_ED25519).expect("kp");
    let cert = params.self_signed(&kp).expect("self_signed");
    (cert.der().to_vec(), kp.serialize_der())
}

fn established_pair() -> (QuicConnection, QuicConnection) {
    let (cert, key) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert], &key).expect("server cfg");
    server_cfg.set_alpn(&[b"hq"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert).expect("root");
    client_cfg.set_alpn(&[b"hq"]);

    let mut client =
        QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).expect("client");
    let mut server = QuicConnection::new_server(&server_cfg, b"s".to_vec()).expect("server");

    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..16 {
        let mut next_server: Vec<Vec<u8>> = Vec::new();
        let mut next_client: Vec<Vec<u8>> = Vec::new();
        for dg in &to_client {
            next_server.extend(client.pump(Some(dg)).expect("client pump"));
        }
        next_server.extend(client.pump(None).expect("client idle"));
        for dg in &to_server {
            next_client.extend(server.pump(Some(dg)).expect("server pump"));
        }
        next_client.extend(server.pump(None).expect("server idle"));
        to_server = next_server;
        to_client = next_client;
        if client.is_established()
            && server.is_established()
            && to_server.is_empty()
            && to_client.is_empty()
        {
            break;
        }
    }
    assert!(client.is_established() && server.is_established());
    (client, server)
}

fn is_short_header(dg: &[u8]) -> bool {
    !dg.is_empty() && dg[0] & 0x80 == 0
}

/// Drive both ends until `done`, mangling the
/// client→server direction with `mangle` (takes the
/// in-flight burst, returns what is actually delivered).
/// The server→client direction is delivered intact.
fn drive_with<F>(
    client: &mut QuicConnection,
    server: &mut QuicConnection,
    mut mangle: F,
    mut done: impl FnMut(&QuicConnection, &QuicConnection) -> bool,
) where
    F: FnMut(Vec<Vec<u8>>) -> Vec<Vec<u8>>,
{
    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..64 {
        if done(client, server) {
            return;
        }
        let mut next_server: Vec<Vec<u8>> = Vec::new();
        let mut next_client: Vec<Vec<u8>> = Vec::new();
        for dg in &to_client {
            next_server.extend(client.pump(Some(dg)).expect("client pump"));
        }
        next_server.extend(client.pump(None).expect("client idle"));
        for dg in &to_server {
            next_client.extend(server.pump(Some(dg)).expect("server pump"));
        }
        next_client.extend(server.pump(None).expect("server idle"));
        to_server = mangle(next_server);
        to_client = next_client;
        if to_server.is_empty() && to_client.is_empty() && done(client, server) {
            return;
        }
    }
    panic!("drive_with did not converge");
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

#[test]
fn lost_middle_packets_are_retransmitted() {
    let (mut client, mut server) = established_pair();
    let sid = client.open_bi();
    // 32 KiB at the 1350 default budget = a ~25-packet
    // flight in the first burst.
    let data = payload(32 * 1024);
    client.write_stream(sid, &data, true).expect("write");

    // Drop short-header packets 2, 5, and 11 of the
    // client→server direction (counted across the run) —
    // never the tail: later packets of the same burst
    // carry the ACK trigger.
    let mut seen = 0u32;
    let drop_at = [2u32, 5, 11];
    drive_with(
        &mut client,
        &mut server,
        |burst| {
            burst
                .into_iter()
                .filter(|dg| {
                    if !is_short_header(dg) {
                        return true;
                    }
                    seen += 1;
                    !drop_at.contains(&seen)
                })
                .collect()
        },
        |_c, s| {
            let mut probe = Vec::new();
            let (_, fin) = s.peek_stream(sid, &mut probe);
            fin
        },
    );

    let mut got = Vec::new();
    let (n, fin) = server.read_stream(sid, &mut got);
    assert!(fin, "server should see FIN after recovery");
    assert_eq!(n, data.len());
    assert_eq!(got, data, "recovered byte stream must be exact");
}

#[test]
fn duplicated_packets_are_deduplicated() {
    let (mut client, mut server) = established_pair();
    let sid = client.open_bi();
    let data = payload(8 * 1024);
    client.write_stream(sid, &data, true).expect("write");

    // Deliver every short-header packet twice.
    drive_with(
        &mut client,
        &mut server,
        |burst| {
            let mut out = Vec::new();
            for dg in burst {
                let dup = is_short_header(&dg);
                out.push(dg.clone());
                if dup {
                    out.push(dg);
                }
            }
            out
        },
        |_c, s| {
            let mut probe = Vec::new();
            let (_, fin) = s.peek_stream(sid, &mut probe);
            fin
        },
    );

    let mut got = Vec::new();
    let (n, fin) = server.read_stream(sid, &mut got);
    assert!(fin);
    assert_eq!(n, data.len(), "duplicates must not inflate the stream");
    assert_eq!(got, data);
}

#[test]
fn reordered_packets_reassemble_in_order() {
    let (mut client, mut server) = established_pair();
    let sid = client.open_bi();
    let data = payload(16 * 1024);
    client.write_stream(sid, &data, true).expect("write");

    // Reverse each burst's short-header packets — maximal
    // reordering within a flight.
    drive_with(
        &mut client,
        &mut server,
        |burst| {
            let (mut short, long): (Vec<_>, Vec<_>) =
                burst.into_iter().partition(|dg| is_short_header(dg));
            short.reverse();
            let mut out = long;
            out.extend(short);
            out
        },
        |_c, s| {
            let mut probe = Vec::new();
            let (_, fin) = s.peek_stream(sid, &mut probe);
            fin
        },
    );

    let mut got = Vec::new();
    let (n, fin) = server.read_stream(sid, &mut got);
    assert!(fin);
    assert_eq!(n, data.len());
    assert_eq!(got, data, "reordered flight must reassemble exactly");
}

#[test]
fn lossy_round_trip_with_response() {
    let (mut client, mut server) = established_pair();
    let sid = client.open_bi();
    let request = payload(12 * 1024);
    client.write_stream(sid, &request, true).expect("write");

    // Request direction loses its 3rd short packet.
    let mut seen = 0u32;
    drive_with(
        &mut client,
        &mut server,
        |burst| {
            burst
                .into_iter()
                .filter(|dg| {
                    if !is_short_header(dg) {
                        return true;
                    }
                    seen += 1;
                    seen != 3
                })
                .collect()
        },
        |_c, s| {
            let mut probe = Vec::new();
            let (_, fin) = s.peek_stream(sid, &mut probe);
            fin
        },
    );
    let mut got = Vec::new();
    let (_, fin) = server.read_stream(sid, &mut got);
    assert!(fin);
    assert_eq!(got, request);

    // Server answers; response direction delivered intact
    // (its loss path is symmetric — same machinery).
    let response = payload(20 * 1024);
    server.write_stream(sid, &response, true).expect("respond");
    drive_with(
        &mut client,
        &mut server,
        |burst| burst,
        |c, _s| {
            let mut probe = Vec::new();
            let (_, fin) = c.peek_stream(sid, &mut probe);
            fin
        },
    );
    let mut got = Vec::new();
    let (_, fin) = client.read_stream(sid, &mut got);
    assert!(fin);
    assert_eq!(got, response);
}
