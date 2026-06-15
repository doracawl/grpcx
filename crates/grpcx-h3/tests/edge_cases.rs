//! v0.7 — edge cases pushing branch coverage on the dynamic-
//! aware decode path + the connection error gates.

use grpcx_h3::{
    DynamicTable, Error, Header, decode_field_section, decode_field_section_with_table,
    encode_field_section,
};

#[test]
fn decode_field_section_with_table_empty_works() {
    let t = DynamicTable::new(0);
    // RIC=0, S=0, Base=0; one Indexed Field Line static index=17 (:method GET).
    let wire = [0x00u8, 0x00, 0b1100_0000 | 17];
    let decoded = decode_field_section_with_table(&wire, &t).unwrap();
    assert_eq!(&decoded[0].name[..], b":method");
    assert_eq!(&decoded[0].value[..], b"GET");
}

#[test]
fn decode_field_section_with_table_out_of_range_static_rejected() {
    let t = DynamicTable::new(0);
    // RIC=0, S=0, Base=0; Indexed static with multi-byte index 100 (past 99-entry table).
    let wire = [0x00u8, 0x00, 0b1111_1111, 37];
    assert_eq!(
        decode_field_section_with_table(&wire, &t),
        Err(Error::QpackDecode)
    );
}

#[test]
fn decode_field_section_with_table_truncated_section_prefix_rejected() {
    let t = DynamicTable::new(0);
    // Only one byte (RIC) — missing S|Base byte.
    let wire = [0x00u8];
    assert!(decode_field_section_with_table(&wire, &t).is_err());
    // Empty.
    assert!(decode_field_section_with_table(&[], &t).is_err());
}

#[test]
fn decode_field_section_with_table_literal_huffman_rejected() {
    let t = DynamicTable::new(0);
    let wire = [
        0x00u8,
        0x00,
        0b0010_1001, // Literal w/ Literal Name, name-len=1, H=0
        b'x',
        0b1000_0001, // value: H=1
        b'A',
    ];
    assert_eq!(
        decode_field_section_with_table(&wire, &t),
        Err(Error::QpackDecode)
    );
}

#[test]
fn decode_field_section_with_table_dynamic_no_table_rejected() {
    let t = DynamicTable::new(0);
    // RIC=1, S=0 base=1, then dynamic indexed rel=0 → would
    // require insert_count >= 1, but table is empty.
    let wire = [0x01u8, 0x00, 0b1000_0000];
    assert!(decode_field_section_with_table(&wire, &t).is_err());
}

#[test]
fn decode_field_section_with_table_post_base_name_ref() {
    let mut t = DynamicTable::new(1024);
    t.insert(b"x-trace-id".to_vec(), b"orig".to_vec()).unwrap();
    // RIC=1, S=1 DeltaBase=0 → Base = 1 - 0 - 1 = 0.
    // Post-Base Literal w/ Name Ref pattern: 0 0 0 0 N i_2 i_1 i_0
    // Bit 7=0, 6=0, 5=0, 4=0, 3=N=0, 2..0 = name-index prefix (3-bit).
    // post_base_index=0 → name abs = base + 0 = 0.
    // Then value string with H=0.
    let wire = [
        0x01u8,
        0b1000_0000, // S=1, DeltaBase=0
        0b0000_0000, // Post-Base Literal w/ Name Ref, N=0, name-index=0
        0b0000_0001, // value len=1, H=0
        b'y',
    ];
    let decoded = decode_field_section_with_table(&wire, &t).unwrap();
    assert_eq!(&decoded[0].name[..], b"x-trace-id");
    assert_eq!(&decoded[0].value[..], b"y");
}

#[test]
fn decode_field_section_with_table_dynamic_literal_name_ref_static_table() {
    // Hit the T=1 (static) branch of Literal w/ Name Ref via the
    // dynamic-aware decoder.
    let t = DynamicTable::new(0);
    let wire = [
        0x00u8,
        0x00,
        0b0101_0000 | 5, // Literal w/ Name Ref, T=1 static, name-index=5 (cookie)
        0b0000_0100,     // value len=4
        b'a',
        b'=',
        b'1',
        b'!',
    ];
    let decoded = decode_field_section_with_table(&wire, &t).unwrap();
    assert_eq!(&decoded[0].name[..], b"cookie");
    assert_eq!(&decoded[0].value[..], b"a=1!");
}

#[test]
fn encode_then_decode_with_empty_dynamic_table_static_only() {
    // Round-trip an all-static section through encode + dynamic-
    // aware decode (with empty table). Same end-state as v0.3.
    let headers = vec![
        Header {
            name: b":method".to_vec(),
            value: b"GET".to_vec(),
        },
        Header {
            name: b":scheme".to_vec(),
            value: b"https".to_vec(),
        },
    ];
    let mut wire = Vec::new();
    encode_field_section(&headers, &mut wire).unwrap();
    let t = DynamicTable::new(0);
    let decoded = decode_field_section_with_table(&wire, &t).unwrap();
    assert_eq!(decoded, headers);
    // Static-only decoder agrees.
    let decoded2 = decode_field_section(&wire).unwrap();
    assert_eq!(decoded2, headers);
}

#[test]
fn signed_delta_base_underflow_rejected() {
    let t = DynamicTable::new(0);
    // RIC=0, S=1, DeltaBase=0 → Base = 0 - 0 - 1 = -1 (underflow).
    let wire = [0x00u8, 0b1000_0000];
    assert!(decode_field_section_with_table(&wire, &t).is_err());
}
