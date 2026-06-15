//! H/2 loopback — client/server pair driven in memory:
//! preface + SETTINGS handshake, a gRPC-shaped exchange
//! (HEADERS + DATA + trailers), and a 64 KiB body that
//! must punch through flow-control window starvation via
//! WINDOW_UPDATE replenishment.

use grpcx_h2::Connection;

fn pump_pair(
    a: &mut Connection,
    a_in: &[Vec<u8>],
    b: &mut Connection,
    b_in: &[Vec<u8>],
) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut a_out = Vec::new();
    for c in a_in {
        a_out.extend(a.pump(Some(c)).unwrap());
    }
    a_out.extend(a.pump(None).unwrap());
    let mut b_out = Vec::new();
    for c in b_in {
        b_out.extend(b.pump(Some(c)).unwrap());
    }
    b_out.extend(b.pump(None).unwrap());
    (a_out, b_out)
}

fn establish() -> (Connection, Connection) {
    let mut c = Connection::new_client();
    let mut s = Connection::new_server();
    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut c, &to_c, &mut s, &to_s);
        to_s = a;
        to_c = b;
        if c.is_ready() && s.is_ready() {
            break;
        }
    }
    assert!(c.is_ready() && s.is_ready(), "settings handshake failed");
    // flush residue
    for _ in 0..2 {
        let (a, b) = pump_pair(&mut c, &to_c, &mut s, &to_s);
        to_s = a;
        to_c = b;
    }
    (c, s)
}

fn h(n: &str, v: &[u8]) -> (String, Vec<u8>) {
    (n.to_string(), v.to_vec())
}

#[test]
fn grpc_shaped_round_trip() {
    let (mut c, mut s) = establish();
    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();

    let req = c.send_request(&[
        h(":method", b"POST"),
        h(":scheme", b"http"),
        h(":path", b"/pkg.Svc/M"),
        h(":authority", b"x"),
        h("content-type", b"application/grpc"),
    ]);
    c.write_data(&req, b"\x00\x00\x00\x00\x02hi");
    c.finish(&req);

    let mut srv_req = None;
    for _ in 0..16 {
        let (a, b) = pump_pair(&mut c, &to_c, &mut s, &to_s);
        to_s = a;
        to_c = b;
        if srv_req.is_none() {
            srv_req = s.accept_request();
        }
        if let Some(ref rs) = srv_req {
            let mut body = Vec::new();
            let (_, end) = s.read_data(rs, &mut body);
            if end || !body.is_empty() {
                // got the request — respond
                let hdrs = s.read_headers(rs);
                if let Some(hdrs) = hdrs {
                    assert!(
                        hdrs.iter()
                            .any(|x| x.name == b":path" && x.value == b"/pkg.Svc/M")
                    );
                }
                s.write_headers(
                    rs,
                    &[h(":status", b"200"), h("content-type", b"application/grpc")],
                );
                s.write_data(rs, b"\x00\x00\x00\x00\x02ok");
                s.write_trailers(rs, &[h("grpc-status", b"0")]);
                break;
            }
        }
    }
    let srv_req = srv_req.expect("server accepted");

    // client drains response
    let mut got_body = Vec::new();
    for _ in 0..16 {
        let (a, b) = pump_pair(&mut c, &to_c, &mut s, &to_s);
        to_s = a;
        to_c = b;
        c.read_data(&req, &mut got_body);
        if c.received_trailers_ready(&req) {
            break;
        }
    }
    let lead = c.read_headers(&req).unwrap();
    assert!(
        lead.iter()
            .any(|x| x.name == b":status" && x.value == b"200")
    );
    assert!(got_body.ends_with(b"ok"));
    let trailers = c.read_trailers(&req).unwrap();
    assert!(
        trailers
            .iter()
            .any(|x| x.name == b"grpc-status" && x.value == b"0")
    );

    // cleanup both sides
    c.close_stream(&req);
    s.close_stream(&srv_req);
}

#[test]
fn large_body_crosses_flow_control_window() {
    let (mut c, mut s) = establish();
    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();

    // 200 KiB — 3× the 64 KiB default window: progress
    // REQUIRES WINDOW_UPDATE replenishment round-trips.
    let big = vec![0xAB; 200 * 1024];
    let req = c.send_request(&[h(":method", b"POST"), h(":path", b"/big")]);
    c.write_data(&req, &big);
    c.finish(&req);

    let mut srv_req = None;
    let mut received = Vec::new();
    for _ in 0..256 {
        let (a, b) = pump_pair(&mut c, &to_c, &mut s, &to_s);
        to_s = a;
        to_c = b;
        if srv_req.is_none() {
            srv_req = s.accept_request();
        }
        if let Some(ref rs) = srv_req {
            let (_, end) = s.read_data(rs, &mut received);
            if end {
                assert_eq!(received.len(), big.len());
                assert!(received.iter().all(|&b| b == 0xAB));
                return;
            }
        }
    }
    panic!(
        "large body did not complete (got {} of {} bytes)",
        received.len(),
        big.len()
    );
}

/// Trailers must follow buffered DATA on the wire — a
/// trailer HEADERS emitted ahead of window-buffered DATA
/// is DATA-after-END_STREAM to a real peer (our own
/// decoder tolerated the reorder; tonic won't).
#[test]
fn trailers_are_ordered_after_buffered_data() {
    let (mut c, mut s) = establish();
    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();

    let req = c.send_request(&[h(":method", b"POST"), h(":path", b"/x")]);
    c.finish(&req);
    let mut srv_req = None;
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut c, &to_c, &mut s, &to_s);
        to_s = a;
        to_c = b;
        if srv_req.is_none() {
            srv_req = s.accept_request();
        }
        if srv_req.is_some() {
            break;
        }
    }
    let rs = srv_req.expect("server accepted");

    // Respond in the gRPC server shape: headers + buffered
    // data + trailers, all before the next pump.
    s.write_headers(&rs, &[h(":status", b"200")]);
    s.write_data(&rs, b"\x00\x00\x00\x00\x02ok");
    s.write_trailers(&rs, &[h("grpc-status", b"0")]);
    let out = s.pump(None).unwrap();

    // Decode the response frame sequence on stream 1 and
    // assert HEADERS < DATA < trailer-HEADERS, with
    // END_STREAM on the trailers only.
    let wire: Vec<u8> = out.concat();
    let mut off = 0;
    let mut seq = Vec::new();
    while let Some((f, n)) = grpcx_h2::frame::try_decode_frame(&wire[off..], 16384).unwrap() {
        off += n;
        if f.stream_id == rs.stream_id() {
            seq.push((f.ty, f.flags));
        }
    }
    assert_eq!(
        seq.len(),
        3,
        "HEADERS + DATA + trailer-HEADERS, got {seq:?}"
    );
    assert_eq!(seq[0].0, grpcx_h2::frame::TYPE_HEADERS);
    assert_eq!(seq[0].1 & grpcx_h2::frame::FLAG_END_STREAM, 0);
    assert_eq!(seq[1].0, grpcx_h2::frame::TYPE_DATA);
    assert_eq!(seq[1].1 & grpcx_h2::frame::FLAG_END_STREAM, 0);
    assert_eq!(seq[2].0, grpcx_h2::frame::TYPE_HEADERS);
    assert_ne!(seq[2].1 & grpcx_h2::frame::FLAG_END_STREAM, 0);
}

#[test]
fn ping_is_acked_and_unknown_frames_ignored() {
    let (c, mut s) = establish();
    // Hand-craft a PING to the server.
    let mut ping = Vec::new();
    grpcx_h2::frame::encode_frame(
        grpcx_h2::frame::TYPE_PING,
        0,
        0,
        &[1, 2, 3, 4, 5, 6, 7, 8],
        &mut ping,
    );
    // Unknown frame type 0xFA — must be ignored.
    grpcx_h2::frame::encode_frame(0xFA, 0, 0, b"junk", &mut ping);
    let out = s.pump(Some(&ping)).unwrap();
    // The PING ACK comes back with the same payload.
    let joined: Vec<u8> = out.concat();
    let (f, _) = grpcx_h2::frame::try_decode_frame(&joined, 16384)
        .unwrap()
        .unwrap();
    assert_eq!(f.ty, grpcx_h2::frame::TYPE_PING);
    assert_eq!(
        f.flags & grpcx_h2::frame::FLAG_ACK,
        grpcx_h2::frame::FLAG_ACK
    );
    assert_eq!(f.payload, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    let _ = c;
}

#[test]
fn rst_stream_cancels_and_conn_survives() {
    // Eager cancel (0.3.0): client RSTs a request
    // mid-response; the server's stream dies, its
    // subsequent writes are silent no-ops, and the SAME
    // connection serves the next request.
    let (mut c, mut s) = establish();
    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();

    let req = c.send_request(&[h(":method", b"POST"), h(":path", b"/cancel.Me/Slow")]);
    for _ in 0..4 {
        let (a, b) = pump_pair(&mut c, &to_c, &mut s, &to_s);
        to_s = a;
        to_c = b;
    }
    let srv_rs = s.accept_request().expect("server sees the request");

    // Server starts answering; client cancels.
    s.write_headers(&srv_rs, &[h(":status", b"200")]);
    c.send_rst_stream(&req, 0x8); // CANCEL
    for _ in 0..4 {
        let (a, b) = pump_pair(&mut c, &to_c, &mut s, &to_s);
        to_s = a;
        to_c = b;
    }

    // Server-side writes after the RST are no-ops — no
    // frames for the dead sid leave the connection (the
    // peer would treat them as protocol errors).
    s.write_data(&srv_rs, b"too late");
    s.write_trailers(&srv_rs, &[h("grpc-status", b"0")]);
    let (a, b) = pump_pair(&mut c, &to_c, &mut s, &to_s);
    to_s = a;
    to_c = b;
    let late_bytes: usize = to_c.iter().map(|v| v.len()).sum();
    assert_eq!(late_bytes, 0, "post-RST writes must emit nothing: {to_c:?}");

    // Fresh request on the SAME connection round-trips.
    let req2 = c.send_request(&[h(":method", b"POST"), h(":path", b"/cancel.Me/Next")]);
    c.write_data(&req2, b"hello");
    c.finish(&req2);
    for _ in 0..6 {
        let (a, b) = pump_pair(&mut c, &to_c, &mut s, &to_s);
        to_s = a;
        to_c = b;
    }
    let srv_rs2 = s.accept_request().expect("second request arrives");
    let mut body = Vec::new();
    let (_, fin) = s.read_data(&srv_rs2, &mut body);
    assert!(fin, "request must be complete");
    assert_eq!(body, b"hello");
}
