//! Eager cancel acceptance (1.9): RESET_STREAM /
//! STOP_SENDING round trips on an intra-process loopback —
//! both directions die, the peer learns, late data is
//! dropped, and the CONNECTION survives for the next call.

use grpcx_crypto::TlsConfig;
use grpcx_quic::{QuicConnection, StreamId};
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

/// Drive both ends, intact delivery, until quiescent.
fn drive(client: &mut QuicConnection, server: &mut QuicConnection) {
    let mut to_server: Vec<Vec<u8>> = client.pump(None).expect("client idle");
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..32 {
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
        if to_server.is_empty() && to_client.is_empty() {
            break;
        }
    }
}

#[test]
fn cancel_resets_both_directions_and_conn_survives() {
    let (mut client, mut server) = established_pair();

    // A mid-flight exchange: client request partly on the
    // wire, server response queued.
    let sid = client.open_bi();
    client
        .write_stream(sid, &vec![0xAB; 4096], false)
        .expect("write");
    drive(&mut client, &mut server);
    server
        .write_stream(sid, &vec![0xCD; 4096], false)
        .expect("server write");

    // Client cancels — RESET_STREAM (our half) +
    // STOP_SENDING (their half) cross the wire.
    client.cancel_stream(sid, 0x10c);
    drive(&mut client, &mut server);

    // Server learned: its per-stream life ended (reset
    // event surfaced) and its recv state for the sid is
    // gone / inert.
    let server_resets = server.take_reset_streams();
    assert!(
        server_resets.contains(&sid.0),
        "server must surface the reset (got {server_resets:?})"
    );
    // Client got the answering RESET_STREAM for the
    // server's half (RFC 9000 §3.5 STOP_SENDING answer).
    let client_resets = client.take_reset_streams();
    assert!(
        client_resets.contains(&sid.0),
        "client must surface the peer reset (got {client_resets:?})"
    );
    // Late server data for the cancelled sid is dropped on
    // the client, not re-registered.
    let mut out = Vec::new();
    let (n, _) = client.read_stream(sid, &mut out);
    assert_eq!(n, 0, "cancelled stream must not deliver data");

    // The CONNECTION survives: a fresh stream round-trips
    // byte-exact.
    let sid2 = client.open_bi();
    let payload = b"after-cancel".to_vec();
    client.write_stream(sid2, &payload, true).expect("write2");
    drive(&mut client, &mut server);
    let mut got = Vec::new();
    let (n, fin) = server.read_stream(sid2, &mut got);
    assert_eq!((n, fin), (payload.len(), true));
    assert_eq!(got, payload);
}

#[test]
fn stop_sending_alone_kills_the_send_half() {
    let (mut client, mut server) = established_pair();

    let sid = client.open_bi();
    client.write_stream(sid, b"req", true).expect("write");
    drive(&mut client, &mut server);

    // Server starts answering, then the CLIENT no longer
    // wants it: server receives STOP_SENDING, must stop
    // and answer RESET_STREAM.
    server
        .write_stream(sid, &vec![0xEE; 2048], false)
        .expect("server write");
    client.cancel_stream(sid, 0x10c);
    drive(&mut client, &mut server);

    // Server's send side died: no STREAM frames flow even
    // if it keeps "writing" (write_stream to a reset
    // stream buffers but the builder skips it — the entry
    // is reset). Observable: client receives nothing new
    // and both sides are quiescent (drive returned).
    let mut out = Vec::new();
    let (n, _) = client.read_stream(sid, &mut out);
    assert_eq!(n, 0, "no data after STOP_SENDING took effect");
}

#[test]
fn cancel_unknown_stream_is_a_noop() {
    let (mut client, mut server) = established_pair();
    client.cancel_stream(StreamId(404), 0x10c);
    drive(&mut client, &mut server);
    assert!(client.take_reset_streams().is_empty());
    assert!(server.take_reset_streams().is_empty());
}

#[test]
fn lost_cancel_frames_are_retransmitted_via_pto() {
    // 1.9.1: RESET_STREAM/STOP_SENDING ride the loss
    // recovery machinery (closing the 1.9.0 recorded
    // departure from RFC 9000 §13.3) — a dropped cancel
    // burst is re-sent on PTO and the peer still learns.
    let (mut client, mut server) = established_pair();
    let mut clock: u64 = 1;

    let sid = client.open_bi();
    client.write_stream(sid, b"req", true).expect("write");
    // Deliver the request normally (timed pumps).
    let mut to_server = client.pump_at(None, true, clock).expect("c");
    for _ in 0..8 {
        let mut to_client = Vec::new();
        for dg in &to_server {
            to_client.extend(server.pump_at(Some(dg), false, clock).expect("s"));
        }
        to_client.extend(server.pump_at(None, true, clock).expect("s idle"));
        to_server = Vec::new();
        for dg in &to_client {
            to_server.extend(client.pump_at(Some(dg), false, clock).expect("c"));
        }
        to_server.extend(client.pump_at(None, true, clock).expect("c idle"));
        if to_server.is_empty() && to_client.is_empty() {
            break;
        }
    }

    // Cancel — and EAT the burst carrying the frames.
    client.cancel_stream(sid, 0x10c);
    let eaten = client.pump_at(None, true, clock).expect("cancel burst");
    assert!(!eaten.is_empty(), "cancel must emit datagrams to lose");
    drop(eaten);
    assert!(
        server.take_reset_streams().is_empty(),
        "server must not have learned yet (burst was dropped)"
    );

    // PTO-drive: jump the clock until the re-sent cancel
    // lands; deliver everything from here on.
    let mut learned = false;
    for _ in 0..12 {
        clock += 100_000; // +100 ms
        let to_server = client.pump_at(None, true, clock).expect("c probe");
        let mut to_client = Vec::new();
        for dg in &to_server {
            to_client.extend(server.pump_at(Some(dg), false, clock).expect("s"));
        }
        to_client.extend(server.pump_at(None, true, clock).expect("s idle"));
        for dg in &to_client {
            let _ = client.pump_at(Some(dg), false, clock).expect("c ack");
        }
        if server.take_reset_streams().contains(&sid.0) {
            learned = true;
            break;
        }
    }
    assert!(
        learned,
        "the re-transmitted cancel must reach the server via PTO"
    );
}
