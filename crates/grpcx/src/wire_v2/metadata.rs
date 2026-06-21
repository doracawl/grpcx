//! Wire-format v2 metadata block (SPEC § 5).
//!
//! Layout per entry:
//! `| name_len u16 LE | value_len u32 LE | name bytes | value bytes |`
//!
//! The decoder is single-pass and zero-copy — entries
//! borrow from the input slice.

use alloc::vec::Vec;

use super::errors::WireError;

/// Per-entry fixed header size: `name_len u16 + value_len u32`.
const ENTRY_HEADER_LEN: usize = 6;

/// Maximum allowed `name_len` value per SPEC § 5.
/// Receivers reject 0 or > `NAME_LEN_MAX` as a protocol
/// error.
pub const NAME_LEN_MAX: u16 = 1024;

/// Borrowed view of one metadata pair within a payload
/// buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataEntry<'a> {
    /// Header name. UTF-8; lowercase ASCII per gRPC
    /// convention.
    pub name: &'a [u8],
    /// Header value. Arbitrary bytes — UTF-8 for textual
    /// headers, binary for `*-bin` named headers.
    pub value: &'a [u8],
}

/// Decode `count` metadata entries from the start of
/// `payload`. Returns the entries plus the remaining
/// payload slice (message body, if any).
///
/// # Errors
///
/// `WireError::TruncatedMetadata` if the slice ends
/// mid-entry, or `WireError::InvalidMetadataNameLen`
/// if any entry's `name_len` is 0 or > `NAME_LEN_MAX`.
pub fn decode_metadata_block<'a>(
    payload: &'a [u8],
    count: u16,
) -> Result<(Vec<MetadataEntry<'a>>, &'a [u8]), WireError> {
    let mut entries: Vec<MetadataEntry<'a>> = Vec::with_capacity(count as usize);
    let mut cursor = payload;

    for _ in 0..count {
        if cursor.len() < ENTRY_HEADER_LEN {
            return Err(WireError::TruncatedMetadata);
        }

        let name_len = u16::from_le_bytes([cursor[0], cursor[1]]);
        let value_len = u32::from_le_bytes([cursor[2], cursor[3], cursor[4], cursor[5]]);

        if name_len == 0 || name_len > NAME_LEN_MAX {
            return Err(WireError::InvalidMetadataNameLen(name_len));
        }

        cursor = &cursor[ENTRY_HEADER_LEN..];

        let name_n = name_len as usize;
        let value_n = value_len as usize;
        let need = name_n
            .checked_add(value_n)
            .ok_or(WireError::TruncatedMetadata)?;
        if cursor.len() < need {
            return Err(WireError::TruncatedMetadata);
        }

        let name = &cursor[..name_n];
        let value = &cursor[name_n..name_n + value_n];
        entries.push(MetadataEntry { name, value });
        cursor = &cursor[need..];
    }

    Ok((entries, cursor))
}

/// Encoded wire size of one metadata entry, in bytes.
#[inline]
pub fn entry_wire_len(name: &[u8], value: &[u8]) -> usize {
    ENTRY_HEADER_LEN + name.len() + value.len()
}

/// Encoded wire size of an entire metadata block, in
/// bytes.
pub fn block_wire_len(entries: &[MetadataEntry<'_>]) -> usize {
    entries
        .iter()
        .map(|e| entry_wire_len(e.name, e.value))
        .sum()
}

/// Write `entries` into the start of `out`. Returns the
/// number of bytes written.
///
/// # Panics
///
/// Panics if `out.len() < block_wire_len(entries)`.
pub fn encode_metadata_block(entries: &[MetadataEntry<'_>], out: &mut [u8]) -> usize {
    let mut off = 0usize;
    for e in entries {
        let nlen = e.name.len() as u16;
        let vlen = e.value.len() as u32;
        out[off..off + 2].copy_from_slice(&nlen.to_le_bytes());
        out[off + 2..off + 6].copy_from_slice(&vlen.to_le_bytes());
        off += ENTRY_HEADER_LEN;
        let nn = e.name.len();
        out[off..off + nn].copy_from_slice(e.name);
        off += nn;
        let vn = e.value.len();
        out[off..off + vn].copy_from_slice(e.value);
        off += vn;
    }
    off
}
