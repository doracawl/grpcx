//! L5 v1.3 — trailing HEADERS (the gRPC shape): a request
//! stream carrying `HEADERS DATA HEADERS(fin)` in the
//! response direction surfaces the trailing section via
//! `read_trailers`, and a third HEADERS frame is dropped.

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

fn establish() -> (H3Connection, H3Connection) {
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
    // Flush any residual datagrams.
    for _ in 0..4 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
    }
    (client, server)
}

#[test]
fn response_trailers_round_trip() {
    let (mut client, mut server) = establish();
    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();

    // Client request: HEADERS + fin.
    let req = client
        .send_request(&[
            h(b":method", b"POST"),
            h(b":scheme", b"https"),
            h(b":path", b"/svc/Method"),
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

    // Server response: HEADERS + DATA + trailing HEADERS + fin
    // — the gRPC shape.
    server
        .write_headers(&srv_req, &[h(b":status", b"200")])
        .unwrap();
    server.write_data(&srv_req, b"payload").unwrap();
    server
        .write_headers(&srv_req, &[h(b"grpc-status", b"0"), h(b"x-trail", b"yes")])
        .unwrap();
    server.finish(&srv_req).unwrap();

    // Client drains: leading headers, body, trailers.
    for _ in 0..16 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if client.received_trailers_ready(&req) {
            break;
        }
    }
    let lead = client.read_headers(&req).unwrap().unwrap();
    assert!(
        lead.iter()
            .any(|hd| hd.name == b":status" && hd.value == b"200")
    );

    let mut body = Vec::new();
    let (n, fin) = client.read_data(&req, &mut body).unwrap();
    assert_eq!(&body[..n], b"payload");
    assert!(fin);

    assert!(client.received_trailers_ready(&req));
    let trailers = client.read_trailers(&req).unwrap().unwrap();
    assert!(
        trailers
            .iter()
            .any(|hd| hd.name == b"grpc-status" && hd.value == b"0")
    );
    assert!(
        trailers
            .iter()
            .any(|hd| hd.name == b"x-trail" && hd.value == b"yes")
    );

    // Second read returns None (dispatched once).
    assert!(client.read_trailers(&req).unwrap().is_none());
    assert!(!client.received_trailers_ready(&req));
}

#[test]
fn third_headers_frame_is_dropped() {
    let (mut client, mut server) = establish();
    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();

    let req = client
        .send_request(&[
            h(b":method", b"POST"),
            h(b":scheme", b"https"),
            h(b":path", b"/svc/M"),
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

    // Three HEADERS sections from the server; the third
    // must be dropped without corrupting the first two.
    server
        .write_headers(&srv_req, &[h(b":status", b"200")])
        .unwrap();
    server
        .write_headers(&srv_req, &[h(b"grpc-status", b"0")])
        .unwrap();
    server
        .write_headers(&srv_req, &[h(b"x-bogus", b"3rd")])
        .unwrap();
    server.finish(&srv_req).unwrap();

    for _ in 0..16 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if client.received_trailers_ready(&req) {
            break;
        }
    }
    let lead = client.read_headers(&req).unwrap().unwrap();
    assert!(lead.iter().any(|hd| hd.name == b":status"));
    let trailers = client.read_trailers(&req).unwrap().unwrap();
    assert!(trailers.iter().any(|hd| hd.name == b"grpc-status"));
    assert!(!trailers.iter().any(|hd| hd.name == b"x-bogus"));
}
