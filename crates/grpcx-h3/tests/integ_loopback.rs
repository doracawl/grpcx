//! L5 v0.5 acceptance — intra-process HTTP/3 round-trip.
//!
//! Drives a client + server pair through:
//! 1. QUIC handshake to 1-RTT
//! 2. H/3 control + QPACK uni-stream opens + SETTINGS exchange
//! 3. Client sends a GET `/index` request (HEADERS frame only,
//!    FIN after HEADERS)
//! 4. Server reads the request headers, replies with `200 OK` +
//!    `text/html` body + FIN
//! 5. Client reads the response headers + body
//! 6. Server emits GOAWAY; client observes it on its control
//!    stream.

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

#[test]
fn h3_loopback_get_round_trip() {
    let (cert_der, key_der) = mint_cert();

    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);

    let client_quic =
        QuicConnection::new_client(&client_cfg, SERVER_NAME, b"client-tp".to_vec()).unwrap();
    let server_quic = QuicConnection::new_server(&server_cfg, b"server-tp".to_vec()).unwrap();

    let client_settings = Settings {
        max_field_section_size: Some(16 * 1024),
        ..Settings::default()
    };
    let server_settings = Settings {
        max_field_section_size: Some(64 * 1024),
        ..Settings::default()
    };

    let mut client = H3Connection::new_client(client_quic, client_settings).unwrap();
    let mut server = H3Connection::new_server(server_quic, server_settings).unwrap();

    // Drive QUIC handshake + H/3 SETTINGS exchange.
    drive_until(
        &mut client,
        &mut server,
        |c, s| c.is_ready() && s.is_ready(),
        24,
    );
    assert!(client.is_quic_established(), "client must reach 1-RTT");
    assert!(server.is_quic_established(), "server must reach 1-RTT");
    assert!(client.is_ready(), "client must see server's SETTINGS");
    assert!(server.is_ready(), "server must see client's SETTINGS");

    // Check peer SETTINGS landed.
    assert_eq!(
        client
            .peer_settings()
            .and_then(|s| s.max_field_section_size),
        Some(64 * 1024)
    );
    assert_eq!(
        server
            .peer_settings()
            .and_then(|s| s.max_field_section_size),
        Some(16 * 1024)
    );

    // Client → Server: send the GET.
    let request_headers = vec![
        h(b":method", b"GET"),
        h(b":scheme", b"https"),
        h(b":path", b"/index"),
        h(b":authority", b"loopback.example"),
    ];
    let req_stream = client.send_request(&request_headers).unwrap();
    client.finish(&req_stream).unwrap();

    // Pump until the server has the new request stream in its
    // accept queue.
    drive_until(
        &mut client,
        &mut server,
        |_, s| s.pending_request_count() > 0,
        24,
    );

    // Server: accept the request, then pump until HEADERS is
    // parsed.
    let server_stream = server
        .accept_request()
        .unwrap()
        .expect("server must have one pending request");
    drive_until(
        &mut client,
        &mut server,
        |_, s| s.received_headers_ready(&server_stream),
        8,
    );
    let received_request = server
        .read_headers(&server_stream)
        .unwrap()
        .expect("server must see decoded request headers");
    assert_eq!(received_request, request_headers);

    // Server → Client: respond.
    let response_headers = vec![
        h(b":status", b"200"),
        h(b"content-type", b"text/html; charset=utf-8"),
    ];
    server
        .write_headers(&server_stream, &response_headers)
        .unwrap();
    server
        .write_data(&server_stream, b"<!doctype html>OK")
        .unwrap();
    server.finish(&server_stream).unwrap();

    // Pump until the client has the response headers + full body
    // (FIN included).
    drive_until(
        &mut client,
        &mut server,
        |c, _| {
            c.received_headers_ready(&req_stream) && {
                let (_, fin) = c.data_buffer_status(&req_stream);
                fin
            }
        },
        16,
    );

    let received_response = client
        .read_headers(&req_stream)
        .unwrap()
        .expect("client must see response headers");
    assert_eq!(received_response, response_headers);

    let mut body = Vec::new();
    let (n, fin) = client.read_data(&req_stream, &mut body).unwrap();
    assert_eq!(n, body.len());
    assert_eq!(&body[..], b"<!doctype html>OK");
    assert!(fin, "client must see FIN on the response stream");

    // Server emits GOAWAY → client sees it on the control stream.
    server.goaway().unwrap();
    drive_until(
        &mut client,
        &mut server,
        |c, _| c.peer_goaway().is_some(),
        8,
    );
    // The highest accepted request stream id was the one we just
    // served. For the very first client-initiated bidi the id is 0.
    assert_eq!(client.peer_goaway(), Some(0));
}
