//! Minimal gRPC loopback for BINSIZE measurement.
//!
//! One unary echo RPC over `grpcx/ws-mux` (the lighter
//! transport — no TLS stack in the binary).
//! `cargo build --release --example min_grpcx` produces
//! the binary whose stripped size fills `BUDGETS.md`.

use grpcx::{Channel, Metadata, Method, RpcError, RpcKind, Server, Service};

struct Echo;

impl Service for Echo {
    fn name(&self) -> &str {
        "min.Echo"
    }
    fn dispatch(
        &self,
        _method: &str,
        _metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        Ok((request.to_vec(), Metadata::default()))
    }
}

fn main() {
    let mut channel = Channel::connect_ws_mux(
        "loopback.example",
        "loopback.example:443",
        "dGhlIHNhbXBsZSBub25jZQ==",
        7,
    );
    let mut server = Server::new();
    server.register(Box::new(Echo));
    let mut conn = server.accept_ws_mux();

    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();

    let cycle = |channel: &mut Channel,
                 conn: &mut grpcx::WsMuxServerConn,
                 server: &Server,
                 to_server: &mut Vec<Vec<u8>>,
                 to_client: &mut Vec<Vec<u8>>| {
        let incoming = if to_client.is_empty() {
            None
        } else {
            Some(to_client.remove(0))
        };
        for c in channel.pump_ws(incoming.as_deref()).unwrap() {
            to_server.push(c);
        }
        let incoming = if to_server.is_empty() {
            None
        } else {
            Some(to_server.remove(0))
        };
        for c in conn.pump(incoming.as_deref()).unwrap() {
            to_client.push(c);
        }
        conn.dispatch(server).unwrap();
    };

    for _ in 0..8 {
        cycle(
            &mut channel,
            &mut conn,
            &server,
            &mut to_server,
            &mut to_client,
        );
        if channel.is_ready_ws() && conn.is_ready() {
            break;
        }
    }
    assert!(channel.is_ready_ws());

    let call = channel
        .begin_unary_ws(
            &Method {
                service: "min.Echo".to_string(),
                method: "Ping".to_string(),
                kind: RpcKind::Unary,
            },
            &Metadata::default(),
            b"min",
        )
        .unwrap();

    for _ in 0..32 {
        cycle(
            &mut channel,
            &mut conn,
            &server,
            &mut to_server,
            &mut to_client,
        );
        if let Some((resp, _)) = channel.poll_unary_ws(&call).unwrap() {
            assert_eq!(resp, b"min");
            println!("min_grpcx ok ({} bytes)", resp.len());
            return;
        }
    }
    panic!("call did not complete");
}
