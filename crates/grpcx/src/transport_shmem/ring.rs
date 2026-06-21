//! SPSC variable-stride ringbuf carved out of a shared
//! [`super::region::Region`]. Two `Ring`s per
//! connection — one per direction.
//!
//! Lock-free Vyukov-style:
//! - Writer reads `reader_pos` with Acquire; writes
//!   slot then publishes `writer_pos` with Release.
//! - Reader reads `writer_pos` with Acquire; reads
//!   slot then publishes `reader_pos` with Release.
//!
//! No CAS, no fences beyond the AcqRel pair.
//!
//! v2 wire-format frame per slot:
//!   `[ 16-byte header | payload_len bytes payload ]`
//!
//! v1 limit: one frame per slot (no `CONTINUATION`).
//! Frame payload ≤ `SLOT_STRIDE - HEADER_LEN`. Larger
//! frames return `RingError::FrameTooLarge` —
//! `CONTINUATION` lands in a T3.2.x perf push.

use core::sync::atomic::{AtomicU64, Ordering};

use alloc::vec::Vec;

use super::region::Region;
use crate::wire_v2::{HEADER_LEN, Header, decode_header, encode_header};

/// Slot stride (per-frame storage) — page-sized for hot-
/// cache alignment.
pub const SLOT_STRIDE: usize = 4096;

/// Slots per ring direction. Power of two so the modulo
/// becomes an `& MASK` — single instruction. v1.3 bumped
/// the depth from 4 → 16 to give the producer enough
/// headroom for busy-poll-only throughput (`Server::
/// listen_shmem_busy_poll_until`) without spinning on
/// `Full` every fourth push. 16 chosen as the depth at
/// which throughput plateaus on lx64 but RTT cache
/// footprint stays inside ~64 KiB per direction.
pub const SLOTS_PER_RING: usize = 16;

/// Cache-line size assumed for the AcqRel padding.
const CACHE_LINE: usize = 64;

/// Total cache-line padded control bytes (4 × cache line:
/// c2s.writer / c2s.reader / s2c.writer / s2c.reader).
pub const CONTROL_BYTES: usize = 4 * CACHE_LINE;
/// Bytes occupied by the slot data of one direction.
pub const RING_DATA_BYTES: usize = SLOTS_PER_RING * SLOT_STRIDE;
/// Byte offset of the c2s slot data inside the region.
pub const C2S_DATA_OFFSET: usize = CONTROL_BYTES;
/// Byte offset of the s2c slot data inside the region.
pub const S2C_DATA_OFFSET: usize = CONTROL_BYTES + RING_DATA_BYTES;
/// Minimum region bytes required for a ring pair.
pub const MIN_REGION_BYTES: usize = CONTROL_BYTES + 2 * RING_DATA_BYTES;

/// Errors raised by the ring producer / consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingError {
    /// Producer: writer would advance past reader.
    Full,
    /// Consumer: writer hasn't published a new slot yet.
    Empty,
    /// Producer: requested frame doesn't fit in one slot.
    FrameTooLarge,
    /// Consumer: header bytes inside the slot don't decode.
    BadHeader,
}

/// One direction of the SPSC ring. Side-agnostic — the
/// `producer` and `consumer` both operate on the same
/// struct; the caller knows which role it plays.
pub struct Ring {
    /// Raw pointer into shmem for `writer_pos`.
    writer_pos: *mut AtomicU64,
    /// Raw pointer into shmem for `reader_pos`.
    reader_pos: *mut AtomicU64,
    /// Pointer to the first slot byte inside shmem.
    slots: *mut u8,
}

// `Ring` is a (Send, Sync) handle into shared memory;
// the SPSC invariant lives in the caller.
unsafe impl Send for Ring {}
unsafe impl Sync for Ring {}

impl Ring {
    /// Construct a producer / consumer handle for one
    /// direction of a region. `data_off` is the byte
    /// offset of the first slot inside the region.
    ///
    /// # Safety
    ///
    /// `region` must outlive every [`Ring`] derived from
    /// it; the caller commits to the SPSC invariant
    /// (at most one producer and one consumer per Ring,
    /// per direction).
    pub unsafe fn new_in_region(
        region: &Region,
        wpos_off: usize,
        rpos_off: usize,
        data_off: usize,
    ) -> Self {
        let base = region.as_ptr();
        // SAFETY: `region` is mmap'd and remains valid
        // for the Region's lifetime; we cast inside its
        // bounds.
        Ring {
            writer_pos: unsafe { base.add(wpos_off).cast::<AtomicU64>() },
            reader_pos: unsafe { base.add(rpos_off).cast::<AtomicU64>() },
            slots: unsafe { base.add(data_off) },
        }
    }

    /// Initialise both positions to zero. Called once
    /// per region by the creator (server) before the
    /// client attaches.
    pub fn reset(&self) {
        // SAFETY: writer_pos and reader_pos point to
        // valid AtomicU64s inside shmem; Relaxed write
        // is fine — no peer is observing yet.
        unsafe {
            (*self.writer_pos).store(0, Ordering::Relaxed);
            (*self.reader_pos).store(0, Ordering::Relaxed);
        }
    }

    /// Push one v2 frame with a **producer-local cached
    /// `reader_pos`** (Rigtorp SPSC pattern). The producer
    /// owns `cached_rpos` and reads it without atomics on
    /// the hot path; only when it shows the ring is Full
    /// does this function `Acquire`-load the true
    /// `reader_pos` and refresh the cache. This collapses
    /// the cross-core cache-line transfer of `reader_pos`
    /// from "every push" to "every ring-full window",
    /// which on i7-10700K class hardware is the dominant
    /// SPSC throughput multiplier — see Rigtorp's
    /// ringbuffer writeup (~20× in his bench) and Aeron's
    /// `headCachePositionIndex`.
    ///
    /// Caller's `cached_rpos` must be initialised to `0`
    /// at construction (matching the post-reset state).
    /// The function may update it on Full-path Acquire.
    ///
    /// # Errors
    ///
    /// `RingError::Full` if the ring is at capacity even
    /// after a fresh Acquire-load (Pop must drain), or
    /// `RingError::FrameTooLarge`.
    pub fn push_cached(
        &self,
        header: &Header,
        payload: &[u8],
        cached_rpos: &mut u64,
    ) -> Result<(), RingError> {
        if payload.len() + HEADER_LEN > SLOT_STRIDE {
            return Err(RingError::FrameTooLarge);
        }
        // SAFETY: writer_pos/reader_pos are valid
        // AtomicU64s inside shmem.
        let wpos = unsafe { (*self.writer_pos).load(Ordering::Relaxed) };
        // Hot path: check Full against the producer-local
        // cache. No atomic load of reader_pos.
        if wpos.wrapping_sub(*cached_rpos) >= SLOTS_PER_RING as u64 {
            // Cache says full; refresh from the consumer-
            // owned cache line (one cross-core transfer).
            *cached_rpos = unsafe { (*self.reader_pos).load(Ordering::Acquire) };
            if wpos.wrapping_sub(*cached_rpos) >= SLOTS_PER_RING as u64 {
                return Err(RingError::Full);
            }
        }
        let slot_idx = (wpos as usize) & (SLOTS_PER_RING - 1);
        let slot_ptr = unsafe { self.slots.add(slot_idx * SLOT_STRIDE) };
        let mut hdr_buf = [0u8; HEADER_LEN];
        encode_header(header, &mut hdr_buf);
        unsafe {
            core::ptr::copy_nonoverlapping(hdr_buf.as_ptr(), slot_ptr, HEADER_LEN);
            if !payload.is_empty() {
                core::ptr::copy_nonoverlapping(
                    payload.as_ptr(),
                    slot_ptr.add(HEADER_LEN),
                    payload.len(),
                );
            }
        }
        unsafe {
            (*self.writer_pos).store(wpos.wrapping_add(1), Ordering::Release);
        }
        Ok(())
    }

    /// Push one v2 frame. Single-producer; caller must
    /// hold the producer role. Uncached version — every
    /// push pays a cross-core `Acquire`-load of
    /// `reader_pos`. Retained for unary callers where the
    /// throughput hot loop doesn't exist; throughput
    /// callers should use [`push_cached`].
    ///
    /// # Errors
    ///
    /// `RingError::Full` if the ring is at capacity
    /// (Pop must drain a slot first), or
    /// `RingError::FrameTooLarge` if `payload.len() >
    /// SLOT_STRIDE - HEADER_LEN`.
    pub fn push(&self, header: &Header, payload: &[u8]) -> Result<(), RingError> {
        if payload.len() + HEADER_LEN > SLOT_STRIDE {
            return Err(RingError::FrameTooLarge);
        }
        // SAFETY: writer_pos/reader_pos are valid
        // AtomicU64s inside shmem. Acquire on reader_pos
        // pairs with the consumer's Release.
        let rpos = unsafe { (*self.reader_pos).load(Ordering::Acquire) };
        let wpos = unsafe { (*self.writer_pos).load(Ordering::Relaxed) };
        if wpos.wrapping_sub(rpos) >= SLOTS_PER_RING as u64 {
            return Err(RingError::Full);
        }
        let slot_idx = (wpos as usize) & (SLOTS_PER_RING - 1);
        let slot_ptr = unsafe { self.slots.add(slot_idx * SLOT_STRIDE) };
        // Encode header in-place.
        let mut hdr_buf = [0u8; HEADER_LEN];
        encode_header(header, &mut hdr_buf);
        unsafe {
            core::ptr::copy_nonoverlapping(hdr_buf.as_ptr(), slot_ptr, HEADER_LEN);
            if !payload.is_empty() {
                core::ptr::copy_nonoverlapping(
                    payload.as_ptr(),
                    slot_ptr.add(HEADER_LEN),
                    payload.len(),
                );
            }
        }
        // Release the publish: any reader observing the
        // new writer_pos will see the slot writes above.
        unsafe {
            (*self.writer_pos).store(wpos.wrapping_add(1), Ordering::Release);
        }
        Ok(())
    }

    /// Pop one v2 frame into `out` with a **consumer-local
    /// cached `writer_pos`** (Rigtorp SPSC pattern). The
    /// consumer owns `cached_wpos` and reads it without
    /// atomics on the hot path; only when it shows Empty
    /// does this function `Acquire`-load the true
    /// `writer_pos`. Pairs with [`push_cached`] on the
    /// producer side to eliminate the per-frame cross-core
    /// transfer of the position cache lines.
    ///
    /// Caller's `cached_wpos` must be initialised to `0`
    /// at construction.
    pub fn pop_cached(
        &self,
        out: &mut Vec<u8>,
        cached_wpos: &mut u64,
    ) -> Result<Header, RingError> {
        // SAFETY: same as `push`.
        let rpos = unsafe { (*self.reader_pos).load(Ordering::Relaxed) };
        if rpos == *cached_wpos {
            // Cache says empty; refresh from the producer-
            // owned cache line.
            *cached_wpos = unsafe { (*self.writer_pos).load(Ordering::Acquire) };
            if rpos == *cached_wpos {
                return Err(RingError::Empty);
            }
        }
        let slot_idx = (rpos as usize) & (SLOTS_PER_RING - 1);
        let slot_ptr = unsafe { self.slots.add(slot_idx * SLOT_STRIDE) };
        let mut hdr_buf = [0u8; HEADER_LEN];
        unsafe {
            core::ptr::copy_nonoverlapping(slot_ptr, hdr_buf.as_mut_ptr(), HEADER_LEN);
        }
        let header = decode_header(&hdr_buf).map_err(|_| RingError::BadHeader)?;
        let plen = header.payload_len as usize;
        if plen + HEADER_LEN > SLOT_STRIDE {
            return Err(RingError::FrameTooLarge);
        }
        out.clear();
        out.resize(plen, 0);
        if plen > 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(slot_ptr.add(HEADER_LEN), out.as_mut_ptr(), plen);
            }
        }
        unsafe {
            (*self.reader_pos).store(rpos.wrapping_add(1), Ordering::Release);
        }
        Ok(header)
    }

    /// **Zero-copy pop** — return the decoded header and a
    /// borrowed slice pointing directly at the slot's
    /// payload bytes in shared memory. The caller MUST
    /// finish reading the slice before calling
    /// [`advance_reader`]; the next push by the producer
    /// will overwrite that slot.
    ///
    /// Used by [`crate::ShmemConnection::call_server_streaming_view`]
    /// to skip the per-frame `memcpy` from shmem slot into
    /// a caller-owned `Vec<u8>` — the dominant
    /// throughput-path cost on high-rate server-streaming.
    /// Returns `RingError::Empty` if no frame ready.
    pub fn pop_view_cached(&self, cached_wpos: &mut u64) -> Result<(Header, &[u8]), RingError> {
        // SAFETY: writer_pos/reader_pos are valid
        // AtomicU64s inside shmem.
        let rpos = unsafe { (*self.reader_pos).load(Ordering::Relaxed) };
        if rpos == *cached_wpos {
            *cached_wpos = unsafe { (*self.writer_pos).load(Ordering::Acquire) };
            if rpos == *cached_wpos {
                return Err(RingError::Empty);
            }
        }
        let slot_idx = (rpos as usize) & (SLOTS_PER_RING - 1);
        let slot_ptr = unsafe { self.slots.add(slot_idx * SLOT_STRIDE) };
        let mut hdr_buf = [0u8; HEADER_LEN];
        unsafe {
            core::ptr::copy_nonoverlapping(slot_ptr, hdr_buf.as_mut_ptr(), HEADER_LEN);
        }
        let header = decode_header(&hdr_buf).map_err(|_| RingError::BadHeader)?;
        let plen = header.payload_len as usize;
        if plen + HEADER_LEN > SLOT_STRIDE {
            return Err(RingError::FrameTooLarge);
        }
        // SAFETY: slot_ptr.add(HEADER_LEN) points into the
        // mmap'd region for `plen` bytes (validated above).
        // The borrow's lifetime is tied to &self, which is
        // the Ring handle — caller cannot advance_reader
        // and re-pop without dropping the borrow.
        let payload = unsafe { core::slice::from_raw_parts(slot_ptr.add(HEADER_LEN), plen) };
        Ok((header, payload))
    }

    /// Release the slot most recently returned by
    /// [`pop_view_cached`]. After this call, the borrowed
    /// payload slice is logically invalidated — the
    /// producer may overwrite the slot. Single-consumer.
    pub fn advance_reader(&self) {
        // SAFETY: same as `pop`. Single-consumer guarantee.
        let rpos = unsafe { (*self.reader_pos).load(Ordering::Relaxed) };
        unsafe {
            (*self.reader_pos).store(rpos.wrapping_add(1), Ordering::Release);
        }
    }

    /// Pop one v2 frame into `out`. `out` is `clear()`-
    /// then-resized to hold the payload. Single-consumer.
    pub fn pop(&self, out: &mut Vec<u8>) -> Result<Header, RingError> {
        // SAFETY: same as `push`.
        let wpos = unsafe { (*self.writer_pos).load(Ordering::Acquire) };
        let rpos = unsafe { (*self.reader_pos).load(Ordering::Relaxed) };
        if wpos == rpos {
            return Err(RingError::Empty);
        }
        let slot_idx = (rpos as usize) & (SLOTS_PER_RING - 1);
        let slot_ptr = unsafe { self.slots.add(slot_idx * SLOT_STRIDE) };
        let mut hdr_buf = [0u8; HEADER_LEN];
        unsafe {
            core::ptr::copy_nonoverlapping(slot_ptr, hdr_buf.as_mut_ptr(), HEADER_LEN);
        }
        let header = decode_header(&hdr_buf).map_err(|_| RingError::BadHeader)?;
        let plen = header.payload_len as usize;
        if plen + HEADER_LEN > SLOT_STRIDE {
            return Err(RingError::FrameTooLarge);
        }
        out.clear();
        out.resize(plen, 0);
        if plen > 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(slot_ptr.add(HEADER_LEN), out.as_mut_ptr(), plen);
            }
        }
        // Release: publishes the slot as free.
        unsafe {
            (*self.reader_pos).store(rpos.wrapping_add(1), Ordering::Release);
        }
        Ok(header)
    }

    /// True if the consumer has no pending slot.
    pub fn is_empty(&self) -> bool {
        let wpos = unsafe { (*self.writer_pos).load(Ordering::Acquire) };
        let rpos = unsafe { (*self.reader_pos).load(Ordering::Acquire) };
        wpos == rpos
    }
}

// ============================================================
// Helpers for constructing a `Ring` pair from a `Region`
// ============================================================

/// Client-side rings: producer for c2s, consumer for s2c.
pub struct ClientRings {
    /// Outbound (client→server) ring — client writes,
    /// server reads.
    pub send: Ring,
    /// Inbound (server→client) ring — client reads,
    /// server writes.
    pub recv: Ring,
}

/// Server-side rings: consumer for c2s, producer for s2c.
pub struct ServerRings {
    /// Inbound (client→server) ring — server reads,
    /// client writes.
    pub recv: Ring,
    /// Outbound (server→client) ring — server writes,
    /// client reads.
    pub send: Ring,
}

/// Wire-up rings from the negotiated `Region`. The
/// region MUST be ≥ `MIN_REGION_BYTES` bytes; T3.1's
/// rendezvous negotiates `slot_count * slot_stride`
/// directly, so the data-plane caller is responsible
/// for asking for enough.
///
/// # Safety
///
/// `region` must be shared with the peer and outlive
/// every Ring derived from it.
pub unsafe fn client_rings(region: &Region) -> ClientRings {
    assert!(
        region.len() >= MIN_REGION_BYTES,
        "shmem region {} bytes < required {}",
        region.len(),
        MIN_REGION_BYTES,
    );
    let send = unsafe { Ring::new_in_region(region, 0, CACHE_LINE, C2S_DATA_OFFSET) };
    let recv =
        unsafe { Ring::new_in_region(region, 2 * CACHE_LINE, 3 * CACHE_LINE, S2C_DATA_OFFSET) };
    ClientRings { send, recv }
}

/// Wire-up server-side rings from the negotiated `Region`.
///
/// # Safety
///
/// Same as [`client_rings`].
pub unsafe fn server_rings(region: &Region) -> ServerRings {
    assert!(region.len() >= MIN_REGION_BYTES);
    let recv = unsafe { Ring::new_in_region(region, 0, CACHE_LINE, C2S_DATA_OFFSET) };
    let send =
        unsafe { Ring::new_in_region(region, 2 * CACHE_LINE, 3 * CACHE_LINE, S2C_DATA_OFFSET) };
    ServerRings { recv, send }
}
