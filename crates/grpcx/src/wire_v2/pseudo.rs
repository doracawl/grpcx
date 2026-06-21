//! Pre-baked pseudo-header byte sequences and stack-
//! buffer encoders for the v2 wire format hot path.
//!
//! `transport-uds` / `transport-shmem` / `transport-tcp-
//! mux` v2 BEGIN frames always carry the same three
//! pseudo-headers (`:service` / `:method` / `:kind`) and
//! their END frames almost always carry `:status = OK`.
//! On the bench unary echo path (`Metadata::default()`),
//! these are the **only** metadata entries — and the
//! per-call `Vec<MetadataEntry>` + `vec![0u8; meta_len]`
//! pair was ~200–400 ns of pure heap traffic that the
//! `encode_metadata_block` byte-walk did not need.
//!
//! This module trades that for: stack-allocated 128-byte
//! buffers for the common case, plus a fully pre-computed
//! `:status = OK` END block.

use crate::wire_v2::metadata::{MetadataEntry, encode_metadata_block};

/// Pseudo-header `:service` (BEGIN).
pub const PSEUDO_SERVICE: &[u8] = b":service";
/// Pseudo-header `:method` (BEGIN).
pub const PSEUDO_METHOD: &[u8] = b":method";
/// Pseudo-header `:kind` (BEGIN). Value is a single byte
/// per [`crate::wire_v2`] SPEC § 4.1.
pub const PSEUDO_KIND: &[u8] = b":kind";
/// Pseudo-header `:status` (END / RESET). Value is a 4-B
/// little-endian `StatusCode`.
pub const PSEUDO_STATUS: &[u8] = b":status";
/// Pseudo-header `:status-msg` (END / RESET). UTF-8.
pub const PSEUDO_STATUS_MSG: &[u8] = b":status-msg";

/// `:status = 0u32 LE` (StatusCode::Ok) — the entire
/// 1-entry metadata block, ready to splice into an END
/// frame. Layout: `name_len u16 LE | value_len u32 LE |
/// ":status" | 0u32 LE` = `6 + 7 + 4 = 17` bytes.
pub const STATUS_OK_BLOCK: [u8; 17] = [
    // name_len = 7
    0x07, 0x00, // value_len = 4
    0x04, 0x00, 0x00, 0x00, // name = ":status"
    b':', b's', b't', b'a', b't', b'u', b's', // value = 0u32 LE (StatusCode::Ok)
    0x00, 0x00, 0x00, 0x00,
];

/// Encoded byte length the BEGIN metadata block needs
/// for `(service, method, kind)` plus the supplied user
/// entries (typically empty on the bench fast path).
#[inline]
pub fn begin_block_len(service: &[u8], method: &[u8], user_entries: &[MetadataEntry<'_>]) -> usize {
    // service entry: 6 + 8 ("::service") + service.len()
    // method  entry: 6 + 7 ("::method")  + method.len()
    // kind    entry: 6 + 5 ("::kind")    + 1 (kind byte)
    let pseudo = 6 + 8 + service.len() + 6 + 7 + method.len() + 6 + 5 + 1;
    let user: usize = user_entries
        .iter()
        .map(|e| 6 + e.name.len() + e.value.len())
        .sum();
    pseudo + user
}

/// Write a BEGIN metadata block — `:service`, `:method`,
/// `:kind`, then any user entries — into `out`. Returns
/// the byte count written. The caller is expected to size
/// `out` via [`begin_block_len`] (or just allocate 128 B
/// on the stack for the common empty-user-meta case).
///
/// # Panics
///
/// Panics if `out.len() < begin_block_len(...)`.
pub fn encode_begin_block(
    out: &mut [u8],
    service: &[u8],
    method: &[u8],
    kind_byte: u8,
    user_entries: &[MetadataEntry<'_>],
) -> usize {
    let kind_value = [kind_byte];
    // Hand-encode the three pseudo-header entries without
    // going through a `Vec<MetadataEntry>` — this is the
    // bench hot path. User entries (cold) reuse the
    // generic encoder.
    let mut off = 0usize;

    off += write_entry(&mut out[off..], PSEUDO_SERVICE, service);
    off += write_entry(&mut out[off..], PSEUDO_METHOD, method);
    off += write_entry(&mut out[off..], PSEUDO_KIND, &kind_value);

    if !user_entries.is_empty() {
        off += encode_metadata_block(user_entries, &mut out[off..]);
    }
    off
}

#[inline]
fn write_entry(out: &mut [u8], name: &[u8], value: &[u8]) -> usize {
    let nlen = name.len() as u16;
    let vlen = value.len() as u32;
    out[0..2].copy_from_slice(&nlen.to_le_bytes());
    out[2..6].copy_from_slice(&vlen.to_le_bytes());
    let after_header = 6;
    out[after_header..after_header + name.len()].copy_from_slice(name);
    let after_name = after_header + name.len();
    out[after_name..after_name + value.len()].copy_from_slice(value);
    after_name + value.len()
}

/// Pseudo-header metadata count carried in every BEGIN
/// (`:service` + `:method` + `:kind` = 3).
pub const BEGIN_PSEUDO_COUNT: u16 = 3;

/// Number of metadata entries in [`STATUS_OK_BLOCK`].
pub const STATUS_OK_COUNT: u16 = 1;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire_v2::metadata::{block_wire_len, decode_metadata_block};

    #[test]
    fn status_ok_block_roundtrips() {
        let (entries, rest) =
            decode_metadata_block(&STATUS_OK_BLOCK, STATUS_OK_COUNT).expect("decode");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, PSEUDO_STATUS);
        assert_eq!(entries[0].value, &[0, 0, 0, 0]);
        assert!(rest.is_empty());
    }

    #[test]
    fn begin_block_matches_generic_encoder() {
        let mut stack = [0u8; 128];
        let n = encode_begin_block(&mut stack, b"bench.Echo", b"echo", 0, &[]);

        let entries = [
            MetadataEntry {
                name: PSEUDO_SERVICE,
                value: b"bench.Echo",
            },
            MetadataEntry {
                name: PSEUDO_METHOD,
                value: b"echo",
            },
            MetadataEntry {
                name: PSEUDO_KIND,
                value: &[0u8],
            },
        ];
        let expected_len = block_wire_len(&entries);
        let mut expected = alloc::vec![0u8; expected_len];
        encode_metadata_block(&entries, &mut expected);

        assert_eq!(n, expected_len);
        assert_eq!(&stack[..n], &expected[..]);
        assert_eq!(begin_block_len(b"bench.Echo", b"echo", &[]), expected_len);
    }

    #[test]
    fn begin_block_with_user_entries_matches() {
        let user = [MetadataEntry {
            name: b"x-trace",
            value: b"abc",
        }];
        let mut stack = [0u8; 128];
        let n = encode_begin_block(&mut stack, b"svc", b"m", 1, &user);

        let entries = [
            MetadataEntry {
                name: PSEUDO_SERVICE,
                value: b"svc",
            },
            MetadataEntry {
                name: PSEUDO_METHOD,
                value: b"m",
            },
            MetadataEntry {
                name: PSEUDO_KIND,
                value: &[1u8],
            },
            user[0],
        ];
        let expected_len = block_wire_len(&entries);
        let mut expected = alloc::vec![0u8; expected_len];
        encode_metadata_block(&entries, &mut expected);

        assert_eq!(n, expected_len);
        assert_eq!(&stack[..n], &expected[..]);
        assert_eq!(begin_block_len(b"svc", b"m", &user), expected_len);
    }
}
