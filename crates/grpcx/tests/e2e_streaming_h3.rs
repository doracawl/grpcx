//! L10 v0.4 acceptance — server-streaming +
//! client-streaming round-trips over `grpcx/h3` on the
//! single-thread alternating loopback harness.

use std::collections::VecDeque;

use grpcx::{
    ALPN_H3, Channel, Metadata, Method, RpcError, RpcKind, Server, Service, Status, StatusCode,
    StreamEvent,
};
use grpcx_crypto::TlsConfig;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    (cert.der().to_vec(), kp.serialize_der())
}

fn tls_pair() -> (TlsConfig, TlsConfig) {
    let (cert_der, key_der) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[ALPN_H3.as_bytes()]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[ALPN_H3.as_bytes()]);
    (client_cfg, server_cfg)
}

// ============================================================
// Streaming test service.
//
// "Count": server-streaming — request is an ASCII number
//   N, response is N messages "msg-0".."msg-(N-1)".
// "Sum": client-streaming — requests are ASCII numbers,
//   response is their ASCII sum.
// "FailMid": error path — answers a NotFound status.
// ============================================================

struct Streamer;

impl Service for Streamer {
    fn name(&self) -> &str {
        "test.v1.Streamer"
    }

    fn dispatch(
        &self,
        _method: &str,
        _metadata: &Metadata,
        _request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        Err(RpcError::Status(Status {
            code: StatusCode::Unimplemented,
            message: "unary not offered here".to_string(),
        }))
    }

    fn dispatch_streaming(
        &self,
        method: &str,
        _metadata: &Metadata,
        requests: &[Vec<u8>],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        match method {
            "Count" => {
                let n: usize = std::str::from_utf8(&requests[0]).unwrap().parse().unwrap();
                let out = (0..n).map(|i| format!("msg-{i}").into_bytes()).collect();
                Ok((
                    out,
                    Metadata {
                        headers: vec![("x-count".to_string(), requests[0].clone())],
                    },
                ))
            }
            "Sum" => {
                let sum: u64 = requests
                    .iter()
                    .map(|r| std::str::from_utf8(r).unwrap().parse::<u64>().unwrap())
                    .sum();
                Ok((vec![sum.to_string().into_bytes()], Metadata::default()))
            }
            "FailMid" => Err(RpcError::Status(Status {
                code: StatusCode::NotFound,
                message: "stream target missing".to_string(),
            })),
            _ => Err(RpcError::Status(Status {
                code: StatusCode::Unimplemented,
                message: String::new(),
            })),
        }
    }
}

fn method(name: &str, kind: RpcKind) -> Method {
    Method {
        service: "test.v1.Streamer".to_string(),
        method: name.to_string(),
        kind,
    }
}

// ============================================================
// Loopback harness (single-thread alternating drive).
// ============================================================

struct Loopback {
    channel: Channel,
    conn: grpcx::ServerConn,
    server: Server,
    to_server: VecDeque<Vec<u8>>,
    to_client: VecDeque<Vec<u8>>,
}

impl Loopback {
    fn new() -> Self {
        let (client_cfg, server_cfg) = tls_pair();
        let channel =
            Channel::connect_h3(&client_cfg, SERVER_NAME, "loopback.example:443").unwrap();
        let mut server = Server::new();
        server.register(Box::new(Streamer));
        let conn = server.accept_h3(&server_cfg).unwrap();
        Loopback {
            channel,
            conn,
            server,
            to_server: VecDeque::new(),
            to_client: VecDeque::new(),
        }
    }

    fn cycle(&mut self) {
        let incoming = self.to_client.pop_front();
        for d in self.channel.pump(incoming.as_deref()).unwrap() {
            self.to_server.push_back(d);
        }
        let incoming = self.to_server.pop_front();
        for d in self.conn.pump(incoming.as_deref()).unwrap() {
            self.to_client.push_back(d);
        }
        self.conn.dispatch(&self.server).unwrap();
    }

    fn establish(&mut self) {
        for _ in 0..64 {
            self.cycle();
            if self.channel.is_ready() && self.conn.is_ready() {
                return;
            }
        }
        panic!("handshake did not complete");
    }
}

// ============================================================
// Server-streaming.
// ============================================================

#[test]
fn server_streaming_count_round_trip() {
    let mut lb = Loopback::new();
    lb.establish();

    let mut call = lb
        .channel
        .begin_server_streaming(
            &method("Count", RpcKind::ServerStreaming),
            &Metadata::default(),
            b"5",
        )
        .unwrap();

    let mut messages = Vec::new();
    for _ in 0..256 {
        lb.cycle();
        match lb.channel.poll_stream_message(&mut call).unwrap() {
            None => continue,
            Some(StreamEvent::Message(m)) => messages.push(m),
            Some(StreamEvent::Complete(trailing)) => {
                assert_eq!(messages.len(), 5);
                for (i, m) in messages.iter().enumerate() {
                    assert_eq!(m, format!("msg-{i}").as_bytes());
                }
                assert!(
                    trailing
                        .headers
                        .iter()
                        .any(|(n, v)| n == "x-count" && v == b"5")
                );
                return;
            }
        }
    }
    panic!(
        "server-streaming did not complete (got {} messages)",
        messages.len()
    );
}

#[test]
fn server_streaming_zero_messages_completes() {
    let mut lb = Loopback::new();
    lb.establish();

    let mut call = lb
        .channel
        .begin_server_streaming(
            &method("Count", RpcKind::ServerStreaming),
            &Metadata::default(),
            b"0",
        )
        .unwrap();

    for _ in 0..256 {
        lb.cycle();
        match lb.channel.poll_stream_message(&mut call).unwrap() {
            None => continue,
            Some(StreamEvent::Message(m)) => panic!("unexpected message {m:?}"),
            Some(StreamEvent::Complete(_)) => return,
        }
    }
    panic!("empty stream did not complete");
}

#[test]
fn server_streaming_error_propagates() {
    let mut lb = Loopback::new();
    lb.establish();

    let mut call = lb
        .channel
        .begin_server_streaming(
            &method("FailMid", RpcKind::ServerStreaming),
            &Metadata::default(),
            b"x",
        )
        .unwrap();

    for _ in 0..256 {
        lb.cycle();
        match lb.channel.poll_stream_message(&mut call) {
            Ok(None) => continue,
            Ok(Some(ev)) => panic!("must not succeed, got {ev:?}"),
            Err(RpcError::Status(status)) => {
                assert_eq!(status.code, StatusCode::NotFound);
                assert_eq!(status.message, "stream target missing");
                return;
            }
            Err(e) => panic!("unexpected error {e:?}"),
        }
    }
    panic!("error did not propagate");
}

// ============================================================
// Client-streaming.
// ============================================================

#[test]
fn client_streaming_sum_round_trip() {
    let mut lb = Loopback::new();
    lb.establish();

    let call_handle = lb
        .channel
        .begin_client_streaming(
            &method("Sum", RpcKind::ClientStreaming),
            &Metadata::default(),
        )
        .unwrap();
    for n in [7u64, 35, 100] {
        lb.channel
            .send_stream_message(&call_handle, n.to_string().as_bytes())
            .unwrap();
        lb.cycle();
    }
    lb.channel.finish_sending(&call_handle).unwrap();

    let mut call = call_handle;
    for _ in 0..256 {
        lb.cycle();
        if let Some((payload, _)) = lb.channel.poll_unary(&mut call).unwrap() {
            assert_eq!(payload, b"142");
            return;
        }
    }
    panic!("client-streaming did not complete");
}

// ============================================================
// Unary bridge regression: a service that only overrides
// the streaming surface still serves 1-in/1-out, and a
// unary-only service (the default bridge) still works —
// covered by tests/e2e_unary_h3.rs. Here: the streaming
// service's `dispatch` (unary path) is NOT reachable
// through the transport since dispatch goes through
// dispatch_streaming; assert the wire behaviour matches
// the streaming override.
// ============================================================

// ============================================================
// Bidirectional streaming (v0.5).
// ============================================================

#[test]
fn bidi_echo_round_trip() {
    let mut lb = Loopback::new();
    lb.establish();

    // "Sum" through the bidi surface: send 3 messages on
    // an open stream, finish, receive the single response
    // as a stream event, then Complete. (The batch server
    // dispatch answers after FIN — true interleaving is
    // v1.x; the wire shape is identical.)
    let call_handle = lb
        .channel
        .begin_bidi(
            &method("Sum", RpcKind::BidirectionalStreaming),
            &Metadata::default(),
        )
        .unwrap();
    for n in [1u64, 2, 39] {
        lb.channel
            .send_bidi_message(&call_handle, n.to_string().as_bytes())
            .unwrap();
        lb.cycle();
    }
    lb.channel.finish_bidi_sending(&call_handle).unwrap();

    let mut call = call_handle;
    let mut messages = Vec::new();
    for _ in 0..256 {
        lb.cycle();
        match lb.channel.poll_stream_message(&mut call).unwrap() {
            None => continue,
            Some(StreamEvent::Message(m)) => messages.push(m),
            Some(StreamEvent::Complete(_)) => {
                assert_eq!(messages, vec![b"42".to_vec()]);
                return;
            }
        }
    }
    panic!("bidi call did not complete");
}

#[test]
fn bidi_count_multi_response() {
    let mut lb = Loopback::new();
    lb.establish();

    // "Count" through bidi: one message up, three down.
    let call_handle = lb
        .channel
        .begin_bidi(
            &method("Count", RpcKind::BidirectionalStreaming),
            &Metadata::default(),
        )
        .unwrap();
    lb.channel.send_bidi_message(&call_handle, b"3").unwrap();
    lb.channel.finish_bidi_sending(&call_handle).unwrap();

    let mut call = call_handle;
    let mut messages = Vec::new();
    for _ in 0..256 {
        lb.cycle();
        match lb.channel.poll_stream_message(&mut call).unwrap() {
            None => continue,
            Some(StreamEvent::Message(m)) => messages.push(m),
            Some(StreamEvent::Complete(_)) => {
                assert_eq!(messages.len(), 3);
                assert_eq!(messages[2], b"msg-2");
                return;
            }
        }
    }
    panic!("bidi count did not complete");
}

#[test]
fn unary_call_against_streaming_service_uses_streaming_surface() {
    let mut lb = Loopback::new();
    lb.establish();

    // A unary call to "Sum" carries one message; the
    // streaming surface sums the single element.
    let mut call = lb
        .channel
        .begin_unary(&method("Sum", RpcKind::Unary), &Metadata::default(), b"41")
        .unwrap();

    for _ in 0..256 {
        lb.cycle();
        if let Some((payload, _)) = lb.channel.poll_unary(&mut call).unwrap() {
            assert_eq!(payload, b"41");
            return;
        }
    }
    panic!("unary against streaming service did not complete");
}
