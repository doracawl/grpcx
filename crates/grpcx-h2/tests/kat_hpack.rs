//! RFC 7541 Appendix C test vectors — the HPACK gold
//! standard. C.4/C.6 exercise the Huffman table against
//! the spec's own encodings.

use grpcx_h2::hpack::{DynamicTable, decode_header_block, encode_header_block, huffman_decode};

fn hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .flat_map(|w| {
            (0..w.len())
                .step_by(2)
                .map(move |i| u8::from_str_radix(&w[i..i + 2], 16).unwrap())
        })
        .collect()
}

// ---- C.4: request examples WITH Huffman coding ----

#[test]
fn c4_1_first_request_huffman() {
    let block = hex("8286 8441 8cf1 e3c2 e5f2 3a6b a0ab 90f4 ff");
    let mut t = DynamicTable::new(4096);
    let h = decode_header_block(&block, &mut t).unwrap();
    assert_eq!(
        h,
        vec![
            (":method".to_string(), b"GET".to_vec()),
            (":scheme".to_string(), b"http".to_vec()),
            (":path".to_string(), b"/".to_vec()),
            (":authority".to_string(), b"www.example.com".to_vec()),
        ]
    );
}

#[test]
fn c4_full_request_sequence_huffman() {
    let mut t = DynamicTable::new(4096);
    // C.4.1
    decode_header_block(&hex("8286 8441 8cf1 e3c2 e5f2 3a6b a0ab 90f4 ff"), &mut t).unwrap();
    // C.4.2 — second request reuses the dynamic entry.
    let h2 = decode_header_block(&hex("8286 84be 5886 a8eb 1064 9cbf"), &mut t).unwrap();
    assert_eq!(
        h2[3],
        (":authority".to_string(), b"www.example.com".to_vec())
    );
    assert_eq!(h2[4], ("cache-control".to_string(), b"no-cache".to_vec()));
    assert_eq!(h2[0], (":method".to_string(), b"GET".to_vec()));
    // C.4.3 — third request.
    let h3 = decode_header_block(
        &hex("8287 85bf 4088 25a8 49e9 5ba9 7d7f 8925 a849 e95b b8e8 b4bf"),
        &mut t,
    )
    .unwrap();
    assert_eq!(h3[2], (":path".to_string(), b"/index.html".to_vec()));
    assert_eq!(h3[4], ("custom-key".to_string(), b"custom-value".to_vec()));
}

// ---- C.3: same requests WITHOUT Huffman ----

#[test]
fn c3_request_sequence_plain() {
    let mut t = DynamicTable::new(4096);
    let h1 = decode_header_block(
        &hex("8286 8441 0f77 7777 2e65 7861 6d70 6c65 2e63 6f6d"),
        &mut t,
    )
    .unwrap();
    assert_eq!(
        h1[3],
        (":authority".to_string(), b"www.example.com".to_vec())
    );
    let h2 = decode_header_block(&hex("8286 84be 5808 6e6f 2d63 6163 6865"), &mut t).unwrap();
    assert_eq!(
        h2[3],
        (":authority".to_string(), b"www.example.com".to_vec())
    );
    assert_eq!(h2[4], ("cache-control".to_string(), b"no-cache".to_vec()));
}

// ---- C.6: response examples WITH Huffman (small table) ----

#[test]
fn c6_response_sequence_huffman() {
    // SETTINGS_HEADER_TABLE_SIZE = 256 → evictions kick in.
    let mut t = DynamicTable::new(256);
    let h1 = decode_header_block(
        &hex("4882 6402 5885 aec3 771a 4b61 96d0 7abe 9410 54d4 44a8 2005 9504 0b81 66e0 82a6 2d1b ff6e 919d 29ad 1718 63c7 8f0b 97c8 e9ae 82ae 43d3"),
        &mut t,
    )
    .unwrap();
    assert_eq!(h1[0], (":status".to_string(), b"302".to_vec()));
    assert_eq!(h1[1], ("cache-control".to_string(), b"private".to_vec()));
    assert_eq!(
        h1[2],
        (
            "date".to_string(),
            b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()
        )
    );
    assert_eq!(
        h1[3],
        ("location".to_string(), b"https://www.example.com".to_vec())
    );

    // C.6.2: ":status 307" evicts the oldest entry.
    let h2 = decode_header_block(&hex("4883 640e ffc1 c0bf"), &mut t).unwrap();
    assert_eq!(h2[0], (":status".to_string(), b"307".to_vec()));
}

// ---- Huffman primitive ----

#[test]
fn huffman_decode_spec_strings() {
    assert_eq!(
        huffman_decode(&hex("f1e3 c2e5 f23a 6ba0 ab90 f4ff")).unwrap(),
        b"www.example.com"
    );
    assert_eq!(huffman_decode(&hex("a8eb 1064 9cbf")).unwrap(), b"no-cache");
    assert_eq!(
        huffman_decode(&hex("25a8 49e9 5ba9 7d7f")).unwrap(),
        b"custom-key"
    );
    // Bad padding (zeros) rejected.
    assert!(huffman_decode(&[0xf1, 0x00]).is_err());
}

// ---- Our encoder round-trips through our decoder ----

#[test]
fn encode_decode_round_trip() {
    let headers = vec![
        (":method".to_string(), b"POST".to_vec()),
        (":path".to_string(), b"/pkg.Svc/Method".to_vec()),
        (
            "content-type".to_string(),
            b"application/grpc+proto".to_vec(),
        ),
        ("x-custom".to_string(), vec![0xde, 0xad]),
    ];
    let mut block = Vec::new();
    encode_header_block(&headers, &mut block);
    let mut t = DynamicTable::new(4096);
    let decoded = decode_header_block(&block, &mut t).unwrap();
    assert_eq!(decoded, headers);
}
