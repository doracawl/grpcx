//! v10.0 release-pass surface KATs — the panicking
//! placeholder facades, Debug formatting, ADT helpers, and
//! negotiation edges that the e2e suites don't reach.

use grpcx::{
    Channel, Metadata, Method, RpcError, RpcKind, Server, Service, Status, StatusCode,
    TransportKind, build_alt_svc_header, parse_alt_svc_header, select_alpn_transport,
};

#[test]
#[should_panic(expected = "ConnectedChannel::connect_h3")]
fn channel_connect_facade_panics_until_io_runtime() {
    let _ = Channel::connect("127.0.0.1:443", TransportKind::H3);
}

#[test]
#[should_panic(expected = "listen_h3")]
fn server_listen_facade_panics_until_io_runtime() {
    let server = Server::new();
    let _ = server.listen("127.0.0.1:443", TransportKind::H3);
}

#[test]
fn channel_debug_and_transport_accessors() {
    let ch = Channel::connect_ws_mux("h", "h:443", "dGhlIHNhbXBsZSBub25jZQ==", 1);
    assert_eq!(ch.transport(), TransportKind::WsMux);
    let dbg = format!("{ch:?}");
    assert!(dbg.contains("Channel"));
    assert!(dbg.contains("WsMux") || dbg.contains("transport"));
}

#[test]
fn server_service_count_tracks_registration() {
    struct Nop;
    impl Service for Nop {
        fn name(&self) -> &str {
            "n.N"
        }
        fn dispatch(
            &self,
            _m: &str,
            _md: &Metadata,
            _r: &[u8],
        ) -> Result<(Vec<u8>, Metadata), RpcError> {
            Ok((Vec::new(), Metadata::default()))
        }
    }
    let mut s = Server::new();
    assert_eq!(s.service_count(), 0);
    s.register(Box::new(Nop));
    s.register(Box::new(Nop));
    assert_eq!(s.service_count(), 2);
}

#[test]
fn dispatch_streaming_default_bridge_empty_request_list() {
    struct EchoFirst;
    impl Service for EchoFirst {
        fn name(&self) -> &str {
            "e.E"
        }
        fn dispatch(
            &self,
            _m: &str,
            _md: &Metadata,
            r: &[u8],
        ) -> Result<(Vec<u8>, Metadata), RpcError> {
            Ok((r.to_vec(), Metadata::default()))
        }
    }
    // Zero request messages → the bridge feeds an empty
    // slice to unary dispatch.
    let (resps, _) = EchoFirst
        .dispatch_streaming("M", &Metadata::default(), &[])
        .unwrap();
    assert_eq!(resps, vec![Vec::<u8>::new()]);
}

#[test]
fn alpn_and_alt_svc_edge_branches() {
    // Server offers nothing.
    assert_eq!(select_alpn_transport(&["grpcx/h3"], &[]), None);
    // Five-digit port renders + parses.
    let h = build_alt_svc_header(65535, 1);
    assert_eq!(parse_alt_svc_header(&h), Some(65535));
    // Missing quote terminator.
    assert_eq!(parse_alt_svc_header("h3-grpcx=\":12"), Some(12));
}

#[test]
fn status_and_method_adt_shapes() {
    // Default Status is Ok/empty; RpcKind default is Unary.
    assert_eq!(Status::default().code, StatusCode::Ok);
    assert_eq!(Method::default().kind, RpcKind::Unary);
    // Clone + Eq on the wire-visible ADTs.
    let m = Method {
        service: "s.S".into(),
        method: "M".into(),
        kind: RpcKind::BidirectionalStreaming,
    };
    assert_eq!(m.clone(), m);
    assert_eq!(StatusCode::from_i32(17), None);
    assert_eq!(StatusCode::from_i32(-1), None);
}
