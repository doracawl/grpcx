//! L5 1.1.0 integ — upper-layer raw-stream surface for
//! WebTransport (RFC 9220) and similar extensions. Validates:
//!
//! - Peer-initiated reserved-typed uni streams reach the upper
//!   layer via [`accept_unknown_uni_stream`] with their type code
//!   intact, and post-prefix bytes are recoverable via
//!   [`read_stream_raw`].
//! - Peer-initiated bidi streams can be claimed via
//!   [`accept_unknown_bi_stream`] and disappear from L5's request
//!   accept queue + frame-decode path.
//! - Locally-opened raw uni / bi streams behave as L4 streams.
//! - Coexistence: a claimed WT bidi sits side-by-side with a
//!   normal H/3 request bidi without interference.
//!
//! [`accept_unknown_uni_stream`]: grpcx_h3::Connection::accept_unknown_uni_stream
//! [`accept_unknown_bi_stream`]: grpcx_h3::Connection::accept_unknown_bi_stream
//! [`read_stream_raw`]: grpcx_h3::Connection::read_stream_raw

use grpcx_crypto::TlsConfig;
use grpcx_h3::{Connection as H3Connection, Header, Settings};
use grpcx_quic::QuicConnection;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

// WebTransport stream-type code (RFC 9220 §4) — the upper-layer
// canary value v0.4 callers look for.
const WT_STREAM_TYPE: u64 = 0x54;

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    (cert.der().to_vec(), kp.serialize_der())
}

fn h(name: &[u8], value: &[u8]) -> Header {
    Header {
        name: name.to_vec(),
        value: value.to_vec(),
    }
}

fn pump_pair(
    a: &mut H3Connection,
    a_in: &[Vec<u8>],
    b: &mut H3Connection,
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

fn drive_until(
    client: &mut H3Connection,
    server: &mut H3Connection,
    mut done: impl FnMut(&H3Connection, &H3Connection) -> bool,
    max_rounds: usize,
) {
    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..max_rounds {
        if done(client, server) {
            return;
        }
        let (a, b) = pump_pair(client, &to_client, server, &to_server);
        to_server = a;
        to_client = b;
        if to_server.is_empty() && to_client.is_empty() && done(client, server) {
            return;
        }
    }
}

fn enc_varint(v: u64, out: &mut Vec<u8>) {
    // RFC 9000 §16 — 2-bit length prefix selects 1/2/4/8 byte form.
    if v < 1 << 6 {
        out.push(v as u8);
    } else if v < 1 << 14 {
        out.push(0x40 | ((v >> 8) as u8));
        out.push(v as u8);
    } else if v < 1 << 30 {
        out.push(0x80 | ((v >> 24) as u8));
        out.push((v >> 16) as u8);
        out.push((v >> 8) as u8);
        out.push(v as u8);
    } else {
        out.push(0xc0 | ((v >> 56) as u8));
        out.push((v >> 48) as u8);
        out.push((v >> 40) as u8);
        out.push((v >> 32) as u8);
        out.push((v >> 24) as u8);
        out.push((v >> 16) as u8);
        out.push((v >> 8) as u8);
        out.push(v as u8);
    }
}

fn varint_bytes(v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    enc_varint(v, &mut out);
    out
}

/// Build a fresh client + server H/3 pair, drive them to ready.
/// Returns the established pair.
fn established_pair() -> (H3Connection, H3Connection) {
    let (cert_der, key_der) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);

    let client_quic =
        QuicConnection::new_client(&client_cfg, SERVER_NAME, b"client-tp".to_vec()).unwrap();
    let server_quic = QuicConnection::new_server(&server_cfg, b"server-tp".to_vec()).unwrap();

    let mut client = H3Connection::new_client(client_quic, Settings::default()).unwrap();
    let mut server = H3Connection::new_server(server_quic, Settings::default()).unwrap();

    drive_until(
        &mut client,
        &mut server,
        |c, s| c.is_ready() && s.is_ready(),
        24,
    );
    assert!(client.is_ready() && server.is_ready());
    (client, server)
}

#[test]
fn reserved_uni_reaches_upper_layer_with_type_code() {
    let (mut client, mut server) = established_pair();

    // Client opens a raw uni and writes [0x54][session_id=7][payload].
    let sid = client.open_uni_raw();
    let mut wire = Vec::new();
    enc_varint(WT_STREAM_TYPE, &mut wire);
    enc_varint(7, &mut wire);
    wire.extend_from_slice(b"hello");
    client.write_stream_raw(sid, &wire, true).unwrap();

    drive_until(
        &mut client,
        &mut server,
        |_, s| {
            // We can't peek private state from a test; use a
            // probe round-trip via the public API: pumping until
            // either side has flushed data and the server has
            // bytes to claim.
            // Side-step: rely on bounded max_rounds.
            let _ = s;
            false
        },
        12,
    );

    let (claim_sid, type_code) = server
        .accept_unknown_uni_stream()
        .expect("server must surface the reserved-typed uni");
    assert_eq!(type_code, WT_STREAM_TYPE);

    let mut buf = Vec::new();
    server.read_stream_raw(claim_sid, &mut buf);
    // Post-prefix bytes: session_id varint (`7`, single byte) +
    // b"hello".
    let mut want = varint_bytes(7);
    want.extend_from_slice(b"hello");
    assert_eq!(buf, want);
}

#[test]
fn unknown_bi_claim_skips_h3_request_path() {
    let (mut client, mut server) = established_pair();

    // Client opens a raw bidi tagged [0x54][session_id=3][payload].
    let sid = client.open_bi_raw();
    let mut wire = Vec::new();
    enc_varint(WT_STREAM_TYPE, &mut wire);
    enc_varint(3, &mut wire);
    wire.extend_from_slice(b"ping");
    client.write_stream_raw(sid, &wire, false).unwrap();

    drive_until(&mut client, &mut server, |_, _| false, 8);

    let claim_sid = server
        .accept_unknown_bi_stream()
        .expect("server must surface the bidi for claim");

    // Once claimed, accept_request must NOT return it.
    assert!(
        server.accept_request().unwrap().is_none(),
        "claimed bidi must not appear as an H/3 request"
    );

    let mut buf = Vec::new();
    server.read_stream_raw(claim_sid, &mut buf);
    let mut want = varint_bytes(WT_STREAM_TYPE);
    want.extend(varint_bytes(3));
    want.extend_from_slice(b"ping");
    assert_eq!(buf, want);

    // Server writes back via the raw bidi; client picks up the
    // bytes via the same raw path (the client's bidi is locally-
    // opened so it's already in `wt_owned_sids` from open_bi_raw).
    server.write_stream_raw(claim_sid, b"pong", true).unwrap();
    drive_until(&mut client, &mut server, |_, _| false, 8);
    let mut got = Vec::new();
    let (n, fin) = client.read_stream_raw(sid, &mut got);
    assert_eq!(&got[..n], b"pong");
    assert!(fin, "server FIN should propagate");
}

#[test]
fn multiple_reserved_uni_arrive_in_order() {
    let (mut client, mut server) = established_pair();

    let s1 = client.open_uni_raw();
    let s2 = client.open_uni_raw();
    let mut w1 = varint_bytes(WT_STREAM_TYPE);
    w1.push(1);
    w1.push(b'a');
    let mut w2 = varint_bytes(WT_STREAM_TYPE);
    w2.push(2);
    w2.push(b'b');
    client.write_stream_raw(s1, &w1, true).unwrap();
    client.write_stream_raw(s2, &w2, true).unwrap();

    drive_until(&mut client, &mut server, |_, _| false, 12);

    let (sid1, t1) = server
        .accept_unknown_uni_stream()
        .expect("first reserved uni");
    let (sid2, t2) = server
        .accept_unknown_uni_stream()
        .expect("second reserved uni");
    assert!(server.accept_unknown_uni_stream().is_none(), "no third");
    assert_eq!(t1, WT_STREAM_TYPE);
    assert_eq!(t2, WT_STREAM_TYPE);

    let mut got1 = Vec::new();
    let mut got2 = Vec::new();
    server.read_stream_raw(sid1, &mut got1);
    server.read_stream_raw(sid2, &mut got2);
    assert_eq!(got1, [1, b'a']);
    assert_eq!(got2, [2, b'b']);
}

#[test]
fn claimed_wt_bidi_coexists_with_h3_request() {
    let (mut client, mut server) = established_pair();

    // Open WT bidi FIRST so it arrives at the server's accept_bi
    // queue before the H/3 request — `accept_unknown_bi_stream`
    // and `accept_request` share one FIFO queue; the caller is
    // expected to invoke the WT-claim path first when a WT bidi
    // is expected. Documented contract.
    let wt_sid = client.open_bi_raw();
    let mut wt_wire = varint_bytes(WT_STREAM_TYPE);
    wt_wire.push(11);
    wt_wire.push(b'x');
    client.write_stream_raw(wt_sid, &wt_wire, true).unwrap();

    let req_headers = vec![
        h(b":method", b"GET"),
        h(b":scheme", b"https"),
        h(b":path", b"/"),
        h(b":authority", b"loopback.example"),
    ];
    let req = client.send_request(&req_headers).unwrap();
    client.finish(&req).unwrap();

    drive_until(
        &mut client,
        &mut server,
        |_, s| s.pending_request_count() >= 2,
        16,
    );

    // Server claims the WT bidi first (it arrived first), then
    // accept_request picks up the H/3 request.
    let claimed = server
        .accept_unknown_bi_stream()
        .expect("server must surface the WT bidi");
    let req_server = server
        .accept_request()
        .unwrap()
        .expect("real H/3 request bidi must survive");
    assert_ne!(req_server.stream_id().0, claimed.0);
    assert!(server.accept_unknown_bi_stream().is_none());
    assert!(server.accept_request().unwrap().is_none());

    // The claimed bidi's bytes are recoverable verbatim.
    let mut got = Vec::new();
    server.read_stream_raw(claimed, &mut got);
    let mut want = varint_bytes(WT_STREAM_TYPE);
    want.push(11);
    want.push(b'x');
    assert_eq!(got, want);

    // Pump until the request's HEADERS arrive on the server.
    drive_until(
        &mut client,
        &mut server,
        |_, s| s.received_headers_ready(&req_server),
        8,
    );
    let decoded = server.read_headers(&req_server).unwrap().unwrap();
    assert_eq!(decoded, req_headers);
}
