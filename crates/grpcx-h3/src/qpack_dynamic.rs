//! QPACK dynamic table primitives + encoder / decoder
//! instruction codecs — RFC 9204 §3.2 + §4.3 + §4.4.
//!
//! Three pieces ship in v0.6:
//! 1. [`DynamicTable`] — the per-direction ordered store with
//!    capacity + eviction.
//! 2. [`EncoderInstruction`] + [`decode_encoder_instruction`] /
//!    [`encode_encoder_instruction`] — the wire codec for the
//!    four messages the encoder pushes on its uni stream
//!    (Set-Capacity / Insert-with-Name-Ref / Insert-with-Literal-
//!    Name / Duplicate).
//! 3. [`DecoderInstruction`] + [`decode_decoder_instruction`] /
//!    [`encode_decoder_instruction`] — the wire codec for the
//!    three messages the decoder pushes back (Section-Ack /
//!    Stream-Cancel / Insert-Count-Increment).
//!
//! Deferred to v0.7 (per the v0.6 plan):
//! - Wiring this into [`crate::Connection`]'s pump (parsing
//!   peer encoder instructions, emitting Insert-Count-Increment,
//!   etc.).
//! - Extending [`crate::decode_field_section`] to resolve
//!   dynamic-table references + Post-Base patterns against a
//!   passed-in [`DynamicTable`].
//! - Encoder-side strategy (when to insert vs literal-emit,
//!   when to reference a dynamic entry).
//!
//! Per the v0.3 deferral, encoder-side Huffman is still off; the
//! decoder rejects `H=1` on any string field. RFC 9204 §3.1
//! mandates that the dynamic-table entry's size accounting use
//! the **uncompressed** name + value byte lengths — Huffman
//! deferral has zero effect on the size math.

use crate::Error;
use crate::qpack::{read_prefixed_int, read_string, write_prefixed_int, write_string};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

// ============================================================
// DynamicTable.
// ============================================================

/// One entry in the [`DynamicTable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicEntry {
    /// Header name.
    pub name: Vec<u8>,
    /// Header value.
    pub value: Vec<u8>,
}

impl DynamicEntry {
    /// Wire size for capacity accounting per RFC 9204 §3.2.1:
    /// `32 + name.len() + value.len()` (regardless of Huffman).
    pub fn size(&self) -> u64 {
        32 + self.name.len() as u64 + self.value.len() as u64
    }
}

/// QPACK dynamic table — per-direction. Tracks the encoder's
/// view (on the encoder side) or the decoder's reconstruction
/// (on the decoder side); both sides keep their tables in
/// lock-step via the encoder's instruction stream.
///
/// Entries are inserted at the head. The newest entry has
/// relative index `0`; the oldest has `entries.len() - 1`.
/// Absolute indices grow monotonically with insertions and
/// never recycle — even after eviction, an entry's absolute
/// index is permanent (this is how Section Acknowledgement
/// + Insert Count Increment stay unambiguous across evictions).
#[derive(Debug, Clone, Default)]
pub struct DynamicTable {
    entries: VecDeque<DynamicEntry>,
    capacity: u64,
    current_size: u64,
    /// Total number of insertions ever performed (RFC 9204
    /// §3.2.4 "Insert Count"). Equal to `entries.len() +
    /// total_evicted_count`.
    insert_count: u64,
    /// `insert_count - entries.len()` — i.e. the absolute index
    /// of the oldest entry currently held.
    evicted_count: u64,
}

impl DynamicTable {
    /// Build an empty table with the given byte capacity. Pass
    /// `0` to keep the table disabled until the first explicit
    /// `set_capacity`.
    pub fn new(capacity: u64) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity,
            current_size: 0,
            insert_count: 0,
            evicted_count: 0,
        }
    }

    /// Current byte capacity (the cap on `current_size`).
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Bytes currently consumed by the table's entries (sum of
    /// `entry.size()` for each entry).
    pub fn current_size(&self) -> u64 {
        self.current_size
    }

    /// Total entries ever inserted (RFC 9204 §3.2.4). Section
    /// Acknowledgement / Insert Count Increment count against
    /// this.
    pub fn insert_count(&self) -> u64 {
        self.insert_count
    }

    /// Number of entries currently held (≤ `insert_count`).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if the table holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Update the table's byte capacity. Per RFC 9204 §3.2.3,
    /// reducing the capacity may force eviction; the eviction
    /// loop runs immediately.
    pub fn set_capacity(&mut self, new_capacity: u64) {
        self.capacity = new_capacity;
        self.evict_until_fits(0);
    }

    /// Look up an entry by absolute index (RFC 9204 §3.2.4).
    /// Returns `None` if the absolute index is out of range
    /// (either past the highest insertion or evicted).
    pub fn get_absolute(&self, abs_index: u64) -> Option<&DynamicEntry> {
        if abs_index < self.evicted_count || abs_index >= self.insert_count {
            return None;
        }
        let local_idx = abs_index - self.evicted_count;
        // entries[0] is the OLDEST currently held — i.e. the one
        // at absolute index `evicted_count`. The newest is at
        // entries[len - 1].
        self.entries.get(local_idx as usize)
    }

    /// Look up an entry by relative index — RFC 9204 §3.2.5.
    /// Relative index `0` is the most recently inserted entry.
    /// Returns `None` if `rel_index >= entries.len()`.
    pub fn get_relative(&self, rel_index: u64) -> Option<&DynamicEntry> {
        if rel_index >= self.entries.len() as u64 {
            return None;
        }
        // Newest is at the back of the deque.
        let local_idx = self.entries.len() - 1 - rel_index as usize;
        self.entries.get(local_idx)
    }

    /// Search for an exact `(name, value)` match. Returns the
    /// absolute index of the first match (newest first;
    /// matches the lookup order encoders prefer for compactness).
    pub fn find(&self, name: &[u8], value: &[u8]) -> Option<u64> {
        // Walk newest-first (back-to-front).
        for (offset_from_newest, entry) in self.entries.iter().rev().enumerate() {
            if entry.name == name && entry.value == value {
                let rel = offset_from_newest as u64;
                return Some(self.insert_count - 1 - rel);
            }
        }
        None
    }

    /// Search for a name-only match (any value). Returns the
    /// absolute index of the newest match.
    pub fn find_name(&self, name: &[u8]) -> Option<u64> {
        for (offset_from_newest, entry) in self.entries.iter().rev().enumerate() {
            if entry.name == name {
                let rel = offset_from_newest as u64;
                return Some(self.insert_count - 1 - rel);
            }
        }
        None
    }

    /// Insert `(name, value)` at the head. Evicts older entries
    /// as needed to fit; returns the absolute index of the new
    /// entry on success.
    ///
    /// Errors with [`Error::QpackEncode`] if the entry's
    /// `size()` exceeds the table capacity (i.e. it can never
    /// fit, even after evicting everything).
    pub fn insert(&mut self, name: Vec<u8>, value: Vec<u8>) -> Result<u64, Error> {
        let entry = DynamicEntry { name, value };
        let entry_size = entry.size();
        if entry_size > self.capacity {
            return Err(Error::QpackEncode);
        }
        self.evict_until_fits(entry_size);
        let abs = self.insert_count;
        self.entries.push_back(entry);
        self.current_size += entry_size;
        self.insert_count += 1;
        Ok(abs)
    }

    /// Duplicate the entry at relative index `rel_index` (RFC
    /// 9204 §4.3.4). Subject to the same eviction + size rules
    /// as a fresh insert.
    pub fn duplicate(&mut self, rel_index: u64) -> Result<u64, Error> {
        let src = self.get_relative(rel_index).ok_or(Error::QpackEncode)?;
        let cloned = src.clone();
        self.insert(cloned.name, cloned.value)
    }

    fn evict_until_fits(&mut self, incoming_size: u64) {
        while self.current_size + incoming_size > self.capacity {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.current_size = self.current_size.saturating_sub(old.size());
            self.evicted_count += 1;
        }
    }
}

// ============================================================
// Encoder instructions — RFC 9204 §4.3.
// ============================================================

/// One QPACK encoder-stream instruction.
///
/// The encoder side pushes these on its outbound encoder uni
/// stream (RFC 9114 §6.2.2 + RFC 9204 §4.2.1). The decoder side
/// parses them to keep its dynamic-table reconstruction in
/// lock-step with the encoder's table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncoderInstruction {
    /// §4.3.1 — set the dynamic-table byte capacity.
    SetDynamicTableCapacity(u64),
    /// §4.3.2 — insert a new entry referencing an existing
    /// name (either from the static table when `table_is_static`
    /// is `true`, or from the dynamic table at relative index
    /// `name_index` when `false`).
    InsertWithNameReference {
        /// `true` = name comes from the QPACK static table;
        /// `false` = relative index into the dynamic table.
        table_is_static: bool,
        /// Static-table absolute index or dynamic-table relative
        /// index, depending on `table_is_static`.
        name_index: u64,
        /// Value bytes.
        value: Vec<u8>,
    },
    /// §4.3.3 — insert a new entry with both name + value as
    /// literals.
    InsertWithLiteralName {
        /// Name bytes.
        name: Vec<u8>,
        /// Value bytes.
        value: Vec<u8>,
    },
    /// §4.3.4 — duplicate the dynamic-table entry at relative
    /// index `rel_index`.
    Duplicate(u64),
}

/// Decode one encoder instruction from the front of `buf`,
/// advancing `buf` past the consumed bytes.
pub fn decode_encoder_instruction(buf: &mut &[u8]) -> Result<EncoderInstruction, Error> {
    if buf.is_empty() {
        return Err(Error::QpackDecode);
    }
    let first = buf[0];
    *buf = &buf[1..];
    if first & 0b1000_0000 != 0 {
        // 1Txxxxxx — Insert with Name Reference.
        let table_is_static = first & 0b0100_0000 != 0;
        let name_index = read_prefixed_int(buf, 6, first & 0b0011_1111)?;
        // Value: H xxxxxxx — 7-bit prefix length.
        if buf.is_empty() {
            return Err(Error::QpackDecode);
        }
        let v_first = buf[0];
        *buf = &buf[1..];
        let value = read_string(buf, 0b1000_0000, 7, v_first)?;
        return Ok(EncoderInstruction::InsertWithNameReference {
            table_is_static,
            name_index,
            value,
        });
    }
    if first & 0b0100_0000 != 0 {
        // 01Hxxxxx — Insert with Literal Name. The H bit is at
        // bit 5; the 5-bit prefix integer is bits 4-0.
        let name = read_string(buf, 0b0010_0000, 5, first & 0b0011_1111)?;
        if buf.is_empty() {
            return Err(Error::QpackDecode);
        }
        let v_first = buf[0];
        *buf = &buf[1..];
        let value = read_string(buf, 0b1000_0000, 7, v_first)?;
        return Ok(EncoderInstruction::InsertWithLiteralName { name, value });
    }
    if first & 0b0010_0000 != 0 {
        // 001xxxxx — Set Dynamic Table Capacity.
        let cap = read_prefixed_int(buf, 5, first & 0b0001_1111)?;
        return Ok(EncoderInstruction::SetDynamicTableCapacity(cap));
    }
    // 000xxxxx — Duplicate.
    let rel = read_prefixed_int(buf, 5, first & 0b0001_1111)?;
    Ok(EncoderInstruction::Duplicate(rel))
}

/// Encode one encoder instruction into `out`.
pub fn encode_encoder_instruction(
    inst: &EncoderInstruction,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    match inst {
        EncoderInstruction::SetDynamicTableCapacity(cap) => {
            // 001xxxxx + 5-bit prefix int.
            write_prefixed_int(*cap, 5, 0b0010_0000, out)
        }
        EncoderInstruction::InsertWithNameReference {
            table_is_static,
            name_index,
            value,
        } => {
            // 1Txxxxxx + 6-bit prefix int for name index.
            let flags = if *table_is_static {
                0b1100_0000
            } else {
                0b1000_0000
            };
            write_prefixed_int(*name_index, 6, flags, out)?;
            // Value: H=0, 7-bit prefix.
            write_string(value, 7, 0b0000_0000, out)
        }
        EncoderInstruction::InsertWithLiteralName { name, value } => {
            // 01Hxxxxx + 5-bit prefix int for name length. H=0
            // for v0.6 (encoder defers Huffman to v1.x).
            write_string(name, 5, 0b0100_0000, out)?;
            write_string(value, 7, 0b0000_0000, out)
        }
        EncoderInstruction::Duplicate(rel) => {
            // 000xxxxx + 5-bit prefix int.
            write_prefixed_int(*rel, 5, 0b0000_0000, out)
        }
    }
}

// ============================================================
// Decoder instructions — RFC 9204 §4.4.
// ============================================================

/// One QPACK decoder-stream instruction.
///
/// The decoder side pushes these on its outbound decoder uni
/// stream (RFC 9114 §6.2.2 + RFC 9204 §4.2.2). The encoder side
/// parses them for flow-control + table-management feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderInstruction {
    /// §4.4.1 — confirms that the decoder has fully processed
    /// the header section on `stream_id`. The encoder may then
    /// retire the references that section held into the
    /// dynamic table.
    SectionAcknowledgement {
        /// Bidi request-stream id the ack covers.
        stream_id: u64,
    },
    /// §4.4.2 — the decoder is dropping the stream; the encoder
    /// should release any references it was holding for it.
    StreamCancellation {
        /// Bidi request-stream id being cancelled.
        stream_id: u64,
    },
    /// §4.4.3 — the decoder has fully processed `increment`
    /// additional dynamic-table insertions beyond its previous
    /// Insert Count high-water mark.
    InsertCountIncrement {
        /// Number of newly-acknowledged entries.
        increment: u64,
    },
}

/// Decode one decoder instruction from the front of `buf`,
/// advancing `buf` past the consumed bytes.
pub fn decode_decoder_instruction(buf: &mut &[u8]) -> Result<DecoderInstruction, Error> {
    if buf.is_empty() {
        return Err(Error::QpackDecode);
    }
    let first = buf[0];
    *buf = &buf[1..];
    if first & 0b1000_0000 != 0 {
        // 1xxxxxxx — Section Acknowledgement, 7-bit prefix.
        let stream_id = read_prefixed_int(buf, 7, first & 0b0111_1111)?;
        return Ok(DecoderInstruction::SectionAcknowledgement { stream_id });
    }
    if first & 0b0100_0000 != 0 {
        // 01xxxxxx — Stream Cancellation, 6-bit prefix.
        let stream_id = read_prefixed_int(buf, 6, first & 0b0011_1111)?;
        return Ok(DecoderInstruction::StreamCancellation { stream_id });
    }
    // 00xxxxxx — Insert Count Increment, 6-bit prefix.
    let increment = read_prefixed_int(buf, 6, first & 0b0011_1111)?;
    Ok(DecoderInstruction::InsertCountIncrement { increment })
}

/// Encode one decoder instruction into `out`.
pub fn encode_decoder_instruction(
    inst: &DecoderInstruction,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    match inst {
        DecoderInstruction::SectionAcknowledgement { stream_id } => {
            write_prefixed_int(*stream_id, 7, 0b1000_0000, out)
        }
        DecoderInstruction::StreamCancellation { stream_id } => {
            write_prefixed_int(*stream_id, 6, 0b0100_0000, out)
        }
        DecoderInstruction::InsertCountIncrement { increment } => {
            write_prefixed_int(*increment, 6, 0b0000_0000, out)
        }
    }
}

// ============================================================
// Tests.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: &[u8], v: &[u8]) -> DynamicEntry {
        DynamicEntry {
            name: n.to_vec(),
            value: v.to_vec(),
        }
    }

    #[test]
    fn dynamic_table_insert_lookup_basic() {
        let mut t = DynamicTable::new(1024);
        let a = t.insert(b":path".to_vec(), b"/api".to_vec()).unwrap();
        let b = t
            .insert(b":authority".to_vec(), b"example.com".to_vec())
            .unwrap();
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(t.insert_count(), 2);
        assert_eq!(t.len(), 2);

        // Relative: newest = 0.
        assert_eq!(
            t.get_relative(0),
            Some(&entry(b":authority", b"example.com"))
        );
        assert_eq!(t.get_relative(1), Some(&entry(b":path", b"/api")));

        // Absolute matches insertion order.
        assert_eq!(t.get_absolute(0), Some(&entry(b":path", b"/api")));
        assert_eq!(
            t.get_absolute(1),
            Some(&entry(b":authority", b"example.com"))
        );

        // find / find_name return absolute indices.
        assert_eq!(t.find(b":path", b"/api"), Some(0));
        assert_eq!(t.find(b":authority", b"example.com"), Some(1));
        assert_eq!(t.find(b"missing", b"x"), None);
        assert_eq!(t.find_name(b":path"), Some(0));
        assert_eq!(t.find_name(b"missing"), None);
    }

    #[test]
    fn dynamic_table_eviction_on_capacity_overflow() {
        // Each entry: 32 + 1 + 1 = 34 bytes. Capacity 70 fits
        // exactly 2 entries (68 bytes used).
        let mut t = DynamicTable::new(70);
        let a = t.insert(b"a".to_vec(), b"1".to_vec()).unwrap();
        let b = t.insert(b"b".to_vec(), b"2".to_vec()).unwrap();
        assert_eq!(t.current_size(), 68);
        assert_eq!(t.len(), 2);
        // Insert a third: must evict `a` (the oldest).
        let c = t.insert(b"c".to_vec(), b"3".to_vec()).unwrap();
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(c, 2);
        assert_eq!(t.len(), 2);
        assert_eq!(t.current_size(), 68);
        // Absolute index 0 is gone now.
        assert!(t.get_absolute(0).is_none());
        assert_eq!(t.get_absolute(1), Some(&entry(b"b", b"2")));
        assert_eq!(t.get_absolute(2), Some(&entry(b"c", b"3")));
        assert_eq!(t.insert_count(), 3);
    }

    #[test]
    fn dynamic_table_oversize_entry_rejected() {
        let mut t = DynamicTable::new(50);
        // entry size = 32 + 30 + 30 = 92 > 50 → reject.
        let huge_name = vec![b'n'; 30];
        let huge_val = vec![b'v'; 30];
        assert_eq!(t.insert(huge_name, huge_val), Err(Error::QpackEncode));
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn dynamic_table_set_capacity_evicts() {
        let mut t = DynamicTable::new(200);
        t.insert(b"a".to_vec(), b"1".to_vec()).unwrap();
        t.insert(b"b".to_vec(), b"2".to_vec()).unwrap();
        t.insert(b"c".to_vec(), b"3".to_vec()).unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t.current_size(), 102);
        // Shrink to fit only 1 entry (34 bytes).
        t.set_capacity(40);
        assert_eq!(t.len(), 1);
        assert_eq!(t.current_size(), 34);
        // The remaining entry is the newest (c).
        assert_eq!(t.get_relative(0), Some(&entry(b"c", b"3")));
    }

    #[test]
    fn dynamic_table_duplicate() {
        let mut t = DynamicTable::new(1024);
        t.insert(b":path".to_vec(), b"/api".to_vec()).unwrap();
        let abs = t.duplicate(0).unwrap();
        assert_eq!(abs, 1);
        assert_eq!(t.len(), 2);
        assert_eq!(t.get_relative(0), Some(&entry(b":path", b"/api")));
        assert_eq!(t.get_relative(1), Some(&entry(b":path", b"/api")));
    }

    #[test]
    fn encoder_instruction_set_capacity_round_trip() {
        for cap in [0u64, 1, 30, 31, 1000, 1u64 << 20] {
            let inst = EncoderInstruction::SetDynamicTableCapacity(cap);
            let mut out = Vec::new();
            encode_encoder_instruction(&inst, &mut out).unwrap();
            let mut cursor: &[u8] = &out;
            assert_eq!(decode_encoder_instruction(&mut cursor).unwrap(), inst);
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn encoder_instruction_insert_with_name_ref_round_trip() {
        for table_is_static in [true, false] {
            for name_index in [0u64, 1, 50, 200] {
                let inst = EncoderInstruction::InsertWithNameReference {
                    table_is_static,
                    name_index,
                    value: b"example.com".to_vec(),
                };
                let mut out = Vec::new();
                encode_encoder_instruction(&inst, &mut out).unwrap();
                let mut cursor: &[u8] = &out;
                assert_eq!(decode_encoder_instruction(&mut cursor).unwrap(), inst);
                assert!(cursor.is_empty());
            }
        }
    }

    #[test]
    fn encoder_instruction_insert_with_literal_name_round_trip() {
        let inst = EncoderInstruction::InsertWithLiteralName {
            name: b"x-custom-header".to_vec(),
            value: b"v".to_vec(),
        };
        let mut out = Vec::new();
        encode_encoder_instruction(&inst, &mut out).unwrap();
        let mut cursor: &[u8] = &out;
        assert_eq!(decode_encoder_instruction(&mut cursor).unwrap(), inst);
        assert!(cursor.is_empty());
    }

    #[test]
    fn encoder_instruction_duplicate_round_trip() {
        for rel in [0u64, 1, 31, 1000] {
            let inst = EncoderInstruction::Duplicate(rel);
            let mut out = Vec::new();
            encode_encoder_instruction(&inst, &mut out).unwrap();
            let mut cursor: &[u8] = &out;
            assert_eq!(decode_encoder_instruction(&mut cursor).unwrap(), inst);
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn encoder_instruction_huffman_rejected() {
        // 01Hxxxxx with H=1, name-len=1; then "a" + value.
        let wire = [0b0110_0001, b'a', 0b0000_0001, b'v'];
        let mut cursor: &[u8] = &wire;
        assert_eq!(
            decode_encoder_instruction(&mut cursor),
            Err(Error::QpackDecode)
        );
    }

    #[test]
    fn decoder_instruction_section_ack_round_trip() {
        for sid in [0u64, 1, 4, 100, 127, 128, 1u64 << 20] {
            let inst = DecoderInstruction::SectionAcknowledgement { stream_id: sid };
            let mut out = Vec::new();
            encode_decoder_instruction(&inst, &mut out).unwrap();
            let mut cursor: &[u8] = &out;
            assert_eq!(decode_decoder_instruction(&mut cursor).unwrap(), inst);
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn decoder_instruction_stream_cancel_round_trip() {
        for sid in [0u64, 1, 4, 100, 1u64 << 20] {
            let inst = DecoderInstruction::StreamCancellation { stream_id: sid };
            let mut out = Vec::new();
            encode_decoder_instruction(&inst, &mut out).unwrap();
            let mut cursor: &[u8] = &out;
            assert_eq!(decode_decoder_instruction(&mut cursor).unwrap(), inst);
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn decoder_instruction_insert_count_increment_round_trip() {
        for inc in [0u64, 1, 50, 63, 64, 1u64 << 20] {
            let inst = DecoderInstruction::InsertCountIncrement { increment: inc };
            let mut out = Vec::new();
            encode_decoder_instruction(&inst, &mut out).unwrap();
            let mut cursor: &[u8] = &out;
            assert_eq!(decode_decoder_instruction(&mut cursor).unwrap(), inst);
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn empty_buf_rejected_both_codecs() {
        let mut e: &[u8] = &[];
        assert_eq!(decode_encoder_instruction(&mut e), Err(Error::QpackDecode));
        let mut d: &[u8] = &[];
        assert_eq!(decode_decoder_instruction(&mut d), Err(Error::QpackDecode));
    }
}
