//! v0.6 KAT — `Sec-WebSocket-Extensions` parse/format +
//! `permessage-deflate` recognition via the public entry
//! points.

use grpcx_ws::{
    ExtensionOffer, PermessageDeflateOffer, WsError, find_offer_by_name, format_extension_header,
    parse_extension_header, parse_extension_offer,
};

// ============================================================
// Accept paths — parse_extension_header.
// ============================================================

#[test]
fn single_offer_no_params() {
    let parsed = parse_extension_header("permessage-deflate").unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].name, "permessage-deflate");
    assert!(parsed[0].params.is_empty());
}

#[test]
fn single_offer_bare_key_param() {
    let parsed = parse_extension_header("permessage-deflate; client_no_context_takeover").unwrap();
    assert_eq!(parsed[0].name, "permessage-deflate");
    assert_eq!(
        parsed[0].params,
        vec![("client_no_context_takeover".to_string(), None)]
    );
}

#[test]
fn single_offer_key_value_param() {
    let parsed = parse_extension_header("permessage-deflate; server_max_window_bits=10").unwrap();
    assert_eq!(
        parsed[0].params,
        vec![("server_max_window_bits".to_string(), Some("10".to_string()))]
    );
}

#[test]
fn single_offer_quoted_value() {
    let parsed = parse_extension_header("x-foo; bar=\"quoted, with, commas\"").unwrap();
    assert_eq!(
        parsed[0].params,
        vec![("bar".to_string(), Some("quoted, with, commas".to_string()))]
    );
}

#[test]
fn quoted_value_with_escape() {
    let parsed = parse_extension_header("x-foo; bar=\"hi \\\"world\\\"\"").unwrap();
    assert_eq!(parsed[0].params[0].1, Some("hi \"world\"".to_string()));
}

#[test]
fn multiple_offers() {
    let parsed = parse_extension_header("permessage-deflate, x-mux").unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].name, "permessage-deflate");
    assert_eq!(parsed[1].name, "x-mux");
}

#[test]
fn whitespace_tolerance() {
    let parsed =
        parse_extension_header("  permessage-deflate ; client_max_window_bits = 15  ,  x-foo  ")
            .unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].name, "permessage-deflate");
    assert_eq!(
        parsed[0].params,
        vec![("client_max_window_bits".to_string(), Some("15".to_string()))]
    );
    assert_eq!(parsed[1].name, "x-foo");
}

// ============================================================
// Round-trip parse → format → parse.
// ============================================================

#[test]
fn round_trip_parse_format_parse() {
    let original = "permessage-deflate; client_no_context_takeover; server_max_window_bits=10";
    let parsed1 = parse_extension_header(original).unwrap();
    let formatted = format_extension_header(&parsed1);
    let parsed2 = parse_extension_header(&formatted).unwrap();
    assert_eq!(parsed1, parsed2);
}

#[test]
fn format_quotes_value_with_special_chars() {
    let offers = vec![ExtensionOffer {
        name: "x-foo".to_string(),
        params: vec![("bar".to_string(), Some("hello world".to_string()))],
    }];
    let out = format_extension_header(&offers);
    assert!(out.contains("\"hello world\""));
}

// ============================================================
// PermessageDeflateOffer.
// ============================================================

#[test]
fn permessage_deflate_parse_no_params() {
    let offer = ExtensionOffer {
        name: "permessage-deflate".to_string(),
        params: vec![],
    };
    let pmd = PermessageDeflateOffer::parse(&offer).unwrap();
    assert_eq!(pmd, PermessageDeflateOffer::default());
}

#[test]
fn permessage_deflate_parse_all_params() {
    let parsed = parse_extension_header(
        "permessage-deflate; server_no_context_takeover; \
         client_no_context_takeover; \
         server_max_window_bits=10; client_max_window_bits=15",
    )
    .unwrap();
    let pmd = PermessageDeflateOffer::parse(&parsed[0]).unwrap();
    assert!(pmd.server_no_context_takeover);
    assert!(pmd.client_no_context_takeover);
    assert_eq!(pmd.server_max_window_bits, Some(10));
    assert_eq!(pmd.client_max_window_bits, Some(15));
}

#[test]
fn permessage_deflate_to_offer_round_trip() {
    let pmd = PermessageDeflateOffer {
        server_no_context_takeover: true,
        client_no_context_takeover: false,
        server_max_window_bits: Some(12),
        client_max_window_bits: None,
    };
    let offer = pmd.to_offer();
    let parsed_back = PermessageDeflateOffer::parse(&offer).unwrap();
    assert_eq!(parsed_back, pmd);
}

#[test]
fn permessage_deflate_window_bits_range() {
    // Accept 8 and 15 (boundary).
    for n in [8u8, 15] {
        let s = alloc::format!("permessage-deflate; server_max_window_bits={n}");
        let parsed = parse_extension_header(&s).unwrap();
        assert!(PermessageDeflateOffer::parse(&parsed[0]).is_ok());
    }
    // Reject 7 and 16.
    for n in [7u8, 16] {
        let s = alloc::format!("permessage-deflate; server_max_window_bits={n}");
        let parsed = parse_extension_header(&s).unwrap();
        let err = PermessageDeflateOffer::parse(&parsed[0]).unwrap_err();
        assert!(matches!(err, WsError::BadHandshake(_)));
    }
}

#[test]
fn permessage_deflate_unknown_param_rejected() {
    let offer = ExtensionOffer {
        name: "permessage-deflate".to_string(),
        params: vec![("unknown_key".to_string(), None)],
    };
    assert!(PermessageDeflateOffer::parse(&offer).is_err());
}

#[test]
fn permessage_deflate_wrong_name_rejected() {
    let offer = ExtensionOffer {
        name: "x-not-permessage".to_string(),
        params: vec![],
    };
    assert!(PermessageDeflateOffer::parse(&offer).is_err());
}

extern crate alloc;

// ============================================================
// find_offer_by_name.
// ============================================================

#[test]
fn find_offer_hit_and_miss() {
    let parsed = parse_extension_header("a, permessage-deflate, c").unwrap();
    assert!(find_offer_by_name(&parsed, "permessage-deflate").is_some());
    assert!(find_offer_by_name(&parsed, "does-not-exist").is_none());
    // Case-sensitive per RFC 6455 § 9.1.
    assert!(find_offer_by_name(&parsed, "PERMESSAGE-DEFLATE").is_none());
}

// ============================================================
// parse_extension_offer (single offer, no commas).
// ============================================================

#[test]
fn parse_single_offer() {
    let offer = parse_extension_offer("permessage-deflate; server_no_context_takeover").unwrap();
    assert_eq!(offer.name, "permessage-deflate");
    assert_eq!(offer.params.len(), 1);
}

#[test]
fn parse_single_offer_rejects_comma() {
    // Comma indicates multiple offers; the single-offer
    // parser should reject extra characters.
    let err = parse_extension_offer("a, b").unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(_)));
}

// ============================================================
// Reject paths.
// ============================================================

#[test]
fn reject_trailing_comma() {
    let err = parse_extension_header("a, b,").unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(_)));
}

#[test]
fn reject_unterminated_quote() {
    let err = parse_extension_header("a; k=\"oops").unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(_)));
}

#[test]
fn reject_non_ascii_in_token() {
    // `é` is not in the RFC 7230 token set.
    let err = parse_extension_header("héllo").unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(_)));
}

#[test]
fn reject_empty_offer_name() {
    let err = parse_extension_header("; key=value").unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(_)));
}

#[test]
fn empty_header_value_is_empty_list() {
    // Empty header (or all-whitespace) parses to an empty
    // offers vec — not an error.
    let parsed = parse_extension_header("").unwrap();
    assert!(parsed.is_empty());
    let parsed = parse_extension_header("   ").unwrap();
    assert!(parsed.is_empty());
}
