//! Connection state machine — drives a complete QUIC handshake
//! to 1-RTT and multiplexes streams over the encryption levels.
//!
//! This is the v0.6 deliverable. Composes v0.2-v0.5 primitives
//! (varint, packet header codec, frame codec, packet protection,
//! Initial packet helper) with the L3 v1.2
//! `grpcx_crypto::quic_handshake::QuicEndpoint` to drive a full
//! handshake to 1-RTT and exchange application data over streams.
//!
//! Scope cuts (deferred to v1.x):
//! - No coalesced packets (each pump emits one datagram per
//!   encryption level that has data; the loopback bus is
//!   reliable + ordered so it works in tests).
//! - No stream-level flow control.
//! - No connection migration.
//! - No 0-RTT.
//!
//! Reliability (1.5.0 phase 1, 1.6.0 phase 2, 1.7.0
//! phase 3): ACK generation/processing, packet-threshold +
//! PTO loss recovery, RFC 9002 RTT estimation, handshake-
//! plane CRYPTO retransmit, persistent-congestion
//! collapse, and congestion control (CUBIC default,
//! NewReno fallback). Time
//! is caller-injected (sans-IO) via [`Connection::pump_at`];
//! untimed pumps never advance the clock, so no timer fires
//! and pre-1.6 behavior is preserved exactly.

use crate::Error;
use crate::frame::{Frame, decode_frame, encode_frame};
use crate::initial::{Side, build_initial_packet_with_initial_dcid, parse_initial_packet};
use crate::packet::{LongHeader, LongHeaderType, PacketHeader, decode_header, encode_header};
use crate::varint::read_varint;
use crate::{ConnectionId, StreamId};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use grpcx_crypto::TlsConfig;
use grpcx_crypto::quic_handshake::{QuicEndpoint, QuicKeyChange, QuicKeys, QuicVersion};

const TAG_LEN: usize = 16;
const PN_LEN: u8 = 4;

/// One end of a QUIC connection — client or server. Drives the
/// handshake to 1-RTT and multiplexes application streams.
pub struct Connection {
    /// At least one stream is soft-closed awaiting drain
    /// (guards the per-pump reap sweep).
    reap_pending: bool,
    /// Reused mutable copy of the incoming datagram (HP
    /// removal + AEAD decrypt work in place, but `pump`
    /// borrows the datagram immutably) — one buffer per
    /// connection instead of one malloc per packet.
    recv_scratch: Vec<u8>,

    // ---- Reliability (phase 1 + phase 2) ----
    // Phase 1: ACK generation + retransmit-on-packet-
    // threshold (RFC 9002 §6.1, kPacketThreshold = 3).
    // Phase 2: the sans-IO time surface (`pump_at`), RTT
    // estimation, PTO/tail-loss probes, handshake-plane
    // CRYPTO retransmit, congestion control. Phase 3:
    // persistent congestion + CUBIC (default).
    /// 1-RTT packet numbers received so far, as merged
    /// inclusive ranges sorted ascending (in-order
    /// delivery keeps this at one range; loss splits it).
    recv_pn_ranges: Vec<(u64, u64)>,
    /// An ACK is owed: an ack-eliciting 1-RTT packet
    /// arrived since the last ACK we sent.
    ack_pending: bool,
    /// Sent ack-eliciting 1-RTT packets awaiting
    /// acknowledgment, ascending by packet number.
    sent_packets: Vec<SentRecord>,
    /// Stream spans declared lost (packet threshold) —
    /// resent ahead of new data by the 1-RTT builder.
    retransmit_queue: Vec<RetransmitSpan>,
    /// Control frames owed to the peer (RESET_STREAM /
    /// STOP_SENDING — eager cancel, 1.9). Loss-recovered
    /// since 1.9.1 (RFC 9000 §13.3): each packet's ctrl
    /// frames ride its SentRecord and re-queue here on
    /// threshold loss or PTO harvest.
    pending_ctrl: Vec<Frame>,
    /// Streams whose peer-facing life ended since the
    /// last `take_reset_streams` drain (either side reset)
    /// — the upper layer drops its per-stream state.
    reset_events: Vec<u64>,
    /// Recently fully-reset sids (bounded ring): late
    /// in-flight data for them is dropped instead of
    /// re-registering a fresh stream entry.
    recently_reset: Vec<u64>,
    /// 1-RTT plaintext budget per datagram. Datagram size ≈
    /// budget + ~30 B header/tag overhead; the caller picks
    /// it from path MTU (default 1350 — the QUIC-ecosystem
    /// conservative value; loopback links can go much
    /// higher, e.g. ~9000 under Darwin's
    /// `net.inet.udp.maxdgram`). Fewer, larger datagrams
    /// amortize per-packet sealing, ACKs, and scheduling.
    one_rtt_plaintext_budget: usize,

    // ---- Phase 2: time, RTT, PTO, congestion control ----
    /// Monotonic clock, µs — the max over every `pump_at`
    /// stamp. Stays 0 on never-timed connections, which
    /// disarms every timer (pre-1.6 behavior).
    now_us: u64,
    /// RFC 9002 §5 estimator state. Valid only once
    /// `has_rtt_sample` (ack_delay is always 0 on our wire
    /// — ACKs are never delayed).
    srtt_us: u64,
    rttvar_us: u64,
    min_rtt_us: u64,
    has_rtt_sample: bool,
    /// Time of the last ack-eliciting 1-RTT send — the PTO
    /// arm point (0 = disarmed; stamps floor at 1 µs).
    last_ae_sent_us: u64,
    /// Consecutive unanswered PTO probes (doubles the
    /// timeout); reset by any forward-progress ACK.
    pto_backoff: u32,
    /// Send a PING probe in the next 1-RTT packet (PTO
    /// fired on a packet with nothing retransmittable —
    /// any ack-eliciting packet makes the peer re-ACK its
    /// full received ranges, recovering a lost ACK).
    ping_pending: bool,
    /// Congestion window and slow-start threshold in 1-RTT
    /// plaintext bytes. Growth/reduction law selected by
    /// `cc_algo` (CUBIC default, NewReno fallback — the v4
    /// bootstrap charter).
    cwnd: usize,
    ssthresh: usize,
    /// Unacknowledged ack-eliciting 1-RTT plaintext bytes.
    bytes_in_flight: usize,
    /// Loss of packets numbered below this is part of an
    /// already-handled recovery episode — cwnd reduces once
    /// per window, not once per lost packet.
    recovery_until_pn: u64,

    // ---- Phase 3: CUBIC + persistent congestion ----
    cc_algo: CongestionAlgorithm,
    /// CUBIC (RFC 9438): window size before the last
    /// reduction — the plateau the cubic curve regrows
    /// toward (bytes).
    cubic_w_max: usize,
    /// Reno-style estimator for the TCP-friendly region
    /// (bytes); while the cubic curve sits below it, cwnd
    /// follows this instead.
    cubic_w_est: usize,
    /// Congestion-avoidance epoch anchor (µs; 0 = not yet
    /// anchored — set on the first CA ack after a
    /// congestion event).
    cubic_epoch_start_us: u64,
    /// Time from the epoch anchor to regaining `w_max`
    /// (ms), `K = ∛(w_max·(1−β)/C)`.
    cubic_k_ms: u64,
    /// Persistent congestion (RFC 9002 §7.6): send-time
    /// span of every packet counted lost (threshold OR
    /// PTO-harvested) since the last forward-progress ACK
    /// (0 = no open span). When the span exceeds
    /// 3 × PTO, cwnd collapses to the minimum window.
    pc_earliest_sent_us: u64,
    pc_latest_sent_us: u64,

    // ---- Link liveness (1.8.0) ----
    /// Quiet-link probe interval (µs) — see
    /// [`Connection::set_keepalive_us`].
    keepalive_us: u64,
    /// No-inbound death bound (µs) — see
    /// [`Connection::set_idle_timeout_us`].
    idle_timeout_us: u64,
    /// Clock reading at the last inbound datagram (any
    /// datagram — a packet on the wire proves the link;
    /// 0 = none yet).
    last_inbound_us: u64,
    /// Last keepalive PING queue time (rate limit).
    last_keepalive_us: u64,
    /// Clock reading at the first handshake-plane send
    /// (bounds a handshake against a dead peer; 0 = not
    /// started).
    hs_started_us: u64,
    /// Clock at the previous liveness check — a gap wider
    /// than the keepalive interval means the driver wasn't
    /// pumping (a blocking caller between calls), so the
    /// quiet window restarts instead of firing falsely.
    last_liveness_check_us: u64,

    // ---- Phase 2: handshake-plane retransmit ----
    // The handshake planes carry no per-level ACK
    // machinery; instead each level's outbound CRYPTO
    // bytes are retained and blindly re-sent in full
    // (offset 0) on a PTO-class timer until the handshake
    // is CONFIRMED — the receive side trims duplicates, so
    // this converges. Confirmation: server = TLS complete;
    // client = HANDSHAKE_DONE received (RFC 9001 §4.1.2).
    handshake_confirmed: bool,
    /// Server: a HANDSHAKE_DONE frame is owed on the next
    /// 1-RTT packet (queued once on establishment;
    /// re-queued if its packet is declared lost).
    handshake_done_pending: bool,
    handshake_done_queued_once: bool,
    initial_crypto_log: Vec<u8>,
    handshake_crypto_log: Vec<u8>,
    /// Time of the last handshake-plane send (0 = never;
    /// stamps floor at 1 µs).
    last_hs_sent_us: u64,
    hs_pto_backoff: u32,
    /// Set by the handshake timer; consumed by the per-
    /// level builders (re-emit the full log from offset 0).
    resend_initial: bool,
    resend_handshake: bool,
    /// Out-of-order CRYPTO segments parked per level
    /// (mirror of the stream-data path — retransmits make
    /// duplicates and reordering inevitable).
    initial_crypto_oob: BTreeMap<u64, Vec<u8>>,
    handshake_crypto_oob: BTreeMap<u64, Vec<u8>>,

    endpoint: QuicEndpoint,
    side: Side,

    /// Initial-level keys derive from the client's destination
    /// Connection ID; both sides agree on this value at handshake
    /// start (the client invents one; the server learns it from
    /// the first inbound Initial packet).
    initial_dcid: Vec<u8>,

    /// DCID we put on outgoing packets (the peer's SCID we
    /// learnt during the handshake).
    peer_cid: ConnectionId,
    /// Our own CID (the peer puts this as DCID on packets they
    /// send to us). Picked at construction.
    our_cid: ConnectionId,

    /// Per-level keys from rustls's KeyChange events.
    handshake_keys: Option<QuicKeys>,
    one_rtt_keys: Option<QuicKeys>,

    /// Per-level next outbound packet number.
    initial_pn: u32,
    handshake_pn: u32,
    one_rtt_pn: u32,

    /// Per-level CRYPTO stream offsets (RFC 9001 §4: each level
    /// has its own CRYPTO byte stream starting at offset 0).
    initial_crypto_sent: u64,
    handshake_crypto_sent: u64,
    initial_crypto_recv_offset: u64,
    handshake_crypto_recv_offset: u64,

    /// Per-stream state.
    streams: BTreeMap<u64, StreamState>,

    /// Bumped on each local outbound bidirectional stream.
    next_local_bi_stream_id: u64,
    /// Bumped on each local outbound unidirectional stream.
    /// RFC 9000 §2.1 stream-id bit 1 = 1 marks uni.
    next_local_uni_stream_id: u64,

    /// FIFO of peer-initiated stream ids we've observed but the
    /// caller hasn't yet drained via `accept_uni` / `accept_bi`.
    /// L5 (`grpcx-h3`) consumes these to discover control / push /
    /// QPACK uni streams and incoming request bi streams.
    peer_initiated_uni: Vec<u64>,
    peer_initiated_bi: Vec<u64>,

    /// 1.2.0: pending outbound unreliable datagrams (RFC 9221).
    /// Drained by `build_one_rtt_datagram`; each item turns into
    /// one `Frame::Datagram` on the wire.
    outbound_datagrams: Vec<Vec<u8>>,
    /// 1.2.0: inbound datagram payloads, populated by
    /// `handle_frame` on the receive path; drained by
    /// `recv_datagram`.
    inbound_datagrams: Vec<Vec<u8>>,

    /// Handshake completion flag (1-RTT keys installed + TLS done).
    established: bool,
}

#[derive(Default)]
struct StreamState {
    /// Send buffer: bytes from stream offset `base_offset`
    /// onward. Three cursors partition it —
    /// `[..acked_cursor]` acknowledged (releasable),
    /// `[acked_cursor..send_cursor]` on the wire awaiting
    /// ACK (the retransmit source), `[send_cursor..]` not
    /// yet sent. The buffer is freed wholesale once
    /// everything in it is both sent and acked (no
    /// front-drain memmove on the hot path — see the
    /// `send_cursor` note).
    pending_send: Vec<u8>,
    /// Stream offset of `pending_send[0]`.
    base_offset: u64,
    /// Contiguously-acknowledged prefix (index).
    acked_cursor: usize,
    /// Acknowledged spans beyond the contiguous prefix
    /// (stream-offset space, merged, ascending) — filled
    /// by out-of-order ACKs, drained into `acked_cursor`
    /// as the holes close.
    acked_ranges: Vec<(u64, u64)>,
    /// Read cursor into `pending_send` — the unsent bytes
    /// are `pending_send[send_cursor..]`. Draining from the
    /// FRONT of a Vec memmoves the whole tail every time;
    /// on a 64 KiB send that was ~60 datagrams x ~60 KiB of
    /// shifting (O(n^2), ~2 MB of pure memmove per call —
    /// the single hottest line in the 64 KiB profile).
    send_cursor: usize,
    sent_offset: u64,
    sent_fin: bool,
    fin_sent_on_wire: bool,
    /// The sent FIN has been acknowledged.
    fin_acked: bool,
    recv_buf: Vec<u8>,
    /// Stream offset one past the last byte delivered into
    /// `recv_buf` (the contiguous receive prefix).
    recv_offset: u64,
    /// Out-of-order received segments (start offset →
    /// bytes), waiting for the prefix to catch up.
    recv_oob: BTreeMap<u64, Vec<u8>>,
    /// Final stream size (offset one past the last byte),
    /// learned from a FIN-bearing frame.
    fin_offset: Option<u64>,
    recv_fin: bool,
    /// Soft-close marker (v1.3): the owner is done with
    /// the stream; the entry is reaped once the send queue
    /// fully drains onto the wire AND every sent byte
    /// (and the FIN) is acknowledged — reaping earlier
    /// would discard the retransmit source.
    closing: bool,
    /// We reset our send side (RESET_STREAM queued/sent —
    /// eager cancel, 1.9): no further STREAM frames go
    /// out, retransmit spans are purged.
    reset_sent: bool,
    /// The peer reset their send side (RESET_STREAM
    /// received): recv state dropped, late data ignored.
    peer_reset: bool,
}

impl StreamState {
    fn send_is_empty(&self) -> bool {
        self.pending_send.len() == self.send_cursor
    }
    fn send_pending_len(&self) -> usize {
        self.pending_send.len() - self.send_cursor
    }
    /// Everything ever queued is sent and acknowledged
    /// (including the FIN if one was sent).
    fn fully_acked(&self) -> bool {
        self.send_is_empty()
            && self.acked_cursor == self.pending_send.len()
            && (!self.sent_fin || self.fin_acked)
    }
    /// Record `[start, end)` (stream offsets) as
    /// acknowledged; advance the contiguous prefix and
    /// release the buffer when everything is through.
    fn ack_span(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        merge_span(&mut self.acked_ranges, start, end);
        // Drain ranges into the contiguous prefix.
        let mut prefix = self.base_offset + self.acked_cursor as u64;
        while let Some(&(s, e)) = self.acked_ranges.first() {
            if s > prefix {
                break;
            }
            prefix = prefix.max(e);
            self.acked_ranges.remove(0);
        }
        self.acked_cursor = (prefix - self.base_offset) as usize;
        // Fully sent + fully acked → release the buffer.
        if self.send_is_empty() && self.acked_cursor == self.pending_send.len() {
            self.base_offset += self.pending_send.len() as u64;
            self.pending_send.clear();
            self.send_cursor = 0;
            self.acked_cursor = 0;
        }
    }
}

/// Per-level CRYPTO receive reassembly: append the newly
/// contiguous bytes (after trimming what the prefix already
/// covers) to `out`, advancing `recv_offset`; park
/// out-of-order segments in `oob` until the prefix reaches
/// them. The TLS machine then consumes `out` in order.
fn crypto_accept(
    recv_offset: &mut u64,
    oob: &mut BTreeMap<u64, Vec<u8>>,
    offset: u64,
    data: &[u8],
    out: &mut Vec<u8>,
) {
    let end = offset + data.len() as u64;
    if end <= *recv_offset {
        // Entirely a duplicate.
        return;
    }
    if offset > *recv_offset {
        // A hole before this segment — park it.
        oob.insert(offset, data.to_vec());
        return;
    }
    let skip = (*recv_offset - offset) as usize;
    out.extend_from_slice(&data[skip..]);
    *recv_offset = end;
    // Pull any parked segments the prefix now reaches.
    while let Some((&s, _)) = oob.first_key_value() {
        if s > *recv_offset {
            break;
        }
        let (s, seg) = oob.pop_first().expect("checked first");
        let seg_end = s + seg.len() as u64;
        if seg_end > *recv_offset {
            let skip = (*recv_offset - s) as usize;
            out.extend_from_slice(&seg[skip..]);
            *recv_offset = seg_end;
        }
    }
}

/// Merge inclusive-exclusive span `[start, end)` into a
/// sorted, disjoint range list.
fn merge_span(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64) {
    let idx = ranges.partition_point(|&(_, e)| e < start);
    let mut start = start;
    let mut end = end;
    while idx < ranges.len() && ranges[idx].0 <= end {
        start = start.min(ranges[idx].0);
        end = end.max(ranges[idx].1);
        ranges.remove(idx);
    }
    ranges.insert(idx, (start, end));
}

/// RFC 9002 §6.1.1 kPacketThreshold — a packet this many
/// numbers below the largest-acknowledged without its own
/// ACK is declared lost.
const PACKET_THRESHOLD: u64 = 3;

/// RFC 9002 §6.1.2 kGranularity — the PTO floor. Keeps the
/// probe rate sane on sub-millisecond-RTT links (loopback).
const GRANULARITY_US: u64 = 1_000;

/// Pre-first-RTT-sample assumed RTT. RFC 9002 says 333 ms;
/// this stack's drive loops report a dead link after
/// ~32 ms, so the first probe must land well inside that —
/// 5 ms gives a 10 ms first PTO and one doubling to spare.
const INITIAL_RTT_US: u64 = 5_000;

/// PTO backoff exponent cap (× 64) — keeps the shift sane.
const MAX_PTO_BACKOFF: u32 = 6;

/// Initial congestion window / minimum congestion window,
/// in units of the per-datagram plaintext budget (RFC 9002
/// §7.2's 10 × max_datagram_size and 2 × minimum).
const INITIAL_CWND_PACKETS: usize = 10;
const MIN_CWND_PACKETS: usize = 2;

/// RFC 9002 §7.6.1 kPersistentCongestionThreshold — lost
/// packets whose send times span more than this many PTOs
/// without an intervening forward-progress ACK collapse
/// the window to the minimum.
const PERSISTENT_CONGESTION_PTOS: u64 = 3;

/// Link-liveness defaults (1.8.0, lab-scale; setters on
/// [`Connection`]): a link quiet past the keepalive
/// interval gets a PING (any response is inbound traffic,
/// which is the liveness proof); no inbound traffic past
/// the idle timeout means the link is dead.
const DEFAULT_KEEPALIVE_US: u64 = 2_500_000;
const DEFAULT_IDLE_TIMEOUT_US: u64 = 10_000_000;

/// Congestion-control algorithm (RFC 9002 §7 base
/// machinery; the growth/reduction law is selectable).
/// CUBIC is the default per the v4 bootstrap charter
/// ("CUBIC default, NewReno fallback").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CongestionAlgorithm {
    /// RFC 9438: β = 0.7, cubic regrowth around the last
    /// congestion point, fast convergence, TCP-friendly
    /// region (which governs at loopback RTTs — K sits
    /// seconds out, so the Reno estimator leads).
    #[default]
    Cubic,
    /// RFC 9002 §7 sample algorithm: halve on loss, linear
    /// recovery. The phase-2 behavior, kept as fallback.
    NewReno,
}

/// Integer cube root: the largest `r` with `r³ ≤ n`
/// (binary search, ~21 iterations, u128 cubes — no floats;
/// `no_std`).
fn icbrt(n: u64) -> u64 {
    // ∛(u64::MAX) < 2 642 246.
    let (mut lo, mut hi) = (0u64, 2_642_246u64);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if (mid as u128).pow(3) <= n as u128 {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// CUBIC window at `d_ms` milliseconds from the curve's
/// inflection point K (signed: negative = concave approach
/// below `w_max`, positive = convex probe above it), in
/// bytes. `W(t) = w_max + C·(t−K)³` with C = 0.4 seg/s³ =
/// 4·(d_ms)³/10¹⁰ segments.
fn cubic_window(w_max_bytes: usize, d_ms: i64, budget: usize) -> usize {
    let cube = (d_ms as i128).pow(3);
    let delta = (4 * cube * budget as i128) / 10_000_000_000;
    let w = w_max_bytes as i128 + delta;
    if w < 0 { 0 } else { w as usize }
}

/// CUBIC K in ms: time to regain `w_max` after a β = 0.7
/// reduction — `K = ∛(w_max_seg·(1−β)/C)` seconds =
/// `∛(0.75·w_max_seg)·1000` ms = `∛(750 000 000·w_max_seg)`.
fn cubic_k_ms(w_max_bytes: usize, budget: usize) -> u64 {
    let w_max_seg = (w_max_bytes / budget.max(1)) as u64;
    icbrt(750_000_000 * w_max_seg)
}

/// Build an ACK frame from received-PN ranges (ascending,
/// half-open `[lo, hi)`), encoding RFC 9000 §19.3.1's
/// descending (gap, range) pairs.
fn ack_frame_from_ranges(ranges: &[(u64, u64)]) -> Frame {
    let last = ranges.last().expect("caller checks non-empty");
    let largest_acked = last.1 - 1;
    let first_ack_range = last.1 - 1 - last.0;
    let mut pairs: Vec<(u64, u64)> = Vec::with_capacity(ranges.len().saturating_sub(1));
    let mut smallest = last.0;
    for &(lo, hi) in ranges.iter().rev().skip(1) {
        // gap = number of unacked PNs between this range's
        // top (hi-1) and the previous range's bottom,
        // minus 2 (RFC's encoding).
        let gap = smallest - hi - 1;
        let range = (hi - 1) - lo;
        pairs.push((gap, range));
        smallest = lo;
    }
    Frame::Ack {
        largest_acked,
        ack_delay: 0,
        first_ack_range,
        ranges: pairs,
        ecn: None,
    }
}

/// One sent ack-eliciting 1-RTT packet awaiting ACK.
struct SentRecord {
    pn: u64,
    spans: Vec<RetransmitSpan>,
    /// Clock reading at send (RTT sample source).
    time_sent: u64,
    /// Plaintext bytes (the `bytes_in_flight` unit).
    size: usize,
    /// The packet carried HANDSHAKE_DONE — re-queue the
    /// frame if the packet is declared lost.
    handshake_done: bool,
    /// Cancel control frames the packet carried
    /// (RESET_STREAM / STOP_SENDING) — re-queued on loss
    /// (RFC 9000 §13.3; departure recorded at 1.9.0,
    /// closed at 1.9.1).
    ctrl: Vec<Frame>,
}

/// A retransmittable STREAM span: `len` bytes of stream
/// `sid` starting at stream offset `offset` (+ FIN flag).
#[derive(Clone)]
struct RetransmitSpan {
    sid: u64,
    offset: u64,
    len: usize,
    fin: bool,
}

impl Connection {
    /// Open a client-side connection.
    pub fn new_client(
        tls: &TlsConfig,
        server_name: &str,
        transport_params: Vec<u8>,
    ) -> Result<Self, Error> {
        // Pick a random-ish 8-byte CID. RFC 9000 §17.2 allows up
        // to 20 bytes. We use a deterministic-from-server-name
        // 8-byte ID for v0.6 tests so the loopback is repeatable.
        let our_cid = cid_from_bytes(&hash_bytes(server_name.as_bytes(), 0xc1_e7_71))?;
        let initial_dcid = hash_bytes(server_name.as_bytes(), 0x5e_77_e7).to_vec();
        let peer_cid = cid_from_bytes(&initial_dcid)?;
        let endpoint =
            QuicEndpoint::new_client(tls, QuicVersion::V1, server_name, transport_params)
                .map_err(|_| Error::BadCrypto)?;
        Ok(Self::new(
            endpoint,
            Side::Client,
            initial_dcid,
            our_cid,
            peer_cid,
        ))
    }

    /// Open a server-side connection.
    pub fn new_server(tls: &TlsConfig, transport_params: Vec<u8>) -> Result<Self, Error> {
        // Server's CIDs land once the client's first Initial
        // arrives. Bootstrap with zero-length placeholders.
        let endpoint = QuicEndpoint::new_server(tls, QuicVersion::V1, transport_params)
            .map_err(|_| Error::BadCrypto)?;
        Ok(Self::new(
            endpoint,
            Side::Server,
            Vec::new(),
            empty_cid(),
            empty_cid(),
        ))
    }

    fn new(
        endpoint: QuicEndpoint,
        side: Side,
        initial_dcid: Vec<u8>,
        our_cid: ConnectionId,
        peer_cid: ConnectionId,
    ) -> Self {
        Self {
            endpoint,
            side,
            initial_dcid,
            peer_cid,
            our_cid,
            handshake_keys: None,
            one_rtt_keys: None,
            initial_pn: 0,
            handshake_pn: 0,
            one_rtt_pn: 0,
            initial_crypto_sent: 0,
            handshake_crypto_sent: 0,
            initial_crypto_recv_offset: 0,
            handshake_crypto_recv_offset: 0,
            streams: BTreeMap::new(),
            reap_pending: false,
            recv_scratch: Vec::new(),
            recv_pn_ranges: Vec::new(),
            ack_pending: false,
            sent_packets: Vec::new(),
            retransmit_queue: Vec::new(),
            pending_ctrl: Vec::new(),
            reset_events: Vec::new(),
            recently_reset: Vec::new(),
            one_rtt_plaintext_budget: 1350,
            now_us: 0,
            srtt_us: 0,
            rttvar_us: 0,
            min_rtt_us: 0,
            has_rtt_sample: false,
            last_ae_sent_us: 0,
            pto_backoff: 0,
            ping_pending: false,
            cwnd: INITIAL_CWND_PACKETS * 1350,
            ssthresh: usize::MAX,
            cc_algo: CongestionAlgorithm::default(),
            cubic_w_max: 0,
            cubic_w_est: 0,
            cubic_epoch_start_us: 0,
            cubic_k_ms: 0,
            pc_earliest_sent_us: 0,
            pc_latest_sent_us: 0,
            keepalive_us: DEFAULT_KEEPALIVE_US,
            idle_timeout_us: DEFAULT_IDLE_TIMEOUT_US,
            last_inbound_us: 0,
            last_keepalive_us: 0,
            hs_started_us: 0,
            last_liveness_check_us: 0,
            bytes_in_flight: 0,
            recovery_until_pn: 0,
            handshake_confirmed: false,
            handshake_done_pending: false,
            handshake_done_queued_once: false,
            initial_crypto_log: Vec::new(),
            handshake_crypto_log: Vec::new(),
            last_hs_sent_us: 0,
            hs_pto_backoff: 0,
            resend_initial: false,
            resend_handshake: false,
            initial_crypto_oob: BTreeMap::new(),
            handshake_crypto_oob: BTreeMap::new(),
            next_local_bi_stream_id: match side {
                // RFC 9000 §2.1 stream-id bit 0: 0=client-initiated.
                // Bit 1: 0=bidi, 1=uni.
                Side::Client => 0,
                Side::Server => 1,
            },
            next_local_uni_stream_id: match side {
                Side::Client => 2,
                Side::Server => 3,
            },
            peer_initiated_uni: Vec::new(),
            peer_initiated_bi: Vec::new(),
            outbound_datagrams: Vec::new(),
            inbound_datagrams: Vec::new(),
            established: false,
        }
    }

    /// Drive one pump cycle: feed `incoming` (if any) and return
    /// outbound datagrams to send to the peer.
    pub fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, Error> {
        self.pump_with(incoming, incoming.is_none())
    }

    /// [`pump`](Self::pump) with explicit control over
    /// PURE-ACK emission. Layered pumps (L5 runs a second
    /// idle L4 pump per incoming datagram to flush its own
    /// writes) pass `false` mid-chain so only the truly
    /// idle pump at the chain's tail may spend a whole
    /// packet on a bare ACK — per-datagram pure ACKs
    /// nearly doubled the wire packet count.
    pub fn pump_with(
        &mut self,
        incoming: Option<&[u8]>,
        allow_pure_ack: bool,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let now = self.now_us;
        self.pump_at(incoming, allow_pure_ack, now)
    }

    /// The timed pump (1.6.0) — [`pump_with`](Self::pump_with)
    /// plus a caller-supplied monotonic clock reading in
    /// microseconds. The clock drives RTT estimation and the
    /// PTO / handshake-retransmit timers; it only moves
    /// forward (`max`), so interleaved untimed pumps are
    /// safe, and a never-timed connection (clock pinned at
    /// 0) never fires a timer — exactly the 1.5 behavior.
    ///
    /// Call with `incoming = None` periodically while idle
    /// (any cadence at or under the
    /// [`next_wakeup_us`](Self::next_wakeup_us) deadline's
    /// granularity) so expired timers get a chance to emit
    /// their probes.
    pub fn pump_at(
        &mut self,
        incoming: Option<&[u8]>,
        allow_pure_ack: bool,
        now_us: u64,
    ) -> Result<Vec<Vec<u8>>, Error> {
        self.now_us = self.now_us.max(now_us);
        if let Some(dg) = incoming {
            self.dispatch_datagram(dg)?;
        }
        // Drain pending CRYPTO bytes from TLS into a per-level
        // queue.
        loop {
            let (hs_bytes, kc) = self.endpoint.write_hs();
            let mut had_progress = !hs_bytes.is_empty();
            if !hs_bytes.is_empty() {
                // Route bytes to whichever level is currently
                // active for outgoing CRYPTO: if Handshake keys
                // are installed, use Handshake; else Initial.
                if self.handshake_keys.is_some() {
                    self.send_crypto_handshake(&hs_bytes)?;
                } else {
                    self.send_crypto_initial(&hs_bytes)?;
                }
            }
            if let Some(change) = kc {
                had_progress = true;
                match change {
                    QuicKeyChange::Handshake(k) => {
                        self.handshake_keys = Some(k);
                    }
                    QuicKeyChange::OneRtt(k) => {
                        self.one_rtt_keys = Some(k);
                    }
                }
            }
            if !had_progress {
                break;
            }
        }
        if !self.established && !self.endpoint.is_handshaking() && self.one_rtt_keys.is_some() {
            self.established = true;
            if self.side == Side::Server {
                // TLS completing server-side means the
                // client's Finished arrived — the whole
                // handshake is confirmed. Tell the client
                // (RFC 9001 §4.1.2: the server MUST send
                // HANDSHAKE_DONE) so it can drop its own
                // handshake retransmit state.
                self.confirm_handshake();
                if !self.handshake_done_queued_once {
                    self.handshake_done_queued_once = true;
                    self.handshake_done_pending = true;
                }
            }
        }

        // Fire expired timers (no-ops while the clock sits
        // at 0): PTO probes / handshake-flight resends are
        // queued here and picked up by the builders below.
        self.check_timers();

        // Link liveness: keepalive PING on a quiet link,
        // death past the idle timeout.
        self.check_liveness()?;

        // Now build outbound datagrams. Each level that has frames
        // to send gets one datagram.
        let mut out: Vec<Vec<u8>> = Vec::new();
        if let Some(dg) = self.build_initial_datagram()? {
            out.push(dg);
        }
        if let Some(dg) = self.build_handshake_datagram()? {
            out.push(dg);
        }
        if self.established {
            // v1.3: the 1-RTT builder emits MTU-sized
            // datagrams — loop until the send queues drain
            // so one pump flushes everything. PURE-ACK
            // packets only when the caller marked this
            // pump truly idle (see `pump_with`); ACKs
            // piggybacked on data packets are immediate.
            while let Some(dg) = self.build_one_rtt_datagram(allow_pure_ack)? {
                out.push(dg);
            }
        }
        // Reap soft-closed streams whose send queues have
        // fully drained (guarded so the sweep only runs
        // when a close is actually pending).
        if self.reap_pending {
            // A closing stream is reaped only once fully
            // ACKNOWLEDGED (not merely on the wire) — the
            // buffer is the retransmit source until then.
            self.streams
                .retain(|_, st| !(st.closing && st.fully_acked()));
            self.reap_pending = self.streams.values().any(|st| st.closing);
        }
        Ok(out)
    }

    /// Whether the connection has reached 1-RTT.
    pub fn is_established(&self) -> bool {
        self.established
    }

    /// Earliest armed timer deadline (µs on the caller's
    /// clock), or `None` when nothing is armed. A driver
    /// that parks may bound its sleep with this; coarser
    /// cadences (e.g. a fixed 500 µs park slice) merely add
    /// their slice to the probe latency.
    pub fn next_wakeup_us(&self) -> Option<u64> {
        let mut deadline: Option<u64> = None;
        if !self.handshake_confirmed && self.last_hs_sent_us > 0 {
            deadline = Some(self.last_hs_sent_us + (self.pto_us() << self.hs_pto_backoff));
        }
        if !self.sent_packets.is_empty() && self.last_ae_sent_us > 0 {
            let t = self.last_ae_sent_us + (self.pto_us() << self.pto_backoff);
            deadline = Some(deadline.map_or(t, |d| d.min(t)));
        }
        deadline
    }

    /// Smoothed RTT (RFC 9002 §5.3) in µs — `None` until
    /// the first sample (an ACK newly covering the largest
    /// acknowledged packet).
    pub fn smoothed_rtt_us(&self) -> Option<u64> {
        self.has_rtt_sample.then_some(self.srtt_us)
    }

    /// Current congestion window in 1-RTT plaintext bytes.
    pub fn cwnd_bytes(&self) -> usize {
        self.cwnd
    }

    /// Select the congestion-control algorithm (default
    /// [`CongestionAlgorithm::Cubic`]). Call at connection
    /// setup; switching mid-flight keeps the current window
    /// and re-anchors the growth law at the next event.
    pub fn set_congestion_algorithm(&mut self, algo: CongestionAlgorithm) {
        self.cc_algo = algo;
        self.cubic_epoch_start_us = 0;
    }

    /// Set the quiet-link probe interval (µs, default
    /// 2.5 s): a link with no inbound traffic for this
    /// long gets a PING — its ACK (any inbound packet) is
    /// the liveness proof, and the PTO machinery retries a
    /// lost probe. Liveness runs only on timed pumps
    /// ([`pump_at`](Self::pump_at)).
    pub fn set_keepalive_us(&mut self, keepalive_us: u64) {
        self.keepalive_us = keepalive_us.max(1);
    }

    /// Set the no-inbound death bound (µs, default 10 s):
    /// with no inbound traffic for this long — keepalive
    /// probes included — the link is declared dead and
    /// `pump_at` returns [`Error::Timeout`]. Also bounds a
    /// handshake against a peer that never answers.
    pub fn set_idle_timeout_us(&mut self, idle_timeout_us: u64) {
        self.idle_timeout_us = idle_timeout_us.max(1);
    }

    /// Link-liveness check (1.8.0), on the caller's clock.
    /// Quiet past the keepalive interval → queue a PING;
    /// no inbound past the idle timeout → the link is
    /// dead. Any inbound datagram resets both. Frozen
    /// clock (untimed pumps) = liveness off, like every
    /// other timer.
    fn check_liveness(&mut self) -> Result<(), Error> {
        let now = self.now_us;
        if now == 0 {
            return Ok(());
        }
        // Gap forgiveness: liveness verdicts are only valid
        // under continuous driving (a call's drive loop
        // pumps every ≲ 5 ms). A pump gap wider than the
        // keepalive interval means nobody was watching the
        // link — re-anchor instead of declaring a false
        // death on the first pump of the next call.
        if now.saturating_sub(self.last_liveness_check_us) > self.keepalive_us
            && self.last_liveness_check_us > 0
        {
            self.last_inbound_us = now.max(1);
        }
        self.last_liveness_check_us = now;
        if self.established {
            if self.last_inbound_us == 0 {
                return Ok(());
            }
            let quiet = now.saturating_sub(self.last_inbound_us);
            if quiet >= self.idle_timeout_us {
                return Err(Error::Timeout);
            }
            if quiet >= self.keepalive_us
                && now.saturating_sub(self.last_keepalive_us) >= self.keepalive_us
            {
                self.ping_pending = true;
                self.last_keepalive_us = now;
            }
        } else if self.hs_started_us > 0
            && now.saturating_sub(self.hs_started_us.max(self.last_inbound_us))
                >= self.idle_timeout_us
        {
            // Handshake started, peer quiet ever since —
            // connect-to-dead-server shape.
            return Err(Error::Timeout);
        }
        Ok(())
    }

    /// One congestion event per recovery episode: reduce
    /// the window by the algorithm's β and re-anchor its
    /// growth state.
    fn congestion_event(&mut self) {
        let floor = MIN_CWND_PACKETS * self.one_rtt_plaintext_budget;
        match self.cc_algo {
            CongestionAlgorithm::NewReno => {
                self.ssthresh = (self.cwnd / 2).max(floor);
                self.cwnd = self.ssthresh;
            }
            CongestionAlgorithm::Cubic => {
                // Fast convergence (RFC 9438 §4.6): a second
                // reduction before regaining the old plateau
                // lowers the plateau itself.
                self.cubic_w_max = if self.cwnd < self.cubic_w_max {
                    self.cwnd * 17 / 20
                } else {
                    self.cwnd
                };
                self.ssthresh = (self.cwnd * 7 / 10).max(floor);
                self.cwnd = self.ssthresh;
                self.cubic_w_est = self.cwnd;
                self.cubic_epoch_start_us = 0;
            }
        }
    }

    /// Congestion-avoidance growth for `acked` newly-acked
    /// bytes (slow start is shared and lives at the call
    /// site).
    fn ca_growth(&mut self, acked: usize) {
        let budget = self.one_rtt_plaintext_budget;
        match self.cc_algo {
            CongestionAlgorithm::NewReno => {
                self.cwnd += (budget * acked) / self.cwnd.max(1);
            }
            CongestionAlgorithm::Cubic => {
                if self.cubic_epoch_start_us == 0 {
                    // Anchor the epoch at the first CA ack
                    // after a congestion event (or after
                    // slow start overshoots ssthresh with
                    // no event history).
                    self.cubic_epoch_start_us = self.now_us.max(1);
                    if self.cubic_w_max <= self.cwnd {
                        self.cubic_w_max = self.cwnd;
                        self.cubic_k_ms = 0;
                    } else {
                        self.cubic_k_ms = cubic_k_ms(self.cubic_w_max, budget);
                    }
                    if self.cubic_w_est == 0 {
                        self.cubic_w_est = self.cwnd;
                    }
                }
                // TCP-friendly region estimator (Reno-rate).
                self.cubic_w_est += (budget * acked) / self.cubic_w_est.max(1);
                // Cubic target at t + srtt (the RFC's
                // look-ahead), clamped to [cwnd, 1.5·cwnd].
                let t_ms =
                    (self.now_us.saturating_sub(self.cubic_epoch_start_us) + self.srtt_us) / 1_000;
                let d_ms = t_ms as i64 - self.cubic_k_ms as i64;
                let w_cubic = cubic_window(self.cubic_w_max, d_ms, budget);
                if w_cubic < self.cubic_w_est {
                    // TCP-friendly region: never slower
                    // than Reno (governs at loopback RTTs,
                    // where K is seconds away).
                    self.cwnd = self.cwnd.max(self.cubic_w_est);
                } else {
                    let target = w_cubic.clamp(self.cwnd, self.cwnd + self.cwnd / 2);
                    self.cwnd += ((target - self.cwnd) * acked) / self.cwnd.max(1);
                }
            }
        }
    }

    /// Persistent congestion (RFC 9002 §7.6): fold a lost
    /// packet's send time into the open span; collapse the
    /// window once the span exceeds 3 × PTO. PTO-harvested
    /// packets count too — they are never "declared lost"
    /// by an ACK (they leave the ledger at harvest), and a
    /// blackout produces ONLY those.
    fn note_lost_for_persistent_congestion(&mut self, time_sent: u64) {
        if time_sent == 0 {
            // Untimed traffic — the detector stays dark.
            return;
        }
        if self.pc_earliest_sent_us == 0 {
            self.pc_earliest_sent_us = time_sent;
        } else {
            self.pc_earliest_sent_us = self.pc_earliest_sent_us.min(time_sent);
        }
        self.pc_latest_sent_us = self.pc_latest_sent_us.max(time_sent);
        let span = self.pc_latest_sent_us - self.pc_earliest_sent_us;
        if span > PERSISTENT_CONGESTION_PTOS * self.pto_us() {
            // The path was dead across the whole span:
            // minimum window, slow start from scratch
            // (ssthresh untouched, per the RFC pseudocode).
            self.cwnd = MIN_CWND_PACKETS * self.one_rtt_plaintext_budget;
            self.cubic_w_max = 0;
            self.cubic_w_est = 0;
            self.cubic_epoch_start_us = 0;
            self.pc_earliest_sent_us = 0;
            self.pc_latest_sent_us = 0;
        }
    }

    /// Probe timeout for the current estimator state:
    /// `srtt + max(4·rttvar, kGranularity)` (max_ack_delay
    /// is 0 — ACKs are never delayed on this wire), or
    /// `2 × kInitialRtt` before the first sample.
    fn pto_us(&self) -> u64 {
        if self.has_rtt_sample {
            self.srtt_us + (4 * self.rttvar_us).max(GRANULARITY_US)
        } else {
            2 * INITIAL_RTT_US
        }
    }

    fn rtt_sample(&mut self, sample_us: u64) {
        if self.has_rtt_sample {
            self.min_rtt_us = self.min_rtt_us.min(sample_us);
            let diff = self.srtt_us.abs_diff(sample_us);
            self.rttvar_us = (3 * self.rttvar_us + diff) / 4;
            self.srtt_us = (7 * self.srtt_us + sample_us) / 8;
        } else {
            self.srtt_us = sample_us;
            self.rttvar_us = sample_us / 2;
            self.min_rtt_us = sample_us;
            self.has_rtt_sample = true;
        }
    }

    /// Handshake confirmed — drop the retransmit logs and
    /// disarm the handshake timer.
    fn confirm_handshake(&mut self) {
        self.handshake_confirmed = true;
        self.initial_crypto_log = Vec::new();
        self.handshake_crypto_log = Vec::new();
        self.initial_crypto_oob.clear();
        self.handshake_crypto_oob.clear();
        self.resend_initial = false;
        self.resend_handshake = false;
    }

    /// Fire expired timers against the internal clock.
    ///
    /// Handshake plane: re-emit every unconfirmed level's
    /// full CRYPTO log (the receive side dedups).
    ///
    /// 1-RTT plane (RFC 9002 §6.2 adapted): the oldest
    /// unacked packet's spans go back on the retransmit
    /// queue — ahead of newer declared-lost spans — and the
    /// record leaves the in-flight ledger (departure from
    /// the RFC's keep-in-flight; duplicates are trimmed).
    /// A record with nothing retransmittable probes with
    /// PING. No cwnd reduction on PTO.
    fn check_timers(&mut self) {
        let now = self.now_us;
        if !self.handshake_confirmed && self.last_hs_sent_us > 0 {
            let pto = self.pto_us() << self.hs_pto_backoff;
            if now >= self.last_hs_sent_us + pto {
                if !self.initial_crypto_log.is_empty() {
                    self.resend_initial = true;
                }
                if !self.handshake_crypto_log.is_empty() && self.handshake_keys.is_some() {
                    self.resend_handshake = true;
                }
                self.hs_pto_backoff = (self.hs_pto_backoff + 1).min(MAX_PTO_BACKOFF);
            }
        }
        if !self.sent_packets.is_empty() && self.last_ae_sent_us > 0 {
            let pto = self.pto_us() << self.pto_backoff;
            if now >= self.last_ae_sent_us + pto {
                let rec = self.sent_packets.remove(0);
                self.bytes_in_flight = self.bytes_in_flight.saturating_sub(rec.size);
                self.note_lost_for_persistent_congestion(rec.time_sent);
                if rec.handshake_done {
                    self.handshake_done_pending = true;
                }
                let had_ctrl = !rec.ctrl.is_empty();
                if had_ctrl {
                    let mut q = rec.ctrl;
                    q.append(&mut self.pending_ctrl);
                    self.pending_ctrl = q;
                }
                if rec.spans.is_empty() && !rec.handshake_done && !had_ctrl {
                    self.ping_pending = true;
                }
                let mut q = rec.spans;
                q.append(&mut self.retransmit_queue);
                self.retransmit_queue = q;
                self.pto_backoff = (self.pto_backoff + 1).min(MAX_PTO_BACKOFF);
                // Re-arm from now; the probe send re-stamps.
                self.last_ae_sent_us = now.max(1);
            }
        }
    }

    /// ALPN protocol agreed during handshake.
    pub fn alpn_protocol(&self) -> Option<Vec<u8>> {
        self.endpoint.alpn_protocol()
    }

    /// Open a new locally-initiated bidirectional stream. Returns
    /// the assigned stream id.
    pub fn open_bi(&mut self) -> StreamId {
        let sid = self.next_local_bi_stream_id;
        self.next_local_bi_stream_id += 4;
        self.streams.insert(sid, StreamState::default());
        StreamId(sid)
    }

    /// Open a new locally-initiated unidirectional stream. Returns
    /// the assigned stream id. RFC 9000 §2.1 stream-id bit 1 = 1
    /// marks uni; bit 0 matches our [`Side`] (0 = client, 1 = server).
    /// L4 1.1.0 addition for L5 (`grpcx-h3`)'s control + QPACK
    /// uni-streams.
    pub fn open_uni(&mut self) -> StreamId {
        let sid = self.next_local_uni_stream_id;
        self.next_local_uni_stream_id += 4;
        self.streams.insert(sid, StreamState::default());
        StreamId(sid)
    }

    /// Pop the next peer-initiated unidirectional stream id we've
    /// observed (FIFO). Returns `None` when the queue is empty.
    /// L5 polls this each pump to discover new control / push /
    /// QPACK streams.
    pub fn accept_uni(&mut self) -> Option<StreamId> {
        if self.peer_initiated_uni.is_empty() {
            None
        } else {
            Some(StreamId(self.peer_initiated_uni.remove(0)))
        }
    }

    /// Pop the next peer-initiated bidirectional stream id we've
    /// observed (FIFO). Returns `None` when the queue is empty.
    /// L5 polls this each pump to discover incoming request
    /// streams (server side).
    pub fn accept_bi(&mut self) -> Option<StreamId> {
        if self.peer_initiated_bi.is_empty() {
            None
        } else {
            Some(StreamId(self.peer_initiated_bi.remove(0)))
        }
    }

    /// Set the 1-RTT plaintext budget per datagram (see the
    /// field note). The caller asserts the resulting
    /// datagram size fits the path MTU. Also re-arms the
    /// initial congestion window — the budget is the CC
    /// unit (RFC 9002 sizes cwnd in datagrams), and the
    /// call happens at connection setup, before traffic.
    pub fn set_datagram_budget(&mut self, plaintext_budget: usize) {
        self.one_rtt_plaintext_budget = plaintext_budget.max(64);
        self.cwnd = INITIAL_CWND_PACKETS * self.one_rtt_plaintext_budget;
        self.ssthresh = usize::MAX;
    }

    /// Queue `data` for the named stream. Returns the number of
    /// bytes accepted (always `data.len()` in v0.6 — no flow
    /// control).
    pub fn write_stream(
        &mut self,
        stream: StreamId,
        data: &[u8],
        fin: bool,
    ) -> Result<usize, Error> {
        let st = self.streams.entry(stream.0).or_default();
        st.pending_send.extend_from_slice(data);
        if fin {
            st.sent_fin = true;
        }
        Ok(data.len())
    }

    /// Drain currently-buffered bytes from the named stream into
    /// `out` and return `(bytes_read, fin)`. A `fin = true`
    /// indicates the peer finished the stream.
    pub fn read_stream(&mut self, stream: StreamId, out: &mut Vec<u8>) -> (usize, bool) {
        if let Some(st) = self.streams.get_mut(&stream.0) {
            let n = st.recv_buf.len();
            if out.is_empty() {
                // Zero-copy handoff: swap buffers instead
                // of copying, recycling the caller's spare
                // capacity into `recv_buf` for the next
                // packet burst.
                core::mem::swap(out, &mut st.recv_buf);
                st.recv_buf.clear();
            } else {
                out.extend_from_slice(&st.recv_buf);
                st.recv_buf.clear();
            }
            (n, st.recv_fin)
        } else {
            (0, false)
        }
    }

    /// Drop all local state for a finished stream (v1.3).
    /// The caller asserts both directions are done — any
    /// late-arriving frames for the id re-register a fresh
    /// (empty) stream entry. Without this, long-lived
    /// connections accumulate one dead `StreamState` per
    /// RPC and every per-stream sweep degrades linearly.
    pub fn close_stream(&mut self, stream: StreamId) {
        if let Some(st) = self.streams.get_mut(&stream.0) {
            let drained = st.fully_acked();
            if drained {
                self.streams.remove(&stream.0);
            } else {
                // Queued data not yet on the wire (or not
                // yet acknowledged) — mark and let the
                // 1-RTT builder reap once fully acked.
                st.closing = true;
                self.reap_pending = true;
            }
        }
    }

    /// Eager cancel (1.9): reset BOTH directions of the
    /// stream — RESET_STREAM for our send side (pending
    /// data + retransmit spans purged, final size = bytes
    /// already on the wire) and STOP_SENDING for the
    /// peer's. Best-effort one-shot frames (see the
    /// `pending_ctrl` note); the entry reaps once the
    /// peer's answering RESET_STREAM lands. No-op for
    /// unknown sids (call already complete).
    pub fn cancel_stream(&mut self, stream: StreamId, error_code: u64) {
        let Some(st) = self.streams.get_mut(&stream.0) else {
            return;
        };
        st.reset_sent = true;
        st.closing = true;
        // Stop sending: park the cursor at the end; purge
        // queued retransmits for this sid.
        st.send_cursor = st.pending_send.len();
        let final_size = st.sent_offset;
        self.retransmit_queue.retain(|sp| sp.sid != stream.0);
        self.pending_ctrl.push(Frame::ResetStream {
            stream_id: stream,
            application_error_code: error_code,
            final_size,
        });
        self.pending_ctrl.push(Frame::StopSending {
            stream_id: stream,
            application_error_code: error_code,
        });
        if !self.reset_events.contains(&stream.0) {
            self.reset_events.push(stream.0);
        }
    }

    /// Drain the sids whose stream life ended via reset
    /// since the last call (either direction, ours or the
    /// peer's) — the upper layer drops per-stream state.
    pub fn take_reset_streams(&mut self) -> Vec<u64> {
        core::mem::take(&mut self.reset_events)
    }

    fn note_recently_reset(&mut self, sid: u64) {
        if self.recently_reset.len() >= 64 {
            self.recently_reset.remove(0);
        }
        self.recently_reset.push(sid);
    }

    /// Receive side of RESET_STREAM: the peer abandoned
    /// their send direction — drop recv state, ignore late
    /// data, tell the upper layer.
    fn on_peer_reset_stream(&mut self, sid: u64) {
        if self.recently_reset.contains(&sid) {
            return;
        }
        let Some(st) = self.streams.get_mut(&sid) else {
            // Reset for a stream we never saw (or already
            // reaped) — remember it so trailing data is
            // dropped rather than re-registering.
            self.note_recently_reset(sid);
            return;
        };
        st.peer_reset = true;
        st.recv_buf.clear();
        st.recv_oob.clear();
        st.recv_fin = false;
        if !self.reset_events.contains(&sid) {
            self.reset_events.push(sid);
        }
        // Both directions dead — reap now; late in-flight
        // packets are filtered by `recently_reset`.
        if st.reset_sent {
            self.streams.remove(&sid);
            self.note_recently_reset(sid);
        }
    }

    /// Receive side of STOP_SENDING: the peer doesn't want
    /// our data — abandon the send direction and answer
    /// with RESET_STREAM (RFC 9000 §3.5).
    fn on_stop_sending(&mut self, sid: u64, error_code: u64) {
        let Some(st) = self.streams.get_mut(&sid) else {
            return;
        };
        if st.reset_sent {
            return; // already reset (e.g. our own cancel)
        }
        st.reset_sent = true;
        st.closing = true;
        st.send_cursor = st.pending_send.len();
        let final_size = st.sent_offset;
        self.retransmit_queue.retain(|sp| sp.sid != sid);
        self.pending_ctrl.push(Frame::ResetStream {
            stream_id: StreamId(sid),
            application_error_code: error_code,
            final_size,
        });
        if !self.reset_events.contains(&sid) {
            self.reset_events.push(sid);
        }
    }

    /// Peek at the named stream's receive buffer without
    /// consuming it. Test helper; returns the buffered length +
    /// FIN flag.
    pub fn peek_stream(&self, stream: StreamId, out: &mut Vec<u8>) -> (usize, bool) {
        if let Some(st) = self.streams.get(&stream.0) {
            out.extend_from_slice(&st.recv_buf);
            (st.recv_buf.len(), st.recv_fin)
        } else {
            (0, false)
        }
    }

    /// `true` if the stream has any buffered bytes or has been
    /// FIN'd by the peer.
    pub fn streams_have_data(&self, stream: StreamId) -> bool {
        self.streams
            .get(&stream.0)
            .is_some_and(|st| !st.recv_buf.is_empty() || st.recv_fin)
    }

    /// Queue an unreliable datagram (RFC 9221) for delivery on
    /// the next 1-RTT outbound packet. 1.2.0+ addition.
    ///
    /// Requires 1-RTT keys to be installed (handshake complete);
    /// before then datagrams have no protected level to ride on,
    /// so this returns `Error::BadFrame`. The payload itself
    /// isn't capped here — oversize payloads either fit one
    /// packet's plaintext budget (sent fine) or hit the wire-
    /// build path and surface as a packetisation error in v1.x's
    /// max-datagram-size enforcement.
    pub fn send_datagram(&mut self, payload: &[u8]) -> Result<(), Error> {
        if self.one_rtt_keys.is_none() {
            return Err(Error::BadFrame);
        }
        self.outbound_datagrams.push(payload.to_vec());
        Ok(())
    }

    /// Pop the next inbound DATAGRAM payload, or `None` if the
    /// queue is empty. 1.2.0+ addition.
    pub fn recv_datagram(&mut self) -> Option<Vec<u8>> {
        if self.inbound_datagrams.is_empty() {
            None
        } else {
            Some(self.inbound_datagrams.remove(0))
        }
    }

    // ============================================================
    // CRYPTO send queues — one byte stream per encryption level
    // per RFC 9001 §4 ("CRYPTO bytes are separate per level").
    // We track the next offset to assign and emit them as CRYPTO
    // frames in the next outbound packet at that level.
    // ============================================================

    fn send_crypto_initial(&mut self, bytes: &[u8]) -> Result<(), Error> {
        // Queue into a virtual stream tagged as Initial-level
        // CRYPTO; the build_initial_datagram below will drain it.
        let id = pseudo_stream_id_initial_crypto();
        let st = self.streams.entry(id).or_default();
        st.pending_send.extend_from_slice(bytes);
        Ok(())
    }

    fn send_crypto_handshake(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let id = pseudo_stream_id_handshake_crypto();
        let st = self.streams.entry(id).or_default();
        st.pending_send.extend_from_slice(bytes);
        Ok(())
    }

    // ============================================================
    // Outbound packet builders.
    // ============================================================

    fn build_initial_datagram(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let id = pseudo_stream_id_initial_crypto();
        // New TLS bytes append to the retransmit log first,
        // so a timer-driven resend (full log, offset 0)
        // always covers everything ever sent at this level.
        let mut took_new = false;
        if let Some(st) = self.streams.get_mut(&id)
            && !st.send_is_empty()
        {
            st.send_cursor = 0;
            let bytes = core::mem::take(&mut st.pending_send);
            self.initial_crypto_sent += bytes.len() as u64;
            self.initial_crypto_log.extend_from_slice(&bytes);
            took_new = true;
        }
        let resend = core::mem::take(&mut self.resend_initial);
        // Every Initial emission is the FULL log from offset
        // 0 — the only shape the Initial packet builder
        // supports — and the receive side trims overlap, so
        // new-bytes sends and timer resends are one path.
        if !(took_new || resend) || self.initial_crypto_log.is_empty() {
            return Ok(None);
        }
        let pn = self.initial_pn;
        self.initial_pn += 1;
        // First client Initial flight MUST be ≥ 1200 bytes (RFC
        // 9000 §14.1). Tests always pad to 1200 to keep things
        // simple; the loopback bus doesn't care about size.
        let padded_to = 1200;
        let crypto_bytes = self.initial_crypto_log.clone();
        let dg = build_initial_with_offset(
            &self.peer_cid,
            &self.our_cid,
            &[],
            pn,
            &crypto_bytes,
            0,
            self.side,
            &self.initial_dcid,
            padded_to,
        )?;
        self.last_hs_sent_us = self.now_us.max(1);
        if self.hs_started_us == 0 {
            self.hs_started_us = self.last_hs_sent_us;
        }
        Ok(Some(dg))
    }

    fn build_handshake_datagram(&mut self) -> Result<Option<Vec<u8>>, Error> {
        if self.handshake_keys.is_none() {
            return Ok(None);
        }
        let id = pseudo_stream_id_handshake_crypto();
        let mut crypto_bytes: Option<(Vec<u8>, u64)> = None;
        if let Some(st) = self.streams.get_mut(&id)
            && !st.send_is_empty()
        {
            st.send_cursor = 0;
            let bytes = core::mem::take(&mut st.pending_send);
            let offset = self.handshake_crypto_sent;
            self.handshake_crypto_sent += bytes.len() as u64;
            self.handshake_crypto_log.extend_from_slice(&bytes);
            crypto_bytes = Some((bytes, offset));
        }
        if crypto_bytes.is_none() && core::mem::take(&mut self.resend_handshake) {
            // Timer-driven resend: the full log from offset
            // 0 (the receive side trims what it already has).
            if !self.handshake_crypto_log.is_empty() {
                crypto_bytes = Some((self.handshake_crypto_log.clone(), 0));
            }
        }
        let Some((bytes, offset)) = crypto_bytes else {
            return Ok(None);
        };
        let keys = self.handshake_keys.as_ref().expect("checked above");
        let pn = self.handshake_pn;
        self.handshake_pn += 1;

        let mut plaintext: Vec<u8> = Vec::new();
        encode_frame(
            &Frame::Crypto {
                offset,
                data: bytes,
            },
            &mut plaintext,
        )?;
        let dg = build_long_packet(
            LongHeaderType::Handshake,
            &self.peer_cid,
            &self.our_cid,
            None,
            pn,
            keys,
            &plaintext,
        )?;
        self.last_hs_sent_us = self.now_us.max(1);
        if self.hs_started_us == 0 {
            self.hs_started_us = self.last_hs_sent_us;
        }
        Ok(Some(dg))
    }

    /// Build at most one budget-sized 1-RTT datagram,
    /// leaving the rest pending for the next call (`pump`
    /// loops until drained). Stream data is CHUNKED to
    /// `one_rtt_plaintext_budget` (see the field note).
    /// Frame order: retransmits, then new stream data,
    /// then RFC 9221 datagrams, then the owed ACK —
    /// piggybacked onto any packet that already carries
    /// something; emitted as a PURE-ACK packet only when
    /// `allow_pure_ack` (the idle-pump batch tail). Every
    /// ack-eliciting packet is recorded for loss recovery.
    fn build_one_rtt_datagram(&mut self, allow_pure_ack: bool) -> Result<Option<Vec<u8>>, Error> {
        let keys = self.one_rtt_keys.as_ref();
        let Some(keys) = keys else {
            return Ok(None);
        };
        // Frames encode straight into the packet buffer
        // after the header (v1.9 — see begin_short_packet);
        // `payload len` below = packet.len() - header_len.
        let (mut packet, header_len) =
            begin_short_packet(&self.peer_cid, self.one_rtt_plaintext_budget + 64);
        let mut sent_spans: Vec<RetransmitSpan> = Vec::new();

        // STREAM frames, chunked to the budget. ~30 bytes
        // covers the worst-case frame header (type + three
        // 8-byte varints).
        const FRAME_OVERHEAD: usize = 30;

        // Control frames owed to the peer ride first — both
        // are one/few bytes and ack-eliciting.
        let sent_hsd = core::mem::take(&mut self.handshake_done_pending);
        if sent_hsd {
            encode_frame(&Frame::HandshakeDone, &mut packet)?;
        }
        let sent_ping = core::mem::take(&mut self.ping_pending);
        if sent_ping {
            encode_frame(&Frame::Ping, &mut packet)?;
        }

        // Cancel control frames (RESET_STREAM /
        // STOP_SENDING — eager cancel, 1.9): tiny and
        // ack-eliciting; recorded per packet and re-queued
        // on loss (1.9.1).
        let mut sent_ctrl: Vec<Frame> = Vec::new();
        while let Some(f) = self.pending_ctrl.first() {
            if (packet.len() - header_len) + FRAME_OVERHEAD >= self.one_rtt_plaintext_budget {
                break;
            }
            let f = f.clone();
            encode_frame(&f, &mut packet)?;
            self.pending_ctrl.remove(0);
            sent_ctrl.push(f);
        }

        // Congestion gate (NewReno): NEW stream data and
        // DATAGRAM frames wait for window; retransmits,
        // ACKs, and probe frames are exempt (recovery must
        // not starve the recovery data itself).
        let cc_open = self.bytes_in_flight < self.cwnd;

        // Declared-lost spans first — the peer is stalled
        // on exactly these bytes.
        while let Some(span) = self.retransmit_queue.first() {
            if (packet.len() - header_len) + FRAME_OVERHEAD >= self.one_rtt_plaintext_budget {
                break;
            }
            let span = span.clone();
            let Some(st) = self.streams.get(&span.sid) else {
                // Stream reaped post-ack — span is moot.
                self.retransmit_queue.remove(0);
                continue;
            };
            let room = self.one_rtt_plaintext_budget - (packet.len() - header_len) - FRAME_OVERHEAD;
            let take = span.len.min(room);
            let buf_start = (span.offset - st.base_offset) as usize;
            crate::frame::encode_stream_frame_into(
                &mut packet,
                span.sid,
                span.offset,
                &st.pending_send[buf_start..buf_start + take],
                span.fin && take == span.len,
            )?;
            sent_spans.push(RetransmitSpan {
                sid: span.sid,
                offset: span.offset,
                len: take,
                fin: span.fin && take == span.len,
            });
            if take == span.len {
                self.retransmit_queue.remove(0);
            } else {
                // Partial fit — shrink the head in place.
                let head = &mut self.retransmit_queue[0];
                head.offset += take as u64;
                head.len -= take;
            }
        }

        for (sid, st) in self.streams.iter_mut() {
            if !cc_open {
                break;
            }
            if (packet.len() - header_len) + FRAME_OVERHEAD >= self.one_rtt_plaintext_budget {
                break;
            }
            if is_pseudo_stream(*sid) {
                continue;
            }
            if st.reset_sent {
                continue; // cancelled — no more STREAM frames
            }
            let fin_pending = st.sent_fin && !st.fin_sent_on_wire;
            if st.send_is_empty() && !fin_pending {
                continue;
            }
            let room = self.one_rtt_plaintext_budget - (packet.len() - header_len) - FRAME_OVERHEAD;
            let take = st.send_pending_len().min(room);
            // Cursor advance — no front-drain, no tail
            // memmove (see the `send_cursor` field note).
            // The queue slice encodes straight into the
            // packet plaintext (v0.15: was a `to_vec`
            // round-trip through an owned `Frame`). The
            // sent bytes stay in `pending_send` until
            // acknowledged (the retransmit source).
            let offset = st.sent_offset;
            let will_be_empty = take == st.send_pending_len();
            // FIN rides only on the frame that carries the
            // final byte (or an empty frame once the queue
            // is drained).
            let fin = st.sent_fin && will_be_empty;
            if take == 0 && !fin {
                continue;
            }
            crate::frame::encode_stream_frame_into(
                &mut packet,
                *sid,
                offset,
                &st.pending_send[st.send_cursor..st.send_cursor + take],
                fin,
            )?;
            st.send_cursor += take;
            st.sent_offset += take as u64;
            if fin {
                st.fin_sent_on_wire = true;
            }
            sent_spans.push(RetransmitSpan {
                sid: *sid,
                offset,
                len: take,
                fin,
            });
        }

        // DATAGRAM frames (RFC 9221) are indivisible — pack
        // whole ones while they fit; an oversized one goes
        // out alone (its sender chose to exceed the MTU
        // floor; RFC 9221 §5 leaves that to the
        // application). Ack-eliciting, but explicitly
        // unreliable — never recorded for retransmission.
        let mut sent_datagram_frame = false;
        while let Some(payload) = self.outbound_datagrams.first() {
            if !cc_open {
                break;
            }
            let needed = payload.len() + FRAME_OVERHEAD;
            let fits = (packet.len() - header_len) + needed <= self.one_rtt_plaintext_budget;
            if fits || packet.len() == header_len {
                let payload = self.outbound_datagrams.remove(0);
                encode_frame(&Frame::Datagram { data: payload }, &mut packet)?;
                sent_datagram_frame = true;
                if !fits {
                    break; // oversized frame went out alone
                }
            } else {
                break; // next datagram waits for its own packet
            }
        }

        // The owed ACK: piggyback onto any packet already
        // carrying frames; otherwise a pure-ACK packet,
        // but only on the idle pump (pure-ACK packets are
        // themselves not ack-eliciting — no ping-pong).
        if self.ack_pending
            && !self.recv_pn_ranges.is_empty()
            && (packet.len() > header_len || allow_pure_ack)
        {
            encode_frame(&ack_frame_from_ranges(&self.recv_pn_ranges), &mut packet)?;
            self.ack_pending = false;
        }

        if packet.len() == header_len {
            return Ok(None);
        }
        let payload_len = packet.len() - header_len;
        let pn = self.one_rtt_pn;
        self.one_rtt_pn += 1;
        // Record ack-eliciting packets for loss recovery +
        // RTT + congestion accounting (a pure-ACK packet is
        // not ack-eliciting and is never retransmitted).
        if !sent_spans.is_empty()
            || sent_datagram_frame
            || sent_hsd
            || sent_ping
            || !sent_ctrl.is_empty()
        {
            self.bytes_in_flight += payload_len;
            self.last_ae_sent_us = self.now_us.max(1);
            self.sent_packets.push(SentRecord {
                pn: pn as u64,
                spans: sent_spans,
                time_sent: self.now_us,
                size: payload_len,
                handshake_done: sent_hsd,
                ctrl: sent_ctrl,
            });
        }
        seal_short_packet(&mut packet, header_len, pn, keys)?;
        Ok(Some(packet))
    }

    // ============================================================
    // Receive-side dispatch.
    // ============================================================

    fn dispatch_datagram(&mut self, datagram: &[u8]) -> Result<(), Error> {
        // We don't do coalesced packets in v0.6; one datagram =
        // one packet. Identify form via the first byte.
        if datagram.is_empty() {
            return Err(Error::BadPacket);
        }
        // Any datagram on the wire proves the link is alive
        // (liveness, 1.8.0) — even one we end up dropping.
        self.last_inbound_us = self.now_us.max(1);
        // In-place HP removal / decrypt need a mutable
        // copy; reuse the per-connection scratch instead
        // of a fresh allocation per packet. The buffer is
        // moved out for the duration of the dispatch (the
        // packet methods take `&mut self` too) and moved
        // back after, keeping its capacity.
        let mut packet = core::mem::take(&mut self.recv_scratch);
        packet.clear();
        packet.extend_from_slice(datagram);
        let r = if packet[0] & 0x80 != 0 {
            // Long header — Initial / Handshake / 0-RTT / Retry.
            self.dispatch_long_packet(&mut packet)
        } else {
            // Short header — 1-RTT.
            self.dispatch_short_packet(&mut packet)
        };
        self.recv_scratch = packet;
        r
    }

    fn dispatch_long_packet(&mut self, packet: &mut [u8]) -> Result<(), Error> {
        // The Long-header type bits live at first-byte bits 4-5;
        // header protection only masks the low 4 bits, so the type
        // is safe to read pre-HP-unmask.
        let type_bits = (packet[0] >> 4) & 0x03;
        let type_ = match type_bits {
            0b00 => LongHeaderType::Initial,
            0b10 => LongHeaderType::Handshake,
            // 0-RTT / Retry not supported in v0.6 — silently drop.
            _ => return Ok(()),
        };

        match type_ {
            LongHeaderType::Initial => {
                // Server case: the first Initial arrives → adopt
                // the client's CIDs as our peer/own identifiers.
                if self.side == Side::Server && self.initial_dcid.is_empty() {
                    let (peeked_header, _) = peek_long_header(packet)?;
                    let peeked_dcid: Vec<u8> =
                        peeked_header.dcid.bytes[..peeked_header.dcid.len as usize].to_vec();
                    self.initial_dcid = peeked_dcid;
                    self.our_cid = peeked_header.dcid;
                    self.peer_cid = peeked_header.scid;
                }
                let sender_side = match self.side {
                    Side::Client => Side::Server,
                    Side::Server => Side::Client,
                };
                let (long, frames) = parse_initial_packet(packet, &self.initial_dcid, sender_side)?;
                self.dispatch_frames(EncryptionLevel::Initial, &frames, &long)?;
            }
            LongHeaderType::Handshake => {
                // No handshake keys yet — the Initial that
                // carries them was lost or reordered behind
                // this packet. Drop; the peer's handshake
                // timer re-sends (1.6.0; was a fatal
                // `BadCrypto`).
                let Some(keys) = self.handshake_keys.as_ref() else {
                    return Ok(());
                };
                let (long, frames) = parse_long_packet(packet, keys)?;
                self.dispatch_frames(EncryptionLevel::Handshake, &frames, &long)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn dispatch_short_packet(&mut self, packet: &mut [u8]) -> Result<(), Error> {
        // 1-RTT keys not installed — a reordered/early short
        // packet during a lossy handshake. Drop; timers
        // recover (1.6.0; was a fatal `BadCrypto`).
        let Some(keys) = self.one_rtt_keys.as_ref() else {
            return Ok(());
        };
        let (header_len, plaintext_len, pn) = decrypt_short_packet(packet, &self.our_cid, keys)?;
        // STREAM payload slices borrow `packet` (a caller
        // buffer, not a `self` field) and route through the
        // offset-aware reassembly — the in-place fast path
        // that replaced the owned-`Frame` round-trip (one
        // memcpy per packet on the 64 KiB lane).
        let mut cursor: &[u8] = &packet[header_len..header_len + plaintext_len];
        let mut ack_eliciting = false;
        while !cursor.is_empty() {
            let type_byte = cursor[0];
            if (0x08..=0x0f).contains(&type_byte) {
                // STREAM fast path — mirror of
                // `frame::decode_stream`, asserted
                // byte-equivalent by the loopback suites.
                ack_eliciting = true;
                let bits = type_byte - 0x08;
                let fin = bits & 0b001 != 0;
                let has_len = bits & 0b010 != 0;
                let has_off = bits & 0b100 != 0;
                cursor = &cursor[1..];
                let sid = crate::varint::read_varint(&mut cursor)?;
                let offset = if has_off {
                    crate::varint::read_varint(&mut cursor)?
                } else {
                    0
                };
                let data = if has_len {
                    let len = crate::varint::read_varint(&mut cursor)? as usize;
                    if cursor.len() < len {
                        return Err(Error::BadFrame);
                    }
                    let (d, rest) = cursor.split_at(len);
                    cursor = rest;
                    d
                } else {
                    let d = cursor;
                    cursor = &cursor[cursor.len()..];
                    d
                };
                self.accept_stream_data(sid, offset, data, fin);
            } else {
                let f = decode_frame(&mut cursor)?;
                if !matches!(f, Frame::Ack { .. } | Frame::Padding) {
                    ack_eliciting = true;
                }
                self.handle_frame(&f)?;
            }
        }
        // Track the packet number for our next ACK; only
        // ack-eliciting packets create the obligation to
        // send one (a pure-ACK packet acknowledging a
        // pure-ACK packet would ping-pong forever).
        merge_span(&mut self.recv_pn_ranges, pn, pn + 1);
        if ack_eliciting {
            self.ack_pending = true;
        }
        Ok(())
    }

    /// Offset-aware receive: deliver in-order bytes into
    /// `recv_buf`, trim duplicates (retransmits make them
    /// inevitable), park out-of-order segments until the
    /// contiguous prefix catches up. FIN is honored only
    /// once the prefix reaches the final size.
    fn accept_stream_data(&mut self, sid: u64, offset: u64, data: &[u8], fin: bool) {
        // Late in-flight data for a reset stream is
        // dropped, not re-registered (eager cancel, 1.9).
        if self.recently_reset.contains(&sid) {
            return;
        }
        if let Some(st) = self.streams.get(&sid)
            && st.peer_reset
        {
            return;
        }
        let is_new = !self.streams.contains_key(&sid);
        if is_new {
            self.note_peer_initiated(sid);
        }
        let st = self.streams.entry(sid).or_default();
        let end = offset + data.len() as u64;
        if fin {
            st.fin_offset = Some(end);
        }
        if end <= st.recv_offset {
            // Entirely a duplicate.
        } else if offset <= st.recv_offset {
            // Extends the contiguous prefix (trim overlap).
            let skip = (st.recv_offset - offset) as usize;
            st.recv_buf.extend_from_slice(&data[skip..]);
            st.recv_offset = end;
            // Pull any parked segments the prefix now reaches.
            while let Some((&s, _)) = st.recv_oob.first_key_value() {
                if s > st.recv_offset {
                    break;
                }
                let (s, seg) = st.recv_oob.pop_first().expect("checked first");
                let seg_end = s + seg.len() as u64;
                if seg_end > st.recv_offset {
                    let skip = (st.recv_offset - s) as usize;
                    st.recv_buf.extend_from_slice(&seg[skip..]);
                    st.recv_offset = seg_end;
                }
            }
        } else {
            // A hole before this segment — park it.
            st.recv_oob.insert(offset, data.to_vec());
        }
        if st.fin_offset == Some(st.recv_offset) {
            st.recv_fin = true;
        }
    }

    fn dispatch_frames(
        &mut self,
        level: EncryptionLevel,
        frames: &[Frame],
        _long: &LongHeader,
    ) -> Result<(), Error> {
        for f in frames {
            if let Frame::Crypto { offset, data } = f {
                // Feed CRYPTO bytes into the TLS state
                // machine in order. Retransmits make
                // duplicates and reordering inevitable
                // (1.6.0) — trim overlap against the
                // contiguous prefix, park out-of-order
                // segments (mirror of the stream path).
                let (recv_offset, oob) = match level {
                    EncryptionLevel::Initial => (
                        &mut self.initial_crypto_recv_offset,
                        &mut self.initial_crypto_oob,
                    ),
                    EncryptionLevel::Handshake => (
                        &mut self.handshake_crypto_recv_offset,
                        &mut self.handshake_crypto_oob,
                    ),
                    EncryptionLevel::OneRtt => return Err(Error::BadFrame),
                };
                let mut in_order: Vec<u8> = Vec::new();
                crypto_accept(recv_offset, oob, *offset, data, &mut in_order);
                if !in_order.is_empty() {
                    self.endpoint
                        .read_hs(&in_order)
                        .map_err(|_| Error::BadCrypto)?;
                }
            }
        }
        Ok(())
    }

    fn handle_frame(&mut self, f: &Frame) -> Result<(), Error> {
        match f {
            Frame::Stream {
                stream_id,
                offset,
                data,
                fin,
            } => {
                self.accept_stream_data(stream_id.0, *offset, data, *fin);
            }
            Frame::Ack {
                largest_acked,
                first_ack_range,
                ranges,
                ..
            } => {
                self.process_ack(*largest_acked, *first_ack_range, ranges)?;
            }
            Frame::Padding | Frame::Ping => {}
            Frame::HandshakeDone => {
                // RFC 9001 §4.1.2 — the server confirms the
                // whole handshake; the client drops its
                // handshake-plane retransmit state.
                self.confirm_handshake();
            }
            Frame::Datagram { data } => {
                // RFC 9221 §4 — unreliable, push raw payload onto
                // the inbound queue. Caller drains via
                // `recv_datagram`.
                self.inbound_datagrams.push(data.clone());
            }
            Frame::ResetStream { stream_id, .. } => {
                self.on_peer_reset_stream(stream_id.0);
            }
            Frame::StopSending {
                stream_id,
                application_error_code,
            } => {
                self.on_stop_sending(stream_id.0, *application_error_code);
            }
            _ => {}
        }
        Ok(())
    }

    /// Process a received ACK frame (RFC 9000 §19.3.1
    /// range decoding): release acknowledged stream spans,
    /// then declare unacked packets more than
    /// kPacketThreshold (3, RFC 9002 §6.1.1) below the
    /// largest-acknowledged lost and queue their spans for
    /// retransmission.
    fn process_ack(
        &mut self,
        largest_acked: u64,
        first_ack_range: u64,
        ranges: &[(u64, u64)],
    ) -> Result<(), Error> {
        // Decode the acked PN ranges (descending on the
        // wire), as inclusive (lo, hi) pairs.
        let mut acked: Vec<(u64, u64)> = Vec::with_capacity(1 + ranges.len());
        let mut smallest = largest_acked
            .checked_sub(first_ack_range)
            .ok_or(Error::BadFrame)?;
        acked.push((smallest, largest_acked));
        for &(gap, range) in ranges {
            let hi = smallest
                .checked_sub(gap)
                .and_then(|v| v.checked_sub(2))
                .ok_or(Error::BadFrame)?;
            let lo = hi.checked_sub(range).ok_or(Error::BadFrame)?;
            acked.push((lo, hi));
            smallest = lo;
        }
        let is_acked = |pn: u64| acked.iter().any(|&(lo, hi)| pn >= lo && pn <= hi);

        // Release acked spans; collect newly-lost spans;
        // account the congestion window.
        let mut lost: Vec<RetransmitSpan> = Vec::new();
        let mut kept: Vec<SentRecord> = Vec::new();
        let mut newly_acked_bytes = 0usize;
        let mut largest_newly_acked: Option<(u64, u64)> = None;
        let mut largest_lost_pn: Option<u64> = None;
        let mut lost_sent_times: Vec<u64> = Vec::new();
        for rec in self.sent_packets.drain(..) {
            if is_acked(rec.pn) {
                newly_acked_bytes += rec.size;
                self.bytes_in_flight = self.bytes_in_flight.saturating_sub(rec.size);
                if largest_newly_acked.is_none_or(|(pn, _)| rec.pn > pn) {
                    largest_newly_acked = Some((rec.pn, rec.time_sent));
                }
                for span in rec.spans {
                    if let Some(st) = self.streams.get_mut(&span.sid) {
                        st.ack_span(span.offset, span.offset + span.len as u64);
                        if span.fin {
                            st.fin_acked = true;
                        }
                    }
                }
            } else if rec.pn + PACKET_THRESHOLD <= largest_acked {
                // Lost by packet threshold — queue for
                // retransmission (data still lives in the
                // stream buffer until acked).
                self.bytes_in_flight = self.bytes_in_flight.saturating_sub(rec.size);
                if rec.handshake_done {
                    self.handshake_done_pending = true;
                }
                if !rec.ctrl.is_empty() {
                    // Lost cancel frames go back to the
                    // front — the peer is wasting work on
                    // exactly these streams.
                    let mut q = rec.ctrl;
                    q.append(&mut self.pending_ctrl);
                    self.pending_ctrl = q;
                }
                if largest_lost_pn.is_none_or(|pn| rec.pn > pn) {
                    largest_lost_pn = Some(rec.pn);
                }
                lost_sent_times.push(rec.time_sent);
                lost.extend(rec.spans);
            } else {
                kept.push(rec);
            }
        }
        self.sent_packets = kept;
        if let Some((pn, time_sent)) = largest_newly_acked {
            // Forward progress: reset the probe backoff and
            // close the persistent-congestion span. An RTT
            // sample is taken only when the frame's
            // largest-acknowledged is itself newly acked
            // (RFC 9002 §5.1) and the clock has moved.
            self.pto_backoff = 0;
            self.pc_earliest_sent_us = 0;
            self.pc_latest_sent_us = 0;
            if pn == largest_acked && self.now_us > time_sent {
                self.rtt_sample(self.now_us - time_sent);
            }
            // Window growth, unless the ACK is for a packet
            // sent before the current recovery episode.
            if pn >= self.recovery_until_pn {
                if self.cwnd < self.ssthresh {
                    // Slow start (shared by both algorithms).
                    self.cwnd += newly_acked_bytes;
                } else {
                    self.ca_growth(newly_acked_bytes);
                }
            }
        }
        if !lost.is_empty() {
            // One window reduction per recovery episode, not
            // per lost packet (RFC 9002 §7.3.1).
            if largest_lost_pn.expect("lost is non-empty") >= self.recovery_until_pn {
                self.congestion_event();
                self.recovery_until_pn = self.one_rtt_pn as u64;
            }
            for t in lost_sent_times {
                self.note_lost_for_persistent_congestion(t);
            }
            self.retransmit_queue.extend(lost);
            // A retransmit-bearing flush may be the only
            // wake the peer gets — the builder runs on the
            // next pump.
        }
        Ok(())
    }

    /// Classify a newly-observed stream id and push it onto the
    /// peer-initiated queue if it actually came from the peer
    /// (RFC 9000 §2.1: stream-id bit 0 = 0 client-initiated,
    /// bit 1 = 1 uni). Internal CRYPTO pseudo streams + locally-
    /// initiated streams are ignored.
    fn note_peer_initiated(&mut self, sid: u64) {
        if is_pseudo_stream(sid) {
            return;
        }
        let peer_initiated_bit: u64 = match self.side {
            Side::Client => 1, // peer = server
            Side::Server => 0, // peer = client
        };
        if (sid & 1) != peer_initiated_bit {
            return;
        }
        if sid & 2 != 0 {
            self.peer_initiated_uni.push(sid);
        } else {
            self.peer_initiated_bi.push(sid);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncryptionLevel {
    Initial,
    Handshake,
    #[allow(dead_code)]
    // reserved for v0.7+ — 1-RTT path goes through `handle_frame` directly today.
    OneRtt,
}

fn empty_cid() -> ConnectionId {
    ConnectionId {
        bytes: [0u8; 20],
        len: 0,
    }
}

fn cid_from_bytes(bytes: &[u8]) -> Result<ConnectionId, Error> {
    let len = bytes.len().min(8);
    let mut out = ConnectionId {
        bytes: [0u8; 20],
        len: len as u8,
    };
    out.bytes[..len].copy_from_slice(&bytes[..len]);
    Ok(out)
}

/// Deterministic 8-byte derivation from a seed string + 24-bit
/// tag — used to bootstrap CIDs in the loopback test where we want
/// repeatable bytes without a random source.
fn hash_bytes(seed: &[u8], tag: u32) -> [u8; 8] {
    // FNV-1a 64-bit — small, no deps, plenty for non-cryptographic
    // ID generation.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (tag as u64);
    for &b in seed {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h.to_be_bytes()
}

// Pseudo stream IDs that we use internally to queue CRYPTO bytes
// per level. Picked outside the RFC 9000 stream-id range
// (u62) so they can't collide with real stream IDs.
fn pseudo_stream_id_initial_crypto() -> u64 {
    u64::MAX
}
fn pseudo_stream_id_handshake_crypto() -> u64 {
    u64::MAX - 1
}
fn is_pseudo_stream(id: u64) -> bool {
    id >= u64::MAX - 1
}

// ============================================================
// Helper: build an Initial packet with a caller-chosen CRYPTO
// offset. Wraps `initial::build_initial_packet` (which always
// uses offset 0).
// ============================================================

#[allow(clippy::too_many_arguments)]
fn build_initial_with_offset(
    dcid: &ConnectionId,
    scid: &ConnectionId,
    token: &[u8],
    packet_number: u32,
    crypto_bytes: &[u8],
    crypto_offset: u64,
    side: Side,
    initial_dcid_for_keys: &[u8],
    padded_to: usize,
) -> Result<Vec<u8>, Error> {
    if crypto_offset == 0 {
        return build_initial_packet_with_initial_dcid(
            dcid,
            scid,
            token,
            packet_number,
            crypto_bytes,
            side,
            padded_to,
            initial_dcid_for_keys,
        );
    }
    // Multi-Initial-packet flows aren't exercised by v0.6 loopback.
    Err(Error::BadFrame)
}

// ============================================================
// Long-packet build using rustls's per-level keys (Handshake).
// ============================================================

fn build_long_packet(
    type_: LongHeaderType,
    dcid: &ConnectionId,
    scid: &ConnectionId,
    token: Option<&[u8]>,
    packet_number: u32,
    keys: &QuicKeys,
    plaintext: &[u8],
) -> Result<Vec<u8>, Error> {
    let payload_len_value = (PN_LEN as u64) + (plaintext.len() as u64) + (TAG_LEN as u64);

    let mut packet: Vec<u8> = Vec::new();
    encode_header(
        &PacketHeader::Long(LongHeader {
            type_,
            version: 0x0000_0001,
            dcid: *dcid,
            scid: *scid,
            token: token.map(|t| t.to_vec()),
            payload_len: payload_len_value,
            packet_number,
            pn_len: PN_LEN,
        }),
        &mut packet,
    )?;
    let header_len = packet.len();
    let pn_offset = header_len - PN_LEN as usize;

    packet.extend_from_slice(plaintext);
    packet.extend_from_slice(&[0u8; TAG_LEN]);
    let aad = packet[..header_len].to_vec();
    let payload_slice = &mut packet[header_len..];
    let _written = keys
        .local_encrypt(packet_number as u64, &aad, payload_slice, plaintext.len())
        .map_err(|_| Error::BadCrypto)?;

    // Header protection: sample 16 bytes at pn_offset + 4.
    apply_hp_with_keys(&mut packet, pn_offset, PN_LEN as usize, keys, true)?;
    Ok(packet)
}

/// Start a Short-header packet: first byte + DCID + a
/// 4-byte packet-number placeholder (patched by
/// [`seal_short_packet`] once the number is allocated).
/// Frames encode DIRECTLY into the returned buffer after
/// `header_len` — v1.9: the separate `plaintext` staging
/// vec cost one alloc + one full payload memmove per
/// packet (~9 KiB at the loopback budget).
fn begin_short_packet(dcid: &ConnectionId, capacity_hint: usize) -> (Vec<u8>, usize) {
    // First byte for Short header: 0|1|spin(0)|0|0|key_phase(0)|pn_len-1
    let first_byte = 0b0100_0000 | (PN_LEN - 1);
    let mut packet: Vec<u8> =
        Vec::with_capacity(1 + dcid.len as usize + PN_LEN as usize + capacity_hint + TAG_LEN);
    packet.push(first_byte);
    packet.extend_from_slice(&dcid.bytes[..dcid.len as usize]);
    packet.extend_from_slice(&[0u8; PN_LEN as usize]);
    let header_len = packet.len();
    (packet, header_len)
}

/// Patch the packet number, AEAD-seal the payload in place
/// (header = AAD, borrowed via `split_at_mut` — no owned
/// AAD copy), and apply header protection.
fn seal_short_packet(
    packet: &mut Vec<u8>,
    header_len: usize,
    packet_number: u32,
    keys: &QuicKeys,
) -> Result<(), Error> {
    let pn_offset = header_len - PN_LEN as usize;
    packet[pn_offset..header_len].copy_from_slice(&packet_number.to_be_bytes());
    let plaintext_len = packet.len() - header_len;
    packet.extend_from_slice(&[0u8; TAG_LEN]);

    let (aad, payload_slice) = packet.split_at_mut(header_len);
    keys.local_encrypt(packet_number as u64, aad, payload_slice, plaintext_len)
        .map_err(|_| Error::BadCrypto)?;

    apply_hp_with_keys(packet, pn_offset, PN_LEN as usize, keys, false)?;
    Ok(())
}

fn apply_hp_with_keys(
    packet: &mut [u8],
    pn_offset: usize,
    pn_len: usize,
    keys: &QuicKeys,
    _is_long: bool,
) -> Result<(), Error> {
    let sample_start = pn_offset + 4;
    let sample_end = sample_start + keys.sample_len();
    if packet.len() < sample_end {
        return Err(Error::BadCrypto);
    }
    // Split the packet into the `first` byte, the PN bytes, and the
    // sample slice — without aliasing.
    let (head, tail) = packet.split_at_mut(pn_offset);
    let (pn_slot, rest) = tail.split_at_mut(pn_len);
    let sample = &rest[4 - pn_len..4 - pn_len + keys.sample_len()];
    let sample_owned = sample.to_vec();
    keys.local_protect_header(&sample_owned, &mut head[0], pn_slot)
        .map_err(|_| Error::BadCrypto)
}

fn remove_hp_with_keys(packet: &mut [u8], pn_offset: usize, keys: &QuicKeys) -> Result<u8, Error> {
    let sample_start = pn_offset + 4;
    let sample_end = sample_start + keys.sample_len();
    if packet.len() < sample_end {
        return Err(Error::BadCrypto);
    }
    let sample: Vec<u8> = packet[sample_start..sample_end].to_vec();
    // We can't know `pn_len` until after the first byte is
    // unmasked. rustls's HP API wants the PN-bytes slice up front
    // and XORs every byte in it. The standard QUIC trick: pass
    // the full 4-byte slot, then restore the bytes past the true
    // `pn_len` (they were XOR-corrupted but they're really part
    // of the protected payload, not the PN).
    let mut pn_slot_orig = [0u8; 4];
    pn_slot_orig.copy_from_slice(&packet[pn_offset..pn_offset + 4]);
    {
        let (head, tail) = packet.split_at_mut(pn_offset);
        let pn_slot = &mut tail[..4];
        keys.remote_unprotect_header(&sample, &mut head[0], pn_slot)
            .map_err(|_| Error::BadCrypto)?;
    }
    let pn_len = (packet[0] & 0x03) + 1;
    // Restore over-XOR'd bytes past the true PN length.
    let start = pn_offset + pn_len as usize;
    let end = pn_offset + 4;
    packet[start..end].copy_from_slice(&pn_slot_orig[pn_len as usize..4]);
    Ok(pn_len)
}

fn parse_long_packet(
    packet: &mut [u8],
    keys: &QuicKeys,
) -> Result<(LongHeader, Vec<Frame>), Error> {
    let pn_offset = locate_long_pn_offset(packet)?;
    let pn_len = remove_hp_with_keys(packet, pn_offset, keys)? as usize;
    let (header, header_len) = decode_header(packet, 0)?;
    debug_assert_eq!(header_len, pn_offset + pn_len);
    let long = match header {
        PacketHeader::Long(h) => h,
        _ => return Err(Error::BadPacket),
    };
    let payload_total = long.payload_len as usize - pn_len;
    let aad = packet[..header_len].to_vec();
    let payload_slice = &mut packet[header_len..header_len + payload_total];
    let plaintext_len = keys
        .remote_decrypt(long.packet_number as u64, &aad, payload_slice)
        .map_err(|_| Error::BadCrypto)?;
    let mut cursor: &[u8] = &payload_slice[..plaintext_len];
    let mut frames = Vec::new();
    while !cursor.is_empty() {
        frames.push(decode_frame(&mut cursor)?);
    }
    Ok((long, frames))
}

/// Remove header protection + decrypt a short-header
/// packet in place. Returns `(header_len, plaintext_len,
/// packet_number)` — the decrypted frame bytes live at
/// `packet[header_len..header_len + plaintext_len]`.
fn decrypt_short_packet(
    packet: &mut [u8],
    our_cid: &ConnectionId,
    keys: &QuicKeys,
) -> Result<(usize, usize, u64), Error> {
    let dcid_len = our_cid.len as usize;
    let pn_offset = 1 + dcid_len;
    let pn_len = remove_hp_with_keys(packet, pn_offset, keys)? as usize;
    let pn_bytes_slice = &packet[pn_offset..pn_offset + pn_len];
    let mut pn_pad = [0u8; 4];
    pn_pad[4 - pn_len..].copy_from_slice(pn_bytes_slice);
    let pn = u32::from_be_bytes(pn_pad);
    let header_len = pn_offset + pn_len;
    let aad = packet[..header_len].to_vec();
    let payload_slice = &mut packet[header_len..];
    let plaintext_len = keys
        .remote_decrypt(pn as u64, &aad, payload_slice)
        .map_err(|_| Error::BadCrypto)?;
    Ok((header_len, plaintext_len, pn as u64))
}

fn locate_long_pn_offset(packet: &[u8]) -> Result<usize, Error> {
    if packet.is_empty() {
        return Err(Error::BadPacket);
    }
    // Long header: first(1) + version(4) + dcid_len(1) + dcid +
    // scid_len(1) + scid + [if Initial: token_len varint + token] +
    // payload_len varint
    let mut idx = 1 + 4;
    if packet.len() <= idx {
        return Err(Error::BadPacket);
    }
    let dcid_len = packet[idx] as usize;
    idx += 1 + dcid_len;
    if packet.len() <= idx {
        return Err(Error::BadPacket);
    }
    let scid_len = packet[idx] as usize;
    idx += 1 + scid_len;
    if packet.len() <= idx {
        return Err(Error::BadPacket);
    }
    // The Long header type bits (4-5 of first byte) tell us if it's Initial.
    let type_bits = (packet[0] >> 4) & 0x03;
    let is_initial = type_bits == 0b00;
    let mut cursor: &[u8] = &packet[idx..];
    if is_initial {
        let token_len = read_varint(&mut cursor)? as usize;
        if cursor.len() < token_len {
            return Err(Error::BadPacket);
        }
        cursor = &cursor[token_len..];
    }
    let _payload_len = read_varint(&mut cursor)?;
    Ok(packet.len() - cursor.len())
}

/// Peek the unprotected long-header fields (version, DCIDs, etc.)
/// without touching the HP-protected first-byte / PN bits.
fn peek_long_header(packet: &[u8]) -> Result<(LongHeader, usize), Error> {
    decode_header(packet, 0).and_then(|(h, len)| match h {
        PacketHeader::Long(l) => Ok((l, len)),
        _ => Err(Error::BadPacket),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_stream_on_missing_stream_returns_zero() {
        // Build a stub Connection via the test helper would
        // require a TLS config; we directly poke the data path
        // through the public surface by exercising it during the
        // loopback test. Here we just smoke-test the helper fns.
        let cid = cid_from_bytes(&[1, 2, 3]).unwrap();
        assert_eq!(cid.len, 3);
        assert!(is_pseudo_stream(pseudo_stream_id_initial_crypto()));
        assert!(is_pseudo_stream(pseudo_stream_id_handshake_crypto()));
        assert!(!is_pseudo_stream(0));
        // hash_bytes is deterministic.
        let h1 = hash_bytes(b"abc", 0);
        let h2 = hash_bytes(b"abc", 0);
        assert_eq!(h1, h2);
    }

    #[test]
    fn build_initial_with_offset_rejects_nonzero_offset() {
        let cid = cid_from_bytes(&[0xaa; 8]).unwrap();
        let err = build_initial_with_offset(
            &cid,
            &cid,
            &[],
            0,
            b"hi",
            1, /* nonzero */
            Side::Client,
            &cid.bytes[..cid.len as usize],
            0,
        );
        assert_eq!(err, Err(Error::BadFrame));
    }

    #[test]
    fn locate_long_pn_offset_rejects_short_packets() {
        assert_eq!(locate_long_pn_offset(&[]), Err(Error::BadPacket));
        // first(1) + version(4) only — no dcid_len byte
        assert_eq!(
            locate_long_pn_offset(&[0xc0, 0, 0, 0, 0x01]),
            Err(Error::BadPacket)
        );
        // dcid_len = 5 declared, no bytes for dcid + scid_len follow
        assert_eq!(
            locate_long_pn_offset(&[0xc0, 0, 0, 0, 0x01, 0x05]),
            Err(Error::BadPacket)
        );
        // dcid_len = 0; truncated before scid_len byte
        assert_eq!(
            locate_long_pn_offset(&[0xc0, 0, 0, 0, 0x01, 0x00]),
            Err(Error::BadPacket)
        );
        // dcid_len = 0; scid_len = 0; but Initial expects a token-len varint next
        // — buf is now empty, so read_varint of token_len fails.
        assert_eq!(
            locate_long_pn_offset(&[0xc0, 0, 0, 0, 0x01, 0x00, 0x00]),
            Err(Error::BadPacket)
        );
        // Initial header with token_len declaring more bytes than exist
        assert_eq!(
            locate_long_pn_offset(&[0xc0, 0, 0, 0, 0x01, 0x00, 0x00, 0x05]),
            Err(Error::BadPacket)
        );
    }

    #[test]
    fn peek_long_header_rejects_short_form() {
        // Short-header first byte (high bit 0).
        let short_pkt = [0x40, 0x00, 0x00, 0x00, 0x01];
        assert_eq!(peek_long_header(&short_pkt), Err(Error::BadPacket));
    }
}

#[cfg(test)]
mod reliability_tests {
    //! Unit KATs for the phase-1 reliability primitives:
    //! span merging and RFC 9000 §19.3.1 ACK-range
    //! encoding (the decode side lives in `process_ack`,
    //! covered end-to-end by `integ_loss_recovery`).

    use super::*;
    use alloc::vec;

    #[test]
    fn merge_span_merges_and_sorts() {
        let mut r: Vec<(u64, u64)> = Vec::new();
        merge_span(&mut r, 5, 8);
        merge_span(&mut r, 0, 2);
        assert_eq!(r, vec![(0, 2), (5, 8)]);
        // Adjacent (half-open touch) merges.
        merge_span(&mut r, 2, 4);
        assert_eq!(r, vec![(0, 4), (5, 8)]);
        // Bridging span collapses everything.
        merge_span(&mut r, 3, 6);
        assert_eq!(r, vec![(0, 8)]);
        // Contained span is a no-op.
        merge_span(&mut r, 1, 7);
        assert_eq!(r, vec![(0, 8)]);
    }

    #[test]
    fn ack_frame_encoding_matches_rfc_ranges() {
        // PNs 0-2 and 5-7 received → largest 7, first
        // range 2, one (gap, range) pair: gap = 5-3-1 = 1,
        // range = 2-0 = 2.
        let frame = ack_frame_from_ranges(&[(0, 3), (5, 8)]);
        let Frame::Ack {
            largest_acked,
            first_ack_range,
            ranges,
            ..
        } = frame
        else {
            panic!("not an ACK");
        };
        assert_eq!(largest_acked, 7);
        assert_eq!(first_ack_range, 2);
        assert_eq!(ranges, vec![(1, 2)]);
        // RFC decode walk: smallest = 7-2 = 5;
        // hi = 5 - gap(1) - 2 = 2; lo = 2 - range(2) = 0.
        // → acks {0..=2, 5..=7}. Round trip is exact.
    }

    #[test]
    fn ack_frame_single_range() {
        let frame = ack_frame_from_ranges(&[(10, 14)]);
        let Frame::Ack {
            largest_acked,
            first_ack_range,
            ranges,
            ..
        } = frame
        else {
            panic!("not an ACK");
        };
        assert_eq!(largest_acked, 13);
        assert_eq!(first_ack_range, 3);
        assert!(ranges.is_empty());
    }

    #[test]
    fn icbrt_exact_and_property() {
        assert_eq!(icbrt(0), 0);
        assert_eq!(icbrt(1), 1);
        assert_eq!(icbrt(7), 1);
        assert_eq!(icbrt(8), 2);
        assert_eq!(icbrt(26), 2);
        assert_eq!(icbrt(27), 3);
        assert_eq!(icbrt(u64::MAX), 2_642_245);
        // Property sweep: r³ ≤ n < (r+1)³.
        for n in [
            100u64,
            999,
            1_000_000,
            750_000_000,
            75_000_000_000,
            u64::MAX / 2,
        ] {
            let r = icbrt(n);
            assert!((r as u128).pow(3) <= n as u128);
            assert!(((r + 1) as u128).pow(3) > n as u128);
        }
    }

    #[test]
    fn cubic_curve_kats() {
        // C = 0.4 seg/s³ at budget 1350: one second from K
        // moves the window by 4·10⁹·1350/10¹⁰ = 540 bytes.
        let w_max = 100_000usize;
        assert_eq!(cubic_window(w_max, 0, 1350), 100_000);
        assert_eq!(cubic_window(w_max, 1_000, 1350), 100_540);
        assert_eq!(cubic_window(w_max, -1_000, 1350), 99_460);
        assert_eq!(cubic_window(w_max, 2_000, 1350), 104_320);
        // K for a 100-segment plateau: ∛(75·10⁹) ms ≈ 4.2 s
        // — at loopback RTTs the TCP-friendly region
        // governs, exactly as designed.
        assert_eq!(cubic_k_ms(100 * 1350, 1350), 4_217);
        // cwnd at the plateau (d = 0 ⇒ w_max regained).
        assert_eq!(cubic_window(w_max, 4_217 - 4_217, 1350), w_max);
    }

    #[test]
    fn crypto_accept_trims_parks_and_pulls() {
        let mut off = 0u64;
        let mut oob: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let mut out = Vec::new();
        // In-order delivery.
        crypto_accept(&mut off, &mut oob, 0, b"abc", &mut out);
        assert_eq!(out, b"abc");
        assert_eq!(off, 3);
        // Full duplicate is a no-op.
        out.clear();
        crypto_accept(&mut off, &mut oob, 0, b"abc", &mut out);
        assert!(out.is_empty());
        assert_eq!(off, 3);
        // Out-of-order segment parks…
        crypto_accept(&mut off, &mut oob, 6, b"ghi", &mut out);
        assert!(out.is_empty());
        assert_eq!(oob.len(), 1);
        // …and an overlapping retransmit (full flight from
        // 0) trims the known prefix and pulls the parked
        // segment through.
        crypto_accept(&mut off, &mut oob, 0, b"abcdefghi", &mut out);
        assert_eq!(out, b"defghi");
        assert_eq!(off, 9);
        assert!(oob.is_empty());
    }

    #[test]
    fn stream_ack_span_releases_buffer_only_when_complete() {
        let mut st = StreamState::default();
        st.pending_send.extend_from_slice(&[0u8; 100]);
        st.send_cursor = 100; // everything sent
        // Out-of-order ack: [50, 100) first — prefix stays.
        st.ack_span(50, 100);
        assert_eq!(st.acked_cursor, 0);
        assert!(!st.pending_send.is_empty());
        // Hole closes → buffer released wholesale.
        st.ack_span(0, 50);
        assert!(st.pending_send.is_empty());
        assert_eq!(st.base_offset, 100);
        assert_eq!(st.acked_cursor, 0);
        assert_eq!(st.send_cursor, 0);
    }
}
