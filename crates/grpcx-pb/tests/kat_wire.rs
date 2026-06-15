//! v0.6 KAT — end-to-end `ProtoCodec` round-trip via a
//! hand-written impl on a small realistic type
//! (`Hello { name: string, age: int32 }`).
//!
//! The hand-written impl is the proof-of-concept that the v0.6
//! trait surface matches what v0.7's codegen will emit
//! automatically. Byte-pattern matches the protoc reference
//! for one fixture to lock wire compatibility at the spec
//! level.

use grpcx_pb::{
    ProtoCodec, WireError, WireType, decode_string, decode_tag, decode_varint, encode_tag,
    encode_varint, from_proto_bytes, skip_field, to_proto_bytes,
};

// ============================================================
// Hand-written ProtoCodec impl for `Hello`.
//
// `.proto` schema:
//   edition = "2023";
//   message Hello {
//     string name = 1;
//     int32  age  = 2;
//   }
//
// Wire layout:
//   - Tag (1, LengthDelimited) + varint(len) + UTF-8 bytes
//   - Tag (2, Varint) + zig-extended varint(age as i64)
// ============================================================

#[derive(Debug, Clone, PartialEq, Eq)]
struct Hello {
    name: String,
    age: i32,
}

impl ProtoCodec for Hello {
    fn encode(&self, out: &mut alloc::vec::Vec<u8>) {
        // Field 1 — string `name`
        encode_tag(1, WireType::LengthDelimited, out);
        let name_bytes = self.name.as_bytes();
        encode_varint(name_bytes.len() as u64, out);
        out.extend_from_slice(name_bytes);
        // Field 2 — int32 `age` (encoded as two's-complement
        // varint; protobuf int32 negatives sign-extend to 10
        // bytes).
        encode_tag(2, WireType::Varint, out);
        encode_varint(self.age as i64 as u64, out);
    }

    fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut name = String::new();
        let mut age: i32 = 0;
        let mut pos = 0;
        while pos < bytes.len() {
            let (field, wire, n) = decode_tag(&bytes[pos..])?;
            pos += n;
            match (field, wire) {
                (1, WireType::LengthDelimited) => {
                    let (s, n) = decode_string(&bytes[pos..])?;
                    name = s;
                    pos += n;
                }
                (2, WireType::Varint) => {
                    let (v, n) = decode_varint(&bytes[pos..])?;
                    age = v as i64 as i32;
                    pos += n;
                }
                (_, wire) => {
                    // Unknown field: skip per wire type (the
                    // v0.6 contract is "drop unknown fields").
                    let n = skip_field(wire, &bytes[pos..])?;
                    pos += n;
                }
            }
        }
        Ok(Hello { name, age })
    }
}

extern crate alloc;

// ============================================================
// Round-trip suite.
// ============================================================

#[test]
fn hello_round_trip_basic() {
    let original = Hello {
        name: "Alice".into(),
        age: 30,
    };
    let bytes = to_proto_bytes(&original);
    let decoded: Hello = from_proto_bytes(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn hello_round_trip_empty_name() {
    let original = Hello {
        name: String::new(),
        age: 0,
    };
    let bytes = to_proto_bytes(&original);
    let decoded: Hello = from_proto_bytes(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn hello_round_trip_negative_age() {
    let original = Hello {
        name: "Bob".into(),
        age: -42,
    };
    let bytes = to_proto_bytes(&original);
    let decoded: Hello = from_proto_bytes(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn hello_round_trip_unicode_name() {
    let original = Hello {
        name: "héllo🦀".into(),
        age: 7,
    };
    let bytes = to_proto_bytes(&original);
    let decoded: Hello = from_proto_bytes(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn hello_round_trip_large_age() {
    let original = Hello {
        name: "max".into(),
        age: i32::MAX,
    };
    let bytes = to_proto_bytes(&original);
    let decoded: Hello = from_proto_bytes(&bytes).unwrap();
    assert_eq!(decoded, original);
}

// ============================================================
// Byte-pattern lock — `Hello { name: "Alice", age: 30 }`
// against the protoc-reference layout:
//
//   0x0A 0x05 'A' 'l' 'i' 'c' 'e'   ← tag(1, LengthDelimited), len=5, "Alice"
//   0x10 0x1E                        ← tag(2, Varint), 30
// ============================================================

#[test]
fn hello_byte_pattern_locks_protoc_reference() {
    let original = Hello {
        name: "Alice".into(),
        age: 30,
    };
    let bytes = to_proto_bytes(&original);
    assert_eq!(
        bytes,
        [0x0A, 0x05, b'A', b'l', b'i', b'c', b'e', 0x10, 0x1E]
    );
}

// ============================================================
// Decoder gracefully skips unknown fields.
// ============================================================

#[test]
fn hello_decoder_skips_unknown_fields() {
    // Construct bytes: known field 1 = "Hi", unknown field 99
    // = varint 42, known field 2 = 5.
    let mut bytes = alloc::vec::Vec::new();
    // field 1 "Hi"
    encode_tag(1, WireType::LengthDelimited, &mut bytes);
    encode_varint(2, &mut bytes);
    bytes.extend_from_slice(b"Hi");
    // unknown field 99 Varint 42
    encode_tag(99, WireType::Varint, &mut bytes);
    encode_varint(42, &mut bytes);
    // field 2 = 5
    encode_tag(2, WireType::Varint, &mut bytes);
    encode_varint(5, &mut bytes);

    let decoded: Hello = from_proto_bytes(&bytes).unwrap();
    assert_eq!(
        decoded,
        Hello {
            name: "Hi".into(),
            age: 5
        }
    );
}

// ============================================================
// Decoder rejects malformed input.
// ============================================================

#[test]
fn hello_decoder_rejects_truncated() {
    // Tag for field 1 + length 5 but only 3 bytes follow.
    let bytes = [0x0A, 0x05, b'H', b'i', b'!'];
    let err = Hello::decode(&bytes).unwrap_err();
    assert_eq!(err, WireError::Truncated);
}

#[test]
fn hello_decoder_rejects_non_utf8_in_string() {
    // Tag (1, LengthDelimited), len=2, invalid UTF-8.
    let bytes = [0x0A, 0x02, 0xFF, 0xFE];
    let err = Hello::decode(&bytes).unwrap_err();
    assert_eq!(err, WireError::Utf8);
}

#[test]
fn hello_decoder_rejects_group_in_unknown_field() {
    // Tag (99, StartGroup) = (99 << 3) | 3 = 795 → varint
    // 0x9B 0x06.
    let bytes = [0x9B, 0x06, 0x00];
    let err = Hello::decode(&bytes).unwrap_err();
    assert!(matches!(err, WireError::UnsupportedFeature(_)));
}
