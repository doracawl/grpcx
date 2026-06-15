//! Full intra-process QUIC loopback — handshake to 1-RTT +
//! bidirectional STREAM round-trip. v0.6 integration acceptance.

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

fn pump(
    a: &mut QuicConnection,
    a_in: &[Vec<u8>],
    b: &mut QuicConnection,
    b_in: &[Vec<u8>],
) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut a_out: Vec<Vec<u8>> = Vec::new();
    for dg in a_in {
        let dgs = a.pump(Some(dg)).expect("a pump in");
        a_out.extend(dgs);
    }
    a_out.extend(a.pump(None).expect("a pump idle"));

    let mut b_out: Vec<Vec<u8>> = Vec::new();
    for dg in b_in {
        let dgs = b.pump(Some(dg)).expect("b pump in");
        b_out.extend(dgs);
    }
    b_out.extend(b.pump(None).expect("b pump idle"));

    (a_out, b_out)
}

fn drive_to_established(client: &mut QuicConnection, server: &mut QuicConnection) {
    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..16 {
        let (a, b) = pump(client, &to_client, server, &to_server);
        to_server = a;
        to_client = b;
        if client.is_established()
            && server.is_established()
            && to_server.is_empty()
            && to_client.is_empty()
        {
            return;
        }
    }
}

fn drain_until(
    client: &mut QuicConnection,
    server: &mut QuicConnection,
    mut done: impl FnMut(&QuicConnection, &QuicConnection) -> bool,
) {
    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..16 {
        if done(client, server) {
            return;
        }
        let (a, b) = pump(client, &to_client, server, &to_server);
        to_server = a;
        to_client = b;
        if to_server.is_empty() && to_client.is_empty() && done(client, server) {
            return;
        }
    }
}

#[test]
fn handshake_to_one_rtt_and_stream_round_trip() {
    let (cert_der, key_der) = mint_cert();

    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).expect("server cfg");
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).expect("trust");
    client_cfg.set_alpn(&[b"h3"]);

    let mut client = QuicConnection::new_client(&client_cfg, SERVER_NAME, b"client-tp".to_vec())
        .expect("client conn");
    let mut server =
        QuicConnection::new_server(&server_cfg, b"server-tp".to_vec()).expect("server conn");

    // Drive the handshake to 1-RTT.
    drive_to_established(&mut client, &mut server);

    assert!(client.is_established(), "client must reach 1-RTT");
    assert!(server.is_established(), "server must reach 1-RTT");
    assert_eq!(client.alpn_protocol().as_deref(), Some(b"h3".as_ref()));
    assert_eq!(server.alpn_protocol().as_deref(), Some(b"h3".as_ref()));

    // Client → Server: open a bidi stream, write, finish.
    let sid = client.open_bi();
    client
        .write_stream(sid, b"GET /index", true)
        .expect("write");
    drain_until(&mut client, &mut server, |_c, s| {
        let mut probe = Vec::new();
        let (n, fin) = s.peek_stream(sid, &mut probe);
        let _ = (n, fin); // probe doesn't drain
        // We're done when server has received the FIN-tagged bytes.
        false
    });

    let mut server_recv = Vec::new();
    let (n, fin) = server.read_stream(sid, &mut server_recv);
    assert!(n > 0, "server should have received bytes by now");
    assert_eq!(&server_recv[..], b"GET /index");
    assert!(fin, "server should see FIN");

    // Server → Client.
    server
        .write_stream(sid, b"HTTP/3 200 OK", true)
        .expect("server write");
    drain_until(&mut client, &mut server, |c, _s| c.streams_have_data(sid));
    let mut client_recv = Vec::new();
    let (n, fin) = client.read_stream(sid, &mut client_recv);
    assert!(n > 0, "client should have received bytes by now");
    assert_eq!(&client_recv[..], b"HTTP/3 200 OK");
    assert!(fin, "client should see FIN");
}
