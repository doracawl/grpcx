//! L8 minimum-shape example: import a `.proto` schema, run
//! codegen, then round-trip a small value through the wire-
//! format primitives.
//!
//! Used by:
//! - `BUDGETS.md` BINSIZE measurement: `cargo bloat --release
//!   --example min_pb --crates`.
//! - `cargo run --example min_pb --release` smoke-test as
//!   part of the v0.7 / v8.0 gate.

use grpcx_pb::{ProtoCodec, WireError, WireType};

const SCHEMA: &str = r#"
edition = "2023";
message Greeting {
  string name = 1;
  int32  age  = 2;
}
"#;

// Hand-written `Greeting` matching what v0.7 codegen would
// produce. We embed it manually here so the example doesn't
// depend on an external `build.rs` step — the goal is to
// prove the wire-format primitives + trait surface compose
// end-to-end.
#[derive(Debug, Default, PartialEq, Eq)]
struct Greeting {
    name: String,
    age: i32,
}

impl ProtoCodec for Greeting {
    fn encode(&self, out: &mut Vec<u8>) {
        grpcx_pb::encode_tag(1, WireType::LengthDelimited, out);
        grpcx_pb::encode_length_delimited(self.name.as_bytes(), out);
        grpcx_pb::encode_tag(2, WireType::Varint, out);
        grpcx_pb::encode_varint(self.age as i64 as u64, out);
    }

    fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut out = Self::default();
        let mut pos = 0;
        while pos < bytes.len() {
            let (field, wire, n) = grpcx_pb::decode_tag(&bytes[pos..])?;
            pos += n;
            match (field, wire) {
                (1, WireType::LengthDelimited) => {
                    let (s, n) = grpcx_pb::decode_string(&bytes[pos..])?;
                    out.name = s;
                    pos += n;
                }
                (2, WireType::Varint) => {
                    let (raw, n) = grpcx_pb::decode_varint(&bytes[pos..])?;
                    out.age = raw as i64 as i32;
                    pos += n;
                }
                (_, wt) => {
                    let n = grpcx_pb::skip_field(wt, &bytes[pos..])?;
                    pos += n;
                }
            }
        }
        Ok(out)
    }
}

fn main() {
    // 1. Lex + parse + semantic analyse the schema, then
    //    codegen Rust source for it. We don't compile the
    //    generated source in the example — the goal is to
    //    exercise the import + codegen path so `cargo bloat`
    //    measures realistic L8 surface area.
    let schema = grpcx_pb::import(SCHEMA).expect("schema parses");
    let _rust_source = grpcx_pb::codegen(&schema);

    // 2. Round-trip a representative Greeting via the wire
    //    primitives.
    let original = Greeting {
        name: "Alice".to_string(),
        age: 30,
    };
    let bytes = grpcx_pb::to_proto_bytes(&original);
    assert_eq!(
        bytes,
        [0x0A, 0x05, b'A', b'l', b'i', b'c', b'e', 0x10, 0x1E]
    );

    let decoded: Greeting = grpcx_pb::from_proto_bytes(&bytes).expect("round-trip succeeds");
    assert_eq!(decoded, original);
}
