//! RFC 4648 § 10 canonical test vectors + edge cases.

use base64_rfc4648::{
    Alphabet, DecodeError, PaddingMode, decode, decode_padded, decode_with, encode, encode_with,
};

// ============================================================
// RFC 4648 § 10 canonical encode vectors.
// ============================================================

#[test]
fn rfc4648_section_10_encode_standard() {
    assert_eq!(encode(b""), "");
    assert_eq!(encode(b"f"), "Zg==");
    assert_eq!(encode(b"fo"), "Zm8=");
    assert_eq!(encode(b"foo"), "Zm9v");
    assert_eq!(encode(b"foob"), "Zm9vYg==");
    assert_eq!(encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(encode(b"foobar"), "Zm9vYmFy");
}

// ============================================================
// RFC 4648 § 10 canonical decode vectors (round-trip).
// ============================================================

#[test]
fn rfc4648_section_10_decode_standard() {
    assert_eq!(decode("").unwrap(), b"");
    assert_eq!(decode("Zg==").unwrap(), b"f");
    assert_eq!(decode("Zm8=").unwrap(), b"fo");
    assert_eq!(decode("Zm9v").unwrap(), b"foo");
    assert_eq!(decode("Zm9vYg==").unwrap(), b"foob");
    assert_eq!(decode("Zm9vYmE=").unwrap(), b"fooba");
    assert_eq!(decode("Zm9vYmFy").unwrap(), b"foobar");
}

// ============================================================
// Long-payload round-trip — every byte value covered.
// ============================================================

#[test]
fn round_trip_all_byte_values() {
    let bytes: Vec<u8> = (0..=255u8).collect();
    let encoded = encode(&bytes);
    let decoded = decode(&encoded).unwrap();
    assert_eq!(decoded, bytes);
}

#[test]
fn round_trip_random_lengths() {
    // Lengths 0..=300 cover every (len % 3) bucket many
    // times, plus the > 1-quad-per-loop case.
    for n in 0..=300usize {
        let bytes: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
        let encoded = encode(&bytes);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, bytes, "round-trip failed at length {n}");
    }
}

// ============================================================
// URL-safe alphabet.
// ============================================================

#[test]
fn urlsafe_alphabet_uses_dash_and_underscore() {
    // `+` → `-`, `/` → `_`. Pick bytes that produce both.
    // 0xFB → "+w==" / "-w==". 0xFE → "/g==" / "_g==".
    assert_eq!(encode_with(&[0xFB], Alphabet::Standard), "+w==");
    assert_eq!(encode_with(&[0xFB], Alphabet::UrlSafe), "-w==");
    assert_eq!(encode_with(&[0xFE], Alphabet::Standard), "/g==");
    assert_eq!(encode_with(&[0xFE], Alphabet::UrlSafe), "_g==");
}

#[test]
fn urlsafe_round_trip() {
    let bytes: Vec<u8> = (0..=255u8).collect();
    let encoded = encode_with(&bytes, Alphabet::UrlSafe);
    // Must not contain `+` or `/`.
    assert!(!encoded.contains('+'));
    assert!(!encoded.contains('/'));
    let decoded = decode_with(&encoded, Alphabet::UrlSafe).unwrap();
    assert_eq!(decoded, bytes);
}

#[test]
fn urlsafe_rejects_standard_chars() {
    // `+` and `/` are NOT in the URL-safe alphabet.
    assert_eq!(
        decode_with("+w==", Alphabet::UrlSafe).unwrap_err(),
        DecodeError::InvalidCharacter
    );
    assert_eq!(
        decode_with("/g==", Alphabet::UrlSafe).unwrap_err(),
        DecodeError::InvalidCharacter
    );
}

// ============================================================
// Padding-mode behaviour.
// ============================================================

#[test]
fn strict_rejects_missing_padding() {
    // "Zg" is "f" without the "==" padding.
    assert_eq!(decode("Zg").unwrap_err(), DecodeError::MissingPadding);
    assert_eq!(decode("Zm8").unwrap_err(), DecodeError::MissingPadding);
}

#[test]
fn tolerant_accepts_missing_padding() {
    assert_eq!(
        decode_padded("Zg", Alphabet::Standard, PaddingMode::Tolerant).unwrap(),
        b"f"
    );
    assert_eq!(
        decode_padded("Zm8", Alphabet::Standard, PaddingMode::Tolerant).unwrap(),
        b"fo"
    );
    // Already-padded input still works.
    assert_eq!(
        decode_padded("Zg==", Alphabet::Standard, PaddingMode::Tolerant).unwrap(),
        b"f"
    );
}

#[test]
fn tolerant_rejects_wrong_padding_count() {
    // "Zg=" has only 1 `=` where 2 are required.
    assert_eq!(
        decode_padded("Zg=", Alphabet::Standard, PaddingMode::Tolerant).unwrap_err(),
        DecodeError::MissingPadding
    );
}

// ============================================================
// Reject paths.
// ============================================================

#[test]
fn invalid_character_rejected() {
    assert_eq!(decode("Zg!=").unwrap_err(), DecodeError::InvalidCharacter);
    assert_eq!(decode("Zg=Z").unwrap_err(), DecodeError::InvalidCharacter);
    // Whitespace is not in the alphabet.
    assert_eq!(decode("Z m9v").unwrap_err(), DecodeError::InvalidCharacter);
    // Non-ASCII.
    assert_eq!(decode("Zg=é").unwrap_err(), DecodeError::InvalidCharacter);
}

#[test]
fn invalid_length_one_char_remainder_rejected() {
    // 1 char after stripping padding can't encode any
    // byte (need ≥ 2 chars for 1 byte).
    assert_eq!(decode("a").unwrap_err(), DecodeError::InvalidLength);
    // With explicit "===" padding, more than 2 `=` → also
    // InvalidLength (per RFC 4648 § 3.2 max-2-equals
    // rule).
    assert_eq!(decode("a===").unwrap_err(), DecodeError::InvalidLength);
}

#[test]
fn non_canonical_tail_after_long_valid_body_rejected() {
    // 64 valid chars (one full SIMD group on AArch64)
    // followed by a non-canonical 2-char tail — exercises
    // the vector-path-succeeds-then-tail-errors branch.
    let mut input = "A".repeat(64);
    input.push_str("Zh==");
    assert_eq!(decode(&input).unwrap_err(), DecodeError::NonCanonical);
}

#[test]
fn invalid_char_in_long_body_rejected() {
    // Invalid byte inside the SIMD group → vector path
    // bails, scalar re-scan reports InvalidCharacter.
    let mut input = "A".repeat(63);
    input.push(' ');
    input.push_str("Zg==");
    assert_eq!(decode(&input).unwrap_err(), DecodeError::InvalidCharacter);
}

#[test]
fn non_canonical_trailing_bits_rejected_2_char_tail() {
    // "Zh==" — c0=25 (Z), c1=33 (h). Low 4 bits of c1 = 1,
    // which encodes garbage past the single output byte
    // — violates RFC 4648 § 3.5 canonical-form rule.
    assert_eq!(decode("Zh==").unwrap_err(), DecodeError::NonCanonical);
}

#[test]
fn non_canonical_trailing_bits_rejected_3_char_tail() {
    // "Zm9=" — c0=25 (Z), c1=38 (m), c2=61 ('9'). Low 2
    // bits of c2 = 1, garbage past the two output bytes.
    assert_eq!(decode("Zm9=").unwrap_err(), DecodeError::NonCanonical);
}

#[test]
fn canonical_2_char_tail_accepted() {
    // "Zg==" — c0=25, c1=32 (g). c1 & 0x0F = 0. Canonical.
    assert_eq!(decode("Zg==").unwrap(), b"f");
}

#[test]
fn canonical_3_char_tail_accepted() {
    // "Zm8=" — c0=25, c1=38, c2=60 ('8'). c2 & 0x03 = 0.
    // Canonical.
    assert_eq!(decode("Zm8=").unwrap(), b"fo");
}

// ============================================================
// WebSocket RFC 6455 § 1.3 reference vector.
//
// The handshake test that drove this crate's extraction
// from grpcx-ws.
// ============================================================

#[test]
fn rfc6455_section_1_3_websocket_reference() {
    let sha1_bytes: [u8; 20] = [
        0xb3, 0x7a, 0x4f, 0x2c, 0xc0, 0x62, 0x4f, 0x16, 0x90, 0xf6, 0x46, 0x06, 0xcf, 0x38, 0x59,
        0x45, 0xb2, 0xbe, 0xc4, 0xea,
    ];
    assert_eq!(encode(&sha1_bytes), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    // And decode reverses it.
    assert_eq!(
        decode("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=").unwrap(),
        sha1_bytes.to_vec()
    );
}

// ============================================================
// Padding-only / oddities.
// ============================================================

#[test]
fn three_or_more_equals_rejected() {
    // Max 2 `=` per RFC § 3.2.
    assert!(decode("Zg===").is_err());
    assert!(decode("====").is_err());
}

#[test]
fn padding_only_input_rejected() {
    assert!(decode("=").is_err());
    assert!(decode("==").is_err());
}
