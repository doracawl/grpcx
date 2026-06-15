//! v0.7 — peer encoder stream wiring + dynamic-table decode.
//!
//! Loopback test where one side hand-crafts an encoder-instruction
//! stream and the other side parses it into `peer_dynamic_table`,
//! then a field-section with dynamic refs decodes against the
//! reconstructed table.

use grpcx_crypto::TlsConfig;
use grpcx_h3::{
    Connection as H3Connection, DynamicTable, EncoderInstruction, Settings,
    decode_field_section_with_table, encode_encoder_instruction,
};
use grpcx_quic::QuicConnection;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    (cert.der().to_vec(), kp.serialize_der())
}

fn pump_pair(
    a: &mut H3Connection,
    a_in: &[Vec<u8>],
    b: &mut H3Connection,
    b_in: &[Vec<u8>],
) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut a_out: Vec<Vec<u8>> = Vec::new();
    for dg in a_in {
        a_out.extend(a.pump(Some(dg)).unwrap());
    }
    a_out.extend(a.pump(None).unwrap());
    let mut b_out: Vec<Vec<u8>> = Vec::new();
    for dg in b_in {
        b_out.extend(b.pump(Some(dg)).unwrap());
    }
    b_out.extend(b.pump(None).unwrap());
    (a_out, b_out)
}

// ============================================================
// Decode-against-table lib-level KAT (no Connection needed).
// ============================================================

#[test]
fn decode_dynamic_ref_round_trip() {
    // Build a dynamic table with one entry.
    let mut t = DynamicTable::new(1024);
    t.insert(b"x-trace-id".to_vec(), b"abc123".to_vec())
        .unwrap();

    // Hand-craft a field section that references it:
    // RIC=1 (we need that entry inserted), S=0, DeltaBase=0 → Base=1.
    // Then one Indexed Field Line with T=0 (dynamic), rel-index=0.
    // abs_index = base - 1 - rel = 1 - 1 - 0 = 0.
    let wire = [
        0x01u8,      // RIC = 1
        0x00,        // S=0, DeltaBase=0 → Base=1
        0b1000_0000, // Indexed, T=0 dynamic, rel-index=0
    ];
    let decoded = decode_field_section_with_table(&wire, &t).unwrap();
    assert_eq!(decoded.len(), 1);
    assert_eq!(&decoded[0].name[..], b"x-trace-id");
    assert_eq!(&decoded[0].value[..], b"abc123");
}

#[test]
fn decode_literal_with_dynamic_name_ref() {
    let mut t = DynamicTable::new(1024);
    t.insert(b"x-trace-id".to_vec(), b"abc".to_vec()).unwrap();

    // RIC=1, Base=1. Literal w/ Name Ref (T=0 dynamic), rel-index=0.
    // First byte: 0 1 N=0 T=0 iiii → 0100_0000 | rel.
    // Then value string with H=0.
    let wire = [
        0x01u8,
        0x00,
        0b0100_0000, // Literal w/ Name Ref, T=0, rel=0
        0b0000_0011, // Value len=3 (H=0, 7-bit prefix int = 3)
        b'x',
        b'y',
        b'z',
    ];
    let decoded = decode_field_section_with_table(&wire, &t).unwrap();
    assert_eq!(&decoded[0].name[..], b"x-trace-id");
    assert_eq!(&decoded[0].value[..], b"xyz");
}

#[test]
fn decode_post_base_indexed() {
    let mut t = DynamicTable::new(1024);
    t.insert(b"x".to_vec(), b"1".to_vec()).unwrap();
    t.insert(b"y".to_vec(), b"2".to_vec()).unwrap();

    // RIC = 2, Base = 1 (S=0, DeltaBase=1: but wait, S=0 means
    // Base = RIC + DeltaBase, so DeltaBase needs to be -1 → must
    // use S=1). Use S=1, DeltaBase=0 → Base = RIC - 0 - 1 = 1.
    // Then Post-Base Indexed with post_base_index=1 → abs = 1+1 = 2.
    // But abs=2 is out of range (insert_count=2 means abs ∈ {0,1}).
    //
    // Try again: S=1 DeltaBase=1 → Base = 2 - 1 - 1 = 0. Post-Base
    // index=1 → abs = 0+1 = 1.  Entry at abs=1 is (y, 2).
    let wire = [
        0x02u8,      // RIC=2
        0b1000_0001, // S=1, DeltaBase=1 → Base=0
        0b0001_0001, // Post-Base Indexed, post-base-index=1
    ];
    let decoded = decode_field_section_with_table(&wire, &t).unwrap();
    assert_eq!(decoded.len(), 1);
    assert_eq!(&decoded[0].name[..], b"y");
    assert_eq!(&decoded[0].value[..], b"2");
}

#[test]
fn ric_exceeds_insert_count_rejected() {
    let t = DynamicTable::new(1024);
    // RIC = 5 but the table is empty.
    let wire = [0x05u8, 0x00];
    assert!(decode_field_section_with_table(&wire, &t).is_err());
}

#[test]
fn dynamic_ref_to_evicted_entry_rejected() {
    let mut t = DynamicTable::new(70); // fits 2 entries of size 34
    t.insert(b"a".to_vec(), b"1".to_vec()).unwrap();
    t.insert(b"b".to_vec(), b"2".to_vec()).unwrap();
    t.insert(b"c".to_vec(), b"3".to_vec()).unwrap();
    // Now `a` (abs=0) is evicted; insert_count=3.
    // Reference to abs=0: RIC=1 (we only need entry abs<=0 visible),
    // Base=1, dynamic ref rel-index=0 → abs = 1-1-0 = 0 — evicted.
    let wire = [0x01u8, 0x00, 0b1000_0000];
    assert!(decode_field_section_with_table(&wire, &t).is_err());
}

// ============================================================
// Connection-level: peer encoder stream → peer_dynamic_table.
// ============================================================

#[test]
fn peer_encoder_instructions_apply_to_local_table() {
    let (cert_der, key_der) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h3"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h3"]);

    let client_quic = QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).unwrap();
    let server_quic = QuicConnection::new_server(&server_cfg, b"s".to_vec()).unwrap();

    let s = Settings {
        qpack_max_table_capacity: Some(4096),
        ..Settings::default()
    };
    let mut client = H3Connection::new_client(client_quic, s.clone()).unwrap();
    let mut server = H3Connection::new_server(server_quic, s).unwrap();

    // Run pumps until both peers are H/3-ready.
    let mut to_s: Vec<Vec<u8>> = Vec::new();
    let mut to_c: Vec<Vec<u8>> = Vec::new();
    for _ in 0..32 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if client.is_ready() && server.is_ready() {
            break;
        }
    }
    assert!(client.is_ready() && server.is_ready());

    // The client's pump opened a uni-stream for its encoder; we can
    // poke instructions directly onto it via `Connection::write_*`.
    // Easier path: re-use the helper exposed via lib.
    let mut payload = Vec::new();
    encode_encoder_instruction(
        &EncoderInstruction::SetDynamicTableCapacity(4096),
        &mut payload,
    )
    .unwrap();
    encode_encoder_instruction(
        &EncoderInstruction::InsertWithLiteralName {
            name: b"x-trace-id".to_vec(),
            value: b"abc123".to_vec(),
        },
        &mut payload,
    )
    .unwrap();
    encode_encoder_instruction(
        &EncoderInstruction::InsertWithNameReference {
            table_is_static: true,
            name_index: 17, // `:method` row in the static table
            value: b"PATCH".to_vec(),
        },
        &mut payload,
    )
    .unwrap();
    client
        .write_to_qpack_encoder_stream(&payload)
        .expect("write encoder bytes");

    // Pump until server has applied them.
    for _ in 0..16 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if server.peer_dynamic_table_insert_count() >= 2 {
            break;
        }
    }
    assert_eq!(server.peer_dynamic_table_insert_count(), 2);
    assert_eq!(server.peer_dynamic_table_capacity(), 4096);

    // The server should have queued Insert-Count-Increment(2) on
    // its outbound decoder stream.
    for _ in 0..8 {
        let (a, b) = pump_pair(&mut client, &to_c, &mut server, &to_s);
        to_s = a;
        to_c = b;
        if client.local_decoder_stream_bytes_in_flight() == 0 {
            break;
        }
    }
}
