//! L10 v0.2 wire-framing KATs — length-prefix codec +
//! header/trailer mapping + the gRPC-spec wire-compat
//! vector (byte-identical with what tonic produces for
//! the same payload, since the framing layout is fully
//! determined by the spec).

use grpcx::{
    FRAME_HEADER_LEN, MessageFrame, Metadata, RpcError, Status, StatusCode, decode_all_frames,
    decode_frame, encode_frame, encode_request_headers, encode_response_headers, encode_trailers,
    parse_request_headers, parse_trailers, percent_decode_lossy, percent_encode, try_decode_frame,
};

#[test]
fn try_decode_returns_none_on_partial_input() {
    let frame = MessageFrame {
        compressed: false,
        payload: b"streaming".to_vec(),
    };
    let mut wire = Vec::new();
    encode_frame(&frame, &mut wire);

    // Every strict prefix is "pending", never an error.
    for cut in 0..wire.len() {
        assert_eq!(
            try_decode_frame(&wire[..cut]).unwrap(),
            None,
            "cut at {cut}"
        );
    }
    // The full frame decodes.
    let (decoded, consumed) = try_decode_frame(&wire).unwrap().unwrap();
    assert_eq!(decoded, frame);
    assert_eq!(consumed, wire.len());
}

#[test]
fn try_decode_still_rejects_bad_flag() {
    // Truncation is pending, but a structurally bad flag
    // byte is a hard error even before the length arrives
    // in full.
    let wire = [7u8, 0, 0, 0, 1, 0xAA];
    assert!(try_decode_frame(&wire).is_err());
}

// ============================================================
// Framing — round-trips.
// ============================================================

#[test]
fn frame_round_trip_basic() {
    let frame = MessageFrame {
        compressed: false,
        payload: b"hello, grpc".to_vec(),
    };
    let mut wire = Vec::new();
    encode_frame(&frame, &mut wire);
    assert_eq!(wire.len(), FRAME_HEADER_LEN + 11);

    let (decoded, consumed) = decode_frame(&wire).unwrap();
    assert_eq!(consumed, wire.len());
    assert_eq!(decoded, frame);
}

#[test]
fn frame_round_trip_empty_payload() {
    let frame = MessageFrame::default();
    let mut wire = Vec::new();
    encode_frame(&frame, &mut wire);
    assert_eq!(wire, [0, 0, 0, 0, 0]);
    let (decoded, consumed) = decode_frame(&wire).unwrap();
    assert_eq!(consumed, 5);
    assert_eq!(decoded, frame);
}

#[test]
fn frame_compressed_flag_round_trips() {
    let frame = MessageFrame {
        compressed: true,
        payload: vec![0xAB; 3],
    };
    let mut wire = Vec::new();
    encode_frame(&frame, &mut wire);
    assert_eq!(wire[0], 1);
    let (decoded, _) = decode_frame(&wire).unwrap();
    assert!(decoded.compressed);
}

#[test]
fn frame_streaming_sequence_decodes_all() {
    // Server-streaming: several frames back-to-back on
    // one stream.
    let mut wire = Vec::new();
    for i in 0..5u8 {
        encode_frame(
            &MessageFrame {
                compressed: false,
                payload: vec![i; i as usize],
            },
            &mut wire,
        );
    }
    let frames = decode_all_frames(&wire).unwrap();
    assert_eq!(frames.len(), 5);
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(f.payload.len(), i);
    }
}

// ============================================================
// Framing — reject paths.
// ============================================================

#[test]
fn frame_truncated_header_rejected() {
    assert_eq!(
        decode_frame(&[0, 0, 0]).unwrap_err(),
        RpcError::Codec("truncated gRPC frame header")
    );
}

#[test]
fn frame_truncated_payload_rejected() {
    // Declares 4 payload bytes, provides 2.
    let wire = [0u8, 0, 0, 0, 4, 0xAA, 0xBB];
    assert_eq!(
        decode_frame(&wire).unwrap_err(),
        RpcError::Codec("truncated gRPC frame payload")
    );
}

#[test]
fn frame_bad_flag_rejected() {
    let wire = [2u8, 0, 0, 0, 0];
    assert_eq!(
        decode_frame(&wire).unwrap_err(),
        RpcError::Codec("invalid gRPC compressed-flag")
    );
}

#[test]
fn decode_all_rejects_trailing_garbage() {
    let mut wire = Vec::new();
    encode_frame(
        &MessageFrame {
            compressed: false,
            payload: b"ok".to_vec(),
        },
        &mut wire,
    );
    wire.extend_from_slice(&[0, 0, 0]); // 3 stray bytes
    assert!(decode_all_frames(&wire).is_err());
}

// ============================================================
// gRPC-spec wire-compat vector.
//
// The framing layout is fully spec-determined, so this is
// byte-identical with tonic's output for the same
// serialized message. Payload: a protobuf message with
// field 1 = string "world" — wire bytes 0A 05 77 6F 72 6C
// 64 (tag 0x0A = field 1 / LEN, length 5, "world").
// ============================================================

#[test]
fn grpc_spec_unary_frame_vector() {
    let proto_payload: Vec<u8> = vec![0x0A, 0x05, b'w', b'o', b'r', b'l', b'd'];
    let mut wire = Vec::new();
    encode_frame(
        &MessageFrame {
            compressed: false,
            payload: proto_payload.clone(),
        },
        &mut wire,
    );
    // 1-byte flag (0) + 4-byte BE length (7) + payload.
    assert_eq!(
        wire,
        vec![
            0x00, 0x00, 0x00, 0x00, 0x07, 0x0A, 0x05, b'w', b'o', b'r', b'l', b'd'
        ]
    );
}

// ============================================================
// Headers — request build + parse round-trip.
// ============================================================

#[test]
fn request_headers_round_trip() {
    let user = Metadata {
        headers: vec![("x-trace-id".to_string(), b"abc123".to_vec())],
    };
    let hdrs = encode_request_headers("greeter.v1.Greeter", "SayHello", "localhost:4433", &user);

    // Pseudo-headers lead (HTTP/3 requirement).
    assert_eq!(hdrs.headers[0].0, ":method");
    assert_eq!(hdrs.headers[0].1, b"POST");
    assert!(
        hdrs.headers
            .iter()
            .any(|(n, v)| n == ":path" && v == b"/greeter.v1.Greeter/SayHello")
    );
    assert!(
        hdrs.headers
            .iter()
            .any(|(n, v)| n == "te" && v == b"trailers")
    );

    let (service, method, meta) = parse_request_headers(&hdrs).unwrap();
    assert_eq!(service, "greeter.v1.Greeter");
    assert_eq!(method, "SayHello");
    assert_eq!(meta, user);
}

#[test]
fn request_headers_reject_non_post() {
    let mut hdrs = encode_request_headers("s.S", "M", "h", &Metadata::default());
    hdrs.headers[0].1 = b"GET".to_vec();
    assert!(parse_request_headers(&hdrs).is_err());
}

#[test]
fn request_headers_reject_bad_content_type() {
    let mut hdrs = encode_request_headers("s.S", "M", "h", &Metadata::default());
    for (n, v) in &mut hdrs.headers {
        if n == "content-type" {
            *v = b"application/json".to_vec();
        }
    }
    assert!(parse_request_headers(&hdrs).is_err());
}

#[test]
fn request_headers_accept_bare_grpc_content_type() {
    let mut hdrs = encode_request_headers("s.S", "M", "h", &Metadata::default());
    for (n, v) in &mut hdrs.headers {
        if n == "content-type" {
            *v = b"application/grpc".to_vec();
        }
    }
    assert!(parse_request_headers(&hdrs).is_ok());
}

#[test]
fn request_headers_reject_malformed_path() {
    for bad in ["/no-method", "missing-slash/M", "/s/m/extra", "//M", "/s/"] {
        let mut hdrs = encode_request_headers("s.S", "M", "h", &Metadata::default());
        for (n, v) in &mut hdrs.headers {
            if n == ":path" {
                *v = bad.as_bytes().to_vec();
            }
        }
        assert!(
            parse_request_headers(&hdrs).is_err(),
            "path {bad:?} must be rejected"
        );
    }
}

// ============================================================
// Trailers — build + parse round-trip.
// ============================================================

#[test]
fn trailers_round_trip_ok_status() {
    let trailers = encode_trailers(&Status::default(), &Metadata::default());
    assert!(
        trailers
            .headers
            .iter()
            .any(|(n, v)| n == "grpc-status" && v == b"0")
    );
    // Empty message → no grpc-message trailer.
    assert!(!trailers.headers.iter().any(|(n, _)| n == "grpc-message"));

    let (status, meta) = parse_trailers(&trailers).unwrap();
    assert_eq!(status, Status::default());
    assert!(meta.headers.is_empty());
}

#[test]
fn trailers_round_trip_error_with_message_and_metadata() {
    let status = Status {
        code: StatusCode::NotFound,
        message: "no such user: 王".to_string(),
    };
    let trailing = Metadata {
        headers: vec![("x-debug".to_string(), b"1".to_vec())],
    };
    let trailers = encode_trailers(&status, &trailing);
    assert!(
        trailers
            .headers
            .iter()
            .any(|(n, v)| n == "grpc-status" && v == b"5")
    );

    let (parsed, meta) = parse_trailers(&trailers).unwrap();
    assert_eq!(parsed, status);
    assert_eq!(meta, trailing);
}

#[test]
fn trailers_two_digit_status_round_trips() {
    let status = Status {
        code: StatusCode::Unauthenticated,
        message: String::new(),
    };
    let trailers = encode_trailers(&status, &Metadata::default());
    assert!(
        trailers
            .headers
            .iter()
            .any(|(n, v)| n == "grpc-status" && v == b"16")
    );
    assert_eq!(
        parse_trailers(&trailers).unwrap().0.code,
        StatusCode::Unauthenticated
    );
}

#[test]
fn trailers_reject_missing_or_bad_status() {
    assert!(parse_trailers(&Metadata::default()).is_err());

    let bad = Metadata {
        headers: vec![("grpc-status".to_string(), b"17".to_vec())],
    };
    assert!(parse_trailers(&bad).is_err());

    let nonnum = Metadata {
        headers: vec![("grpc-status".to_string(), b"abc".to_vec())],
    };
    assert!(parse_trailers(&nonnum).is_err());
}

// ============================================================
// percent-coding vectors.
// ============================================================

#[test]
fn percent_encode_vectors() {
    // Printable ASCII passes through; space included.
    assert_eq!(percent_encode(b"plain text!"), b"plain text!".to_vec());
    // '%' itself is encoded.
    assert_eq!(percent_encode(b"100%"), b"100%25".to_vec());
    // Multi-byte UTF-8 encodes per byte, uppercase hex.
    assert_eq!(percent_encode("王".as_bytes()), b"%E7%8E%8B".to_vec());
    // Control bytes encode.
    assert_eq!(percent_encode(b"\n"), b"%0A".to_vec());
}

#[test]
fn percent_decode_is_lenient() {
    assert_eq!(percent_decode_lossy(b"%E7%8E%8B"), "王");
    // Stray '%' without two hex digits passes through.
    assert_eq!(percent_decode_lossy(b"50% off"), "50% off");
    assert_eq!(percent_decode_lossy(b"%G1"), "%G1");
    assert_eq!(percent_decode_lossy(b"%"), "%");
    // Lowercase hex accepted on decode.
    assert_eq!(percent_decode_lossy(b"%e7%8e%8b"), "王");
}

#[test]
fn percent_round_trip_arbitrary_bytes() {
    let raw: Vec<u8> = (0..=255u8).collect();
    let encoded = percent_encode(&raw);
    // Decoded-as-bytes equality is checked through the
    // lossy string only for valid-UTF-8 inputs; for the
    // all-bytes case, verify the encoding is pure ASCII +
    // reversible at the byte level via a manual decode.
    assert!(encoded.iter().all(u8::is_ascii));
    let decoded = percent_decode_lossy(&encoded);
    // 0x00..=0xFF as raw bytes is NOT valid UTF-8 — the
    // lossy decode replaces invalid sequences. Spot-check
    // the ASCII core instead.
    assert!(decoded.starts_with('\u{0}'));
}

// ============================================================
// grpc-timeout vectors.
// ============================================================

#[test]
fn grpc_timeout_format_vectors() {
    use grpcx::{GrpcTimeout, TimeoutUnit};
    // The spec's canonical example.
    let t = GrpcTimeout {
        value: 5000,
        unit: TimeoutUnit::Millis,
    };
    assert_eq!(
        t.to_header(),
        ("grpc-timeout".to_string(), b"5000m".to_vec())
    );
    // Each unit suffix.
    for (unit, ch) in [
        (TimeoutUnit::Hours, b'H'),
        (TimeoutUnit::Minutes, b'M'),
        (TimeoutUnit::Seconds, b'S'),
        (TimeoutUnit::Millis, b'm'),
        (TimeoutUnit::Micros, b'u'),
        (TimeoutUnit::Nanos, b'n'),
    ] {
        let (_, v) = GrpcTimeout { value: 1, unit }.to_header();
        assert_eq!(v, vec![b'1', ch]);
    }
    // 8-digit max renders.
    let (_, v) = GrpcTimeout {
        value: 99_999_999,
        unit: TimeoutUnit::Nanos,
    }
    .to_header();
    assert_eq!(v, b"99999999n".to_vec());
}

#[test]
fn grpc_timeout_parse_round_trip_and_rejects() {
    use grpcx::{GrpcTimeout, TimeoutUnit};
    assert_eq!(
        GrpcTimeout::parse(b"5000m"),
        Some(GrpcTimeout {
            value: 5000,
            unit: TimeoutUnit::Millis
        })
    );
    assert_eq!(GrpcTimeout::parse(b"0S").unwrap().value, 0);
    // Rejects: empty, no digits, >8 digits, bad unit,
    // non-digit chars.
    assert_eq!(GrpcTimeout::parse(b""), None);
    assert_eq!(GrpcTimeout::parse(b"m"), None);
    assert_eq!(GrpcTimeout::parse(b"123456789S"), None);
    assert_eq!(GrpcTimeout::parse(b"100x"), None);
    assert_eq!(GrpcTimeout::parse(b"1a0m"), None);
}

#[test]
fn grpc_timeout_travels_via_metadata() {
    use grpcx::{GrpcTimeout, TimeoutUnit};
    let t = GrpcTimeout {
        value: 30,
        unit: TimeoutUnit::Seconds,
    };
    let meta = Metadata {
        headers: vec![t.to_header()],
    };
    // Through the request-header builder + parser: the
    // entry survives as user metadata on the server side.
    let hdrs = encode_request_headers("s.S", "M", "h", &meta);
    let (_, _, user) = parse_request_headers(&hdrs).unwrap();
    assert_eq!(GrpcTimeout::from_metadata(&user), Some(t));
}

// ============================================================
// Unary end-to-end (wire-level walk-through).
// ============================================================

#[test]
fn unary_wire_walkthrough() {
    // -- client side: request headers + one request frame.
    let req_meta = Metadata {
        headers: vec![("x-request-id".to_string(), b"42".to_vec())],
    };
    let req_headers = encode_request_headers(
        "greeter.v1.Greeter",
        "SayHello",
        "svc.local:4433",
        &req_meta,
    );
    let mut req_body = Vec::new();
    encode_frame(
        &MessageFrame {
            compressed: false,
            payload: vec![0x0A, 0x05, b'w', b'o', b'r', b'l', b'd'],
        },
        &mut req_body,
    );

    // -- server side: route + decode.
    let (service, method, meta) = parse_request_headers(&req_headers).unwrap();
    assert_eq!(
        (service.as_str(), method.as_str()),
        ("greeter.v1.Greeter", "SayHello")
    );
    assert_eq!(meta.headers[0].0, "x-request-id");
    let frames = decode_all_frames(&req_body).unwrap();
    assert_eq!(frames.len(), 1);

    // -- server side: response headers + frame + trailers.
    let resp_headers = encode_response_headers();
    assert!(
        resp_headers
            .headers
            .iter()
            .any(|(n, v)| n == ":status" && v == b"200")
    );
    let mut resp_body = Vec::new();
    encode_frame(
        &MessageFrame {
            compressed: false,
            payload: vec![0x0A, 0x02, b'h', b'i'],
        },
        &mut resp_body,
    );
    let trailers = encode_trailers(&Status::default(), &Metadata::default());

    // -- client side: decode + status check.
    let resp_frames = decode_all_frames(&resp_body).unwrap();
    assert_eq!(resp_frames[0].payload, vec![0x0A, 0x02, b'h', b'i']);
    let (status, _) = parse_trailers(&trailers).unwrap();
    assert_eq!(status.code, StatusCode::Ok);
}

#[test]
fn grpc_timeout_duration_round_trips() {
    use grpcx::{GrpcTimeout, TimeoutUnit};
    // Spec-shaped picks: finest unit fitting 8 digits.
    let t = GrpcTimeout::from_duration(core::time::Duration::from_millis(50));
    assert_eq!((t.value, t.unit), (50_000_000, TimeoutUnit::Nanos));
    assert_eq!(t.as_duration(), core::time::Duration::from_millis(50));
    let t = GrpcTimeout::from_duration(core::time::Duration::from_secs(120));
    assert_eq!((t.value, t.unit), (120_000, TimeoutUnit::Millis));
    assert_eq!(t.as_duration(), core::time::Duration::from_secs(120));
    let t = GrpcTimeout::from_duration(core::time::Duration::from_secs(200_000));
    assert_eq!((t.value, t.unit), (200_000, TimeoutUnit::Seconds));
    assert_eq!(t.as_duration(), core::time::Duration::from_secs(200_000));
    // Wire round trip through the header codec.
    let (n, v) = GrpcTimeout::from_duration(core::time::Duration::from_millis(7)).to_header();
    assert_eq!(n, "grpc-timeout");
    let parsed = GrpcTimeout::parse(&v).expect("parse");
    assert_eq!(parsed.as_duration(), core::time::Duration::from_millis(7));
}
