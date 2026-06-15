//! H/3 Connection state machine — RFC 9114 §3-§6.
//!
//! Owns one [`grpcx_quic::QuicConnection`] and:
//! - opens the H/3 control + QPACK encoder + QPACK decoder
//!   uni-streams as soon as 1-RTT is reached,
//! - exchanges SETTINGS frames on the control streams,
//! - opens / accepts bidirectional request streams,
//! - frames request + response HEADERS via QPACK,
//! - frames body chunks via DATA frames,
//! - emits GOAWAY on graceful shutdown (RFC 9114 §5.2).
//!
//! Out of scope for v0.5 (defers per `docs/v5-scope.md`):
//! - QPACK dynamic table — encoder / decoder instruction streams
//!   are opened (one each per direction) and their payload is
//!   drained-and-ignored. v0.6 wires real instructions.
//! - Server push — push streams are discovered + classified but
//!   their payload is silently dropped.
//! - Per-stream flow control / priority — RFC 9218 PRIORITY_UPDATE
//!   parses but doesn't influence scheduling.
//!
//! Trailers (since v1.3, the L10 layer-up patch): the second
//! HEADERS frame on a request stream is stored as the
//! trailing section and surfaced via `read_trailers` /
//! `received_trailers_ready`; sending trailers is a second
//! `write_headers` call before `finish`. A third HEADERS
//! frame is dropped.

use crate::qpack_dynamic::{
    DecoderInstruction, DynamicTable, EncoderInstruction, decode_encoder_instruction,
    encode_decoder_instruction,
};
use crate::varint::{read_varint, varint_len, write_varint};
use crate::{
    Error, Frame, Header, SettingPair, Settings, StreamType, decode_frame, encode_field_section,
    encode_frame, read_stream_type, static_entry, write_stream_type,
};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use grpcx_quic::{QuicConnection, StreamId};

// ============================================================
// SETTINGS identifier registry (RFC 9114 §7.2.4 + RFC 9204 §5 +
// RFC 9220).
// ============================================================

const SETTINGS_QPACK_MAX_TABLE_CAPACITY: u64 = 0x01;
const SETTINGS_MAX_FIELD_SECTION_SIZE: u64 = 0x06;
const SETTINGS_QPACK_BLOCKED_STREAMS: u64 = 0x07;
const SETTINGS_ENABLE_CONNECT_PROTOCOL: u64 = 0x08;

// ============================================================
// Connection.
// ============================================================

/// One end of an HTTP/3 session — client or server. Owns a
/// [`grpcx_quic::QuicConnection`] and drives RFC 9114 framing +
/// QPACK over its streams.
///
/// The `pump`-shaped API mirrors L4's [`grpcx_quic::QuicConnection`]:
/// the caller feeds inbound bytes and pulls outbound bytes,
/// keeping the surface runtime-agnostic.
pub struct Connection {
    quic: QuicConnection,
    side: Side,
    settings: Settings,
    peer_settings: Option<Settings>,

    // Local uni streams (lazily opened the first pump after
    // 1-RTT is reached).
    local_control_sid: Option<u64>,
    local_qpack_encoder_sid: Option<u64>,
    local_qpack_decoder_sid: Option<u64>,
    local_uni_init_done: bool,

    // Peer uni streams (lazily classified as accept_uni hands
    // them over + the type byte arrives).
    peer_control_sid: Option<u64>,
    peer_qpack_encoder_sid: Option<u64>,
    peer_qpack_decoder_sid: Option<u64>,

    // Uni streams in flight whose type byte hasn't fully arrived
    // yet. Keyed by sid; value is the buffer we accumulated.
    pending_uni_classify: BTreeMap<u64, Vec<u8>>,

    // Peer-uni streams classified as `Push` that we silently
    // drain to keep flow-control happy (v0.5 doesn't accept
    // pushes; v1.x revisits).
    drained_uni_sids: Vec<u64>,

    // Reserved-typed peer uni streams pending pickup by an upper
    // layer (e.g. WebTransport on L6, RFC 9220). Each entry is
    // `(sid, raw_type_code)`. L5 1.1.0+ addition: callers drain
    // via `accept_unknown_uni_stream`; not auto-drained.
    unknown_uni_pending: Vec<(u64, u64)>,
    // Post-prefix bytes captured at classify time for Reserved
    // uni streams — `read_stream_raw` returns these before
    // falling through to fresh L4 bytes.
    claimed_uni_buf: BTreeMap<u64, Vec<u8>>,
    // Bidi sids claimed via `accept_unknown_bi_stream`. Skipped
    // by `accept_request` and `drain_request_streams` so the
    // upper layer owns I/O on them via `read_stream_raw` /
    // `write_stream_raw`.
    wt_owned_sids: BTreeSet<u64>,
    // Bytes `drain_request_streams` already buffered for a bidi
    // BEFORE the upper layer claimed it (transferred out of the
    // discarded `RequestState`). `read_stream_raw` drains this
    // before falling through to L4.
    claimed_bi_buf: BTreeMap<u64, Vec<u8>>,

    // Inbound control-stream byte buffer (post-type-byte; the
    // frame decoder walks it).
    control_in_buf: Vec<u8>,

    // QPACK dynamic-table machinery (v0.7).
    /// Decoder's reconstruction of the peer encoder's dynamic
    /// table — kept in lock-step by parsing inbound encoder
    /// instructions on the peer's encoder uni-stream.
    peer_dynamic_table: DynamicTable,
    /// Buffered bytes from the peer encoder uni-stream that
    /// haven't yet parsed into a full instruction.
    encoder_in_buf: Vec<u8>,
    /// Insert Count Increment we owe the peer the next time we
    /// flush our local decoder stream. Accumulates whenever
    /// `peer_dynamic_table.insert_count()` advances.
    pending_insert_count_increment: u64,
    /// Pending Section-Acknowledgement stream ids to send on
    /// our local decoder stream.
    pending_section_acks: Vec<u64>,

    // Per-bidi-request state.
    requests: BTreeMap<u64, RequestState>,
    /// Retired request buffers (`in_buf` / `body`) kept for
    /// reuse — a fresh `RequestState` otherwise re-grows
    /// both from zero capacity on every RPC, and the
    /// doubling copies showed hot on the 64 KiB profile.
    /// Bounded so an occasional giant body doesn't pin
    /// memory forever.
    spare_request_bufs: Vec<Vec<u8>>,
    /// FIFO of incoming bidi sids that the caller hasn't yet
    /// drained via [`accept_request`].
    ///
    /// [`accept_request`]: Connection::accept_request
    incoming_requests: Vec<u64>,

    // Outbound GOAWAY (highest accepted stream id).
    goaway_sent: bool,
    /// Highest peer-initiated request stream id we've seen
    /// (server-side) or `0` if none yet.
    highest_accepted_request_id: u64,
    /// GOAWAY id received from the peer (None if not yet).
    peer_goaway: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Client,
    Server,
}

#[derive(Default)]
struct RequestState {
    in_buf: Vec<u8>,
    body: Vec<u8>,
    body_fin: bool,
    received_headers: Option<Vec<Header>>,
    headers_dispatched: bool,
    /// Trailing HEADERS section (the second HEADERS frame
    /// on the stream — RFC 9114 §4.1 allows
    /// `HEADERS DATA* HEADERS`). gRPC carries
    /// `grpc-status` here. v1.3 layer-up patch for L10.
    received_trailers: Option<Vec<Header>>,
    trailers_dispatched: bool,
}

impl Connection {
    /// Open a client-side H/3 session over an already-established
    /// (or about-to-establish) QUIC connection. The pump cycle
    /// drives the QUIC handshake to completion, then opens the
    /// H/3 control + QPACK streams and emits SETTINGS.
    pub fn new_client(quic: QuicConnection, settings: Settings) -> Result<Self, Error> {
        Ok(Self::new(quic, Side::Client, settings))
    }

    /// Server-side counterpart of [`new_client`].
    ///
    /// [`new_client`]: Connection::new_client
    pub fn new_server(quic: QuicConnection, settings: Settings) -> Result<Self, Error> {
        Ok(Self::new(quic, Side::Server, settings))
    }

    fn new(quic: QuicConnection, side: Side, settings: Settings) -> Self {
        Self {
            quic,
            side,
            settings,
            peer_settings: None,
            local_control_sid: None,
            local_qpack_encoder_sid: None,
            local_qpack_decoder_sid: None,
            local_uni_init_done: false,
            peer_control_sid: None,
            peer_qpack_encoder_sid: None,
            peer_qpack_decoder_sid: None,
            pending_uni_classify: BTreeMap::new(),
            drained_uni_sids: Vec::new(),
            unknown_uni_pending: Vec::new(),
            claimed_uni_buf: BTreeMap::new(),
            wt_owned_sids: BTreeSet::new(),
            claimed_bi_buf: BTreeMap::new(),
            control_in_buf: Vec::new(),
            peer_dynamic_table: DynamicTable::new(0),
            encoder_in_buf: Vec::new(),
            pending_insert_count_increment: 0,
            pending_section_acks: Vec::new(),
            requests: BTreeMap::new(),
            spare_request_bufs: Vec::new(),
            incoming_requests: Vec::new(),
            goaway_sent: false,
            highest_accepted_request_id: 0,
            peer_goaway: None,
        }
    }

    /// `true` once both directions have exchanged SETTINGS and
    /// the H/3 session is ready for requests. The pump cycle
    /// itself drives the QUIC handshake first; `is_ready` only
    /// returns `true` after that, too.
    pub fn is_ready(&self) -> bool {
        self.local_uni_init_done && self.peer_settings.is_some()
    }

    /// SETTINGS the peer sent us (`None` until both control
    /// streams have caught up).
    pub fn peer_settings(&self) -> Option<&Settings> {
        self.peer_settings.as_ref()
    }

    /// `true` once we've seen 1-RTT keys (the underlying QUIC
    /// handshake is done). Equivalent to the L4 flag.
    pub fn is_quic_established(&self) -> bool {
        self.quic.is_established()
    }

    /// ALPN passed through from L3 / L4.
    pub fn alpn_protocol(&self) -> Option<Vec<u8>> {
        self.quic.alpn_protocol()
    }

    /// Drive one round of inbound + outbound traffic. Mirrors
    /// [`grpcx_quic::QuicConnection::pump`] one layer up. The
    /// returned vec is the set of QUIC datagrams that need to
    /// reach the peer.
    pub fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, Error> {
        self.pump_at(incoming, 0)
    }

    /// [`pump`](Self::pump) with a caller-supplied monotonic
    /// clock reading (µs), threaded through to the L4 timer
    /// surface ([`grpcx_quic::QuicConnection::pump_at`]) —
    /// RTT estimation, PTO/tail-loss probes, and handshake
    /// retransmit only run on timed pumps. 1.6.0 addition.
    pub fn pump_at(&mut self, incoming: Option<&[u8]>, now_us: u64) -> Result<Vec<Vec<u8>>, Error> {
        // 1. Drive the underlying QUIC machinery. Pure-ACK
        //    emission is deferred to the flush pump in step
        //    8 — and only when this whole pump is idle
        //    (otherwise every incoming datagram's flush
        //    would spend a packet on a bare ACK).
        let mut out = self.quic.pump_at(incoming, false, now_us)?;

        // 2. On the first pump after 1-RTT, open our uni streams
        //    + send SETTINGS.
        if self.quic.is_established() && !self.local_uni_init_done {
            self.init_local_uni_streams()?;
        }

        // 3. Drain newly-arrived peer streams.
        while let Some(sid) = self.quic.accept_uni() {
            self.pending_uni_classify.insert(sid.0, Vec::new());
        }
        // 3b. Eager cancel (1.8): streams whose life ended
        //     via RESET_STREAM / STOP_SENDING — drop the
        //     request state; a cancelled-but-not-yet-
        //     accepted request never reaches dispatch.
        for sid in self.quic.take_reset_streams() {
            self.requests.remove(&sid);
            self.incoming_requests.retain(|&s| s != sid);
        }
        while let Some(sid) = self.quic.accept_bi() {
            // RFC 9114 §6.1: bidi streams are unconditionally
            // request streams. 1.1.0+: an upper layer (e.g. L6
            // WebTransport, RFC 9220) may claim the bidi out of
            // the H/3 request path via
            // `accept_unknown_bi_stream`. To give the caller
            // that chance, defer creating the `RequestState` (+
            // its H/3-frame draining) until `accept_request`
            // actually consumes the sid — bytes accumulate in
            // the underlying L4 stream buffer in the meantime
            // and `read_stream_raw` (for WT) drains them.
            self.incoming_requests.push(sid.0);
            if sid.0 > self.highest_accepted_request_id {
                self.highest_accepted_request_id = sid.0;
            }
        }

        // 4. Classify pending uni streams.
        self.classify_pending_uni()?;

        // 5. Drain the peer control stream + decode frames.
        self.drain_peer_control()?;

        // 6. Drain the peer QPACK encoder stream — parse incoming
        //    instructions, update `peer_dynamic_table`, accumulate
        //    `pending_insert_count_increment` for each successful
        //    insert.
        self.drain_peer_encoder()?;
        // The peer decoder stream is parsed too but v0.7 doesn't
        // act on Section-Ack / Stream-Cancel / Insert-Count-Increment
        // yet (encoder-side strategy is v1.x). We still drain it to
        // keep QUIC flow-control happy.
        if let Some(sid) = self.peer_qpack_decoder_sid {
            let mut tmp = Vec::new();
            self.quic.read_stream(StreamId(sid), &mut tmp);
        }
        // Flush pending decoder-stream messages (Insert-Count-
        // Increment + Section-Ack) on our local decoder stream.
        self.flush_decoder_instructions()?;
        // Also drain reserved/push uni streams.
        for sid in &self.drained_uni_sids.clone() {
            let mut tmp = Vec::new();
            self.quic.read_stream(StreamId(*sid), &mut tmp);
        }

        // 7. Drain request bidis + decode H/3 frames.
        self.drain_request_streams()?;

        // 8. Drive QUIC once more to flush anything we wrote this
        //    cycle (pure ACKs only on a truly idle pump — see
        //    step 1).
        out.extend(self.quic.pump_at(None, incoming.is_none(), now_us)?);
        Ok(out)
    }

    fn init_local_uni_streams(&mut self) -> Result<(), Error> {
        let control = self.quic.open_uni();
        let qpack_enc = self.quic.open_uni();
        let qpack_dec = self.quic.open_uni();
        self.local_control_sid = Some(control.0);
        self.local_qpack_encoder_sid = Some(qpack_enc.0);
        self.local_qpack_decoder_sid = Some(qpack_dec.0);

        // Write the type byte on each.
        for (sid, t) in [
            (control, StreamType::Control),
            (qpack_enc, StreamType::QpackEncoder),
            (qpack_dec, StreamType::QpackDecoder),
        ] {
            let mut buf = Vec::new();
            write_stream_type(t, &mut buf)?;
            self.quic.write_stream(sid, &buf, false)?;
        }

        // Send our SETTINGS frame on the control stream.
        let settings_payload = serialize_settings(&self.settings);
        let mut frame_buf = Vec::new();
        encode_frame(&Frame::Settings(settings_payload), &mut frame_buf)?;
        self.quic.write_stream(control, &frame_buf, false)?;

        self.local_uni_init_done = true;
        Ok(())
    }

    fn classify_pending_uni(&mut self) -> Result<(), Error> {
        let sids: Vec<u64> = self.pending_uni_classify.keys().copied().collect();
        for sid in sids {
            let mut new_bytes = Vec::new();
            self.quic.read_stream(StreamId(sid), &mut new_bytes);
            if new_bytes.is_empty() {
                continue;
            }
            let buf = self.pending_uni_classify.get_mut(&sid).unwrap();
            buf.extend_from_slice(&new_bytes);
            let buf_clone = buf.clone();
            if let Ok((stype, consumed)) = read_stream_type(&buf_clone) {
                let rest: Vec<u8> = buf_clone[consumed..].to_vec();
                self.pending_uni_classify.remove(&sid);
                match stype {
                    StreamType::Control => {
                        if self.peer_control_sid.is_some() {
                            // RFC 9114 §6.2.1: second control stream
                            // is `H3_STREAM_CREATION_ERROR`. v0.5
                            // surfaces this as a generic protocol
                            // error.
                            return Err(Error::Internal);
                        }
                        self.peer_control_sid = Some(sid);
                        self.control_in_buf.extend_from_slice(&rest);
                    }
                    StreamType::QpackEncoder => {
                        if self.peer_qpack_encoder_sid.is_some() {
                            return Err(Error::Internal);
                        }
                        self.peer_qpack_encoder_sid = Some(sid);
                        // Drop the body; v0.5 doesn't run dynamic
                        // table instructions.
                    }
                    StreamType::QpackDecoder => {
                        if self.peer_qpack_decoder_sid.is_some() {
                            return Err(Error::Internal);
                        }
                        self.peer_qpack_decoder_sid = Some(sid);
                    }
                    StreamType::Push => {
                        // RFC 9114 §6.2.3: push streams without an
                        // accepting handler are silently drained
                        // by pump to keep flow-control happy. v0.5
                        // doesn't accept pushes; v1.x revisits.
                        self.drained_uni_sids.push(sid);
                    }
                    StreamType::Reserved(raw) => {
                        // RFC 9114 §11.2.2 + RFC 9220
                        // (WebTransport): hand reserved-typed
                        // streams to the upper layer via
                        // `accept_unknown_uni_stream`. Capture any
                        // post-prefix bytes that came in the same
                        // QUIC frame as the type byte so
                        // `read_stream_raw` can return them.
                        self.unknown_uni_pending.push((sid, raw));
                        if !rest.is_empty() {
                            self.claimed_uni_buf.insert(sid, rest);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn drain_peer_control(&mut self) -> Result<(), Error> {
        let Some(sid) = self.peer_control_sid else {
            return Ok(());
        };
        let mut new_bytes = Vec::new();
        self.quic.read_stream(StreamId(sid), &mut new_bytes);
        self.control_in_buf.extend_from_slice(&new_bytes);

        loop {
            let mut cursor: &[u8] = &self.control_in_buf;
            let before_len = cursor.len();
            match decode_frame(&mut cursor) {
                Ok(frame) => {
                    let consumed = before_len - cursor.len();
                    self.control_in_buf.drain(..consumed);
                    self.handle_control_frame(frame)?;
                }
                // Truncated frame — wait for more bytes.
                Err(_) => break,
            }
        }
        Ok(())
    }

    fn handle_control_frame(&mut self, frame: Frame) -> Result<(), Error> {
        match frame {
            Frame::Settings(pairs) => {
                self.peer_settings = Some(parse_settings(&pairs));
            }
            Frame::Goaway { stream_or_push_id } => {
                self.peer_goaway = Some(stream_or_push_id);
            }
            Frame::MaxPushId { .. }
            | Frame::CancelPush { .. }
            | Frame::PriorityUpdate { .. }
            | Frame::Origin { .. }
            | Frame::Reserved { .. } => {
                // v0.5 accepts these silently; richer handling lands
                // in later checkpoints alongside the matching feature.
            }
            // RFC 9114 §7.2: DATA / HEADERS / PUSH_PROMISE on the
            // control stream are `H3_FRAME_UNEXPECTED`. v0.5
            // surfaces as a generic protocol error.
            Frame::Data { .. } | Frame::Headers { .. } | Frame::PushPromise { .. } => {
                return Err(Error::Internal);
            }
        }
        Ok(())
    }

    fn drain_peer_encoder(&mut self) -> Result<(), Error> {
        let Some(sid) = self.peer_qpack_encoder_sid else {
            return Ok(());
        };
        let mut new_bytes = Vec::new();
        self.quic.read_stream(StreamId(sid), &mut new_bytes);
        self.encoder_in_buf.extend_from_slice(&new_bytes);

        loop {
            let mut cursor: &[u8] = &self.encoder_in_buf;
            let before_len = cursor.len();
            match decode_encoder_instruction(&mut cursor) {
                Ok(inst) => {
                    let consumed = before_len - cursor.len();
                    self.encoder_in_buf.drain(..consumed);
                    self.apply_encoder_instruction(inst)?;
                }
                // Truncated — wait for more bytes.
                Err(_) => break,
            }
        }
        Ok(())
    }

    fn apply_encoder_instruction(&mut self, inst: EncoderInstruction) -> Result<(), Error> {
        match inst {
            EncoderInstruction::SetDynamicTableCapacity(cap) => {
                self.peer_dynamic_table.set_capacity(cap);
            }
            EncoderInstruction::InsertWithNameReference {
                table_is_static,
                name_index,
                value,
            } => {
                let name = if table_is_static {
                    let (n, _) = static_entry(name_index).ok_or(Error::QpackDecode)?;
                    n.to_vec()
                } else {
                    // Relative index into the dynamic table as it
                    // stood when the encoder sent this instruction
                    // — which is the table we're maintaining
                    // (newest = 0).
                    self.peer_dynamic_table
                        .get_relative(name_index)
                        .ok_or(Error::QpackDecode)?
                        .name
                        .clone()
                };
                self.peer_dynamic_table.insert(name, value)?;
                self.pending_insert_count_increment += 1;
            }
            EncoderInstruction::InsertWithLiteralName { name, value } => {
                self.peer_dynamic_table.insert(name, value)?;
                self.pending_insert_count_increment += 1;
            }
            EncoderInstruction::Duplicate(rel) => {
                self.peer_dynamic_table.duplicate(rel)?;
                self.pending_insert_count_increment += 1;
            }
        }
        Ok(())
    }

    fn flush_decoder_instructions(&mut self) -> Result<(), Error> {
        let Some(sid) = self.local_qpack_decoder_sid else {
            return Ok(());
        };
        let mut out: Vec<u8> = Vec::new();
        if self.pending_insert_count_increment > 0 {
            encode_decoder_instruction(
                &DecoderInstruction::InsertCountIncrement {
                    increment: self.pending_insert_count_increment,
                },
                &mut out,
            )?;
            self.pending_insert_count_increment = 0;
        }
        for stream_id in self.pending_section_acks.drain(..) {
            encode_decoder_instruction(
                &DecoderInstruction::SectionAcknowledgement { stream_id },
                &mut out,
            )?;
        }
        if !out.is_empty() {
            self.quic.write_stream(StreamId(sid), &out, false)?;
        }
        Ok(())
    }

    fn drain_request_streams(&mut self) -> Result<(), Error> {
        let sids: Vec<u64> = self.requests.keys().copied().collect();
        for sid in sids {
            // Bidi claimed by an upper layer via
            // `accept_unknown_bi_stream` — L5 leaves I/O on it
            // alone so the upper layer's raw read path sees the
            // bytes unmolested.
            if self.wt_owned_sids.contains(&sid) {
                continue;
            }
            self.drain_one_request(sid)?;
        }
        Ok(())
    }

    /// Drain L4 bytes for one already-registered bidi `sid` into
    /// its `RequestState`, then run the H/3 frame decoder over
    /// what's accumulated. Used both by `pump`'s per-cycle
    /// `drain_request_streams` and by `accept_request` (which
    /// drains synchronously on pop so a just-accepted request's
    /// HEADERS are immediately available via `read_headers`).
    fn drain_one_request(&mut self, sid: u64) -> Result<(), Error> {
        {
            // Read straight into the request's reassembly
            // buffer — the old bounce through a temporary
            // Vec doubled the copy on every bulk drain.
            let st = self.requests.get_mut(&sid).expect("registered sid");
            let (_, fin) = self.quic.read_stream(StreamId(sid), &mut st.in_buf);
            if fin {
                st.body_fin = true;
            }
        }

        // Deferred-drain cursor: the per-frame
        // `in_buf.drain(..)` memmoved the whole remaining
        // tail once per frame (hot on the 64 KiB profile —
        // several DATA frames per burst); track consumed
        // bytes and drain ONCE on exit instead.
        let mut consumed: usize = 0;
        loop {
            // DATA fast path: stream the payload straight from
            // `in_buf` into `body` — `decode_frame` would copy
            // it into an owned `Frame` and the match arm would
            // copy it again (the double copy showed up hot on
            // the 64 KiB profile).
            {
                let st = self.requests.get_mut(&sid).expect("registered sid");
                let mut cursor: &[u8] = &st.in_buf[consumed..];
                let before_len = cursor.len();
                if let Ok(ty) = crate::varint::read_varint(&mut cursor)
                    && ty == crate::frame::TYPE_DATA
                {
                    let Ok(len) = crate::varint::read_varint(&mut cursor) else {
                        break; // header incomplete — wait for more bytes
                    };
                    let hdr = before_len - cursor.len();
                    let len = len as usize;
                    if cursor.len() < len {
                        break; // payload incomplete
                    }
                    let start = consumed + hdr;
                    // (split borrow: body and in_buf are
                    // disjoint fields of the same state)
                    let (body, in_buf) = (&mut st.body, &st.in_buf);
                    body.extend_from_slice(&in_buf[start..start + len]);
                    consumed += hdr + len;
                    continue;
                }
            }
            // Pull one frame out of `in_buf` in a tight scope so
            // the per-frame match below can re-borrow `self`.
            let frame = {
                let st = self.requests.get_mut(&sid).expect("registered sid");
                let mut cursor: &[u8] = &st.in_buf[consumed..];
                let before_len = cursor.len();
                match decode_frame(&mut cursor) {
                    Ok(f) => {
                        consumed += before_len - cursor.len();
                        f
                    }
                    Err(_) => break,
                }
            };
            match frame {
                Frame::Headers {
                    encoded_field_section,
                } => {
                    // First HEADERS = request / response header
                    // section; second HEADERS = trailing
                    // section (RFC 9114 §4.1 `HEADERS DATA*
                    // HEADERS` — gRPC's `grpc-status` rides
                    // there). Decoding goes against
                    // `peer_dynamic_table`; if it's empty the
                    // path behaves identically to static-only.
                    // A third HEADERS frame is a peer protocol
                    // violation and is dropped.
                    let slot = self.requests.get(&sid).map(|st| {
                        (
                            st.received_headers.is_some(),
                            st.received_trailers.is_some(),
                        )
                    });
                    let slot = match slot {
                        Some((false, _)) => 0,    // leading section
                        Some((true, false)) => 1, // trailing section
                        _ => 2,                   // excess — drop
                    };
                    if slot < 2 {
                        let hdrs = crate::qpack::decode_field_section_with_table(
                            &encoded_field_section,
                            &self.peer_dynamic_table,
                        )?;
                        let st = self.requests.get_mut(&sid).expect("registered sid");
                        if slot == 0 {
                            st.received_headers = Some(hdrs);
                        } else {
                            st.received_trailers = Some(hdrs);
                        }
                        // Queue Section-Ack on the local decoder
                        // stream — the next pump cycle flushes
                        // it.
                        self.pending_section_acks.push(sid);
                    }
                }
                Frame::Data { payload } => {
                    let st = self.requests.get_mut(&sid).expect("registered sid");
                    st.body.extend_from_slice(&payload);
                }
                // Forward-compat: unknown / reserved frame types
                // on request streams are reserved-skip per RFC
                // 9114 §7.2.8.
                _ => {}
            }
        }
        if consumed > 0 {
            let st = self.requests.get_mut(&sid).expect("registered sid");
            st.in_buf.drain(..consumed);
        }
        Ok(())
    }

    // ============================================================
    // Request-stream API.
    // ============================================================

    /// Eager cancel (1.8): abandon the request — QUIC
    /// RESET_STREAM + STOP_SENDING with H3_REQUEST_CANCELLED
    /// (RFC 9114 §8.1, 0x10c) and local state dropped. The
    /// connection survives; best-effort (frames flush on
    /// the next pump).
    pub fn cancel_request(&mut self, rs: &RequestStream) {
        self.quic.cancel_stream(rs.sid, 0x10c);
        self.requests.remove(&rs.sid.0);
        self.incoming_requests.retain(|&s| s != rs.sid.0);
    }

    /// Open a new client-initiated bidi request stream and write
    /// the request's HEADERS frame. Client-only.
    pub fn send_request(&mut self, headers: &[Header]) -> Result<RequestStream, Error> {
        if !matches!(self.side, Side::Client) {
            return Err(Error::Internal);
        }
        let sid = self.quic.open_bi();
        let st = self.fresh_request_state();
        self.requests.insert(sid.0, st);
        self.send_headers(sid.0, headers)?;
        Ok(RequestStream { sid })
    }

    /// Pop the next server-side request stream the peer has
    /// opened. Server-only. Returns `Ok(None)` when nothing is
    /// pending. Skips bidis claimed by an upper layer via
    /// [`accept_unknown_bi_stream`].
    ///
    /// Creates the per-stream `RequestState` on pop and eagerly
    /// drains + decodes any bytes already buffered in L4 so a
    /// subsequent [`read_headers`] returns `Some(...)` straight
    /// away when the HEADERS frame is already on the wire (matches
    /// pre-1.1.0 caller expectations).
    ///
    /// [`accept_unknown_bi_stream`]: Connection::accept_unknown_bi_stream
    /// [`read_headers`]: Connection::read_headers
    pub fn accept_request(&mut self) -> Result<Option<RequestStream>, Error> {
        if !matches!(self.side, Side::Server) {
            return Err(Error::Internal);
        }
        while let Some(&sid) = self.incoming_requests.first() {
            if self.wt_owned_sids.contains(&sid) {
                self.incoming_requests.remove(0);
                continue;
            }
            self.incoming_requests.remove(0);
            if !self.requests.contains_key(&sid) {
                let st = self.fresh_request_state();
                self.requests.insert(sid, st);
            }
            // Eagerly drain so HEADERS that the most recent pump
            // already brought across L4 decode synchronously.
            self.drain_one_request(sid)?;
            return Ok(Some(RequestStream { sid: StreamId(sid) }));
        }
        Ok(None)
    }

    /// Non-mutating probe: how many peer-opened request streams
    /// are currently waiting for [`accept_request`]. Tests +
    /// pump-drive loops use this to know when to stop polling.
    ///
    /// [`accept_request`]: Connection::accept_request
    pub fn pending_request_count(&self) -> usize {
        self.incoming_requests.len()
    }

    /// Non-mutating probe: `true` if `stream`'s request /
    /// response HEADERS frame has arrived + been decoded but
    /// hasn't been read out via [`read_headers`] yet.
    ///
    /// [`read_headers`]: Connection::read_headers
    pub fn received_headers_ready(&self, stream: &RequestStream) -> bool {
        self.requests
            .get(&stream.sid.0)
            .is_some_and(|st| !st.headers_dispatched && st.received_headers.is_some())
    }

    /// Non-mutating probe: `(buffered_data_bytes, fin_seen)` for
    /// `stream`. Useful in pump-driven tests to know when to
    /// stop pumping vs read the body.
    pub fn data_buffer_status(&self, stream: &RequestStream) -> (usize, bool) {
        self.requests
            .get(&stream.sid.0)
            .map(|st| (st.body.len(), st.body_fin))
            .unwrap_or((0, false))
    }

    /// Write a HEADERS frame on `stream` (response headers from
    /// the server side; informational 1xx from either).
    pub fn write_headers(
        &mut self,
        stream: &RequestStream,
        headers: &[Header],
    ) -> Result<(), Error> {
        self.send_headers(stream.sid.0, headers)
    }

    fn send_headers(&mut self, sid: u64, headers: &[Header]) -> Result<(), Error> {
        let mut encoded = Vec::new();
        encode_field_section(headers, &mut encoded).map_err(|_| Error::QpackEncode)?;
        let mut frame_buf = Vec::new();
        encode_frame(
            &Frame::Headers {
                encoded_field_section: encoded,
            },
            &mut frame_buf,
        )?;
        self.quic.write_stream(StreamId(sid), &frame_buf, false)?;
        Ok(())
    }

    /// Set the underlying QUIC 1-RTT plaintext budget per
    /// datagram — the caller asserts the resulting datagram
    /// size fits the path MTU (loopback links can go far
    /// beyond the 1350 default).
    pub fn set_datagram_budget(&mut self, plaintext_budget: usize) {
        self.quic.set_datagram_budget(plaintext_budget);
    }

    /// Link-liveness passthrough to L4 (1.7.0): quiet-link
    /// keepalive probe interval + no-inbound death bound,
    /// in µs. See [`grpcx_quic::QuicConnection::set_keepalive_us`]
    /// / [`grpcx_quic::QuicConnection::set_idle_timeout_us`].
    pub fn set_link_liveness_us(&mut self, keepalive_us: u64, idle_timeout_us: u64) {
        self.quic.set_keepalive_us(keepalive_us);
        self.quic.set_idle_timeout_us(idle_timeout_us);
    }

    /// Write a DATA frame on `stream`. Returns the bytes accepted
    /// (always `data.len()` in v0.5 — no L5 flow control yet).
    pub fn write_data(&mut self, stream: &RequestStream, data: &[u8]) -> Result<usize, Error> {
        // Fast path: emit the DATA frame header directly and
        // append the payload as a second stream write —
        // skipping the owned-`Frame` clone and the
        // intermediate frame buffer. On bulk transfers those
        // two copies (plus the buffer's regrowth) were the
        // second-hottest chain in the 64 KiB profile.
        let mut hdr = [0u8; 16];
        let mut n = crate::varint::write_varint(crate::frame::TYPE_DATA, &mut hdr)?;
        n += crate::varint::write_varint(data.len() as u64, &mut hdr[n..])?;
        self.quic.write_stream(stream.sid, &hdr[..n], false)?;
        self.quic.write_stream(stream.sid, data, false)?;
        Ok(data.len())
    }

    /// Send FIN on `stream`. No further writes are accepted.
    pub fn finish(&mut self, stream: &RequestStream) -> Result<(), Error> {
        self.quic.write_stream(stream.sid, &[], true)?;
        Ok(())
    }

    /// Read the request / response HEADERS frame (`Ok(Some(headers))`
    /// the first time HEADERS is available; subsequent calls
    /// return `Ok(None)`).
    pub fn read_headers(&mut self, stream: &RequestStream) -> Result<Option<Vec<Header>>, Error> {
        let st = self
            .requests
            .get_mut(&stream.sid.0)
            .ok_or(Error::Internal)?;
        if st.headers_dispatched {
            return Ok(None);
        }
        if let Some(hdrs) = st.received_headers.clone() {
            st.headers_dispatched = true;
            return Ok(Some(hdrs));
        }
        Ok(None)
    }

    /// Release all connection-side state for a finished
    /// request stream (v1.3): the H/3 `RequestState` and
    /// the underlying L4 stream entry. Call once the
    /// caller has consumed everything it needs (headers /
    /// body / trailers) — further reads on the handle
    /// return empty/`None`. Without this, every RPC leaks
    /// one dead request entry and the per-pump stream
    /// sweep degrades linearly with call count.
    pub fn close_request(&mut self, stream: &RequestStream) {
        if let Some(mut st) = self.requests.remove(&stream.sid.0) {
            // Recycle the big per-request buffers (see
            // `spare_request_bufs`).
            for mut b in [
                core::mem::take(&mut st.in_buf),
                core::mem::take(&mut st.body),
            ] {
                if b.capacity() > 0 && self.spare_request_bufs.len() < 8 {
                    b.clear();
                    self.spare_request_bufs.push(b);
                }
            }
        }
        self.quic.close_stream(stream.sid);
    }

    /// A `RequestState` with its two big buffers taken from
    /// the spare pool when available (capacity recycling —
    /// see `spare_request_bufs`).
    fn fresh_request_state(&mut self) -> RequestState {
        let mut st = RequestState::default();
        if let Some(b) = self.spare_request_bufs.pop() {
            st.in_buf = b;
        }
        if let Some(b) = self.spare_request_bufs.pop() {
            st.body = b;
        }
        st
    }

    /// `true` once a trailing HEADERS section has arrived on
    /// `stream` and hasn't been read out via
    /// [`read_trailers`] yet.
    ///
    /// [`read_trailers`]: Connection::read_trailers
    pub fn received_trailers_ready(&self, stream: &RequestStream) -> bool {
        self.requests
            .get(&stream.sid.0)
            .is_some_and(|st| !st.trailers_dispatched && st.received_trailers.is_some())
    }

    /// Read the trailing HEADERS section (the second HEADERS
    /// frame on the stream — RFC 9114 §4.1; gRPC carries
    /// `grpc-status` there). `Ok(Some(trailers))` the first
    /// time trailers are available; subsequent calls return
    /// `Ok(None)`.
    pub fn read_trailers(&mut self, stream: &RequestStream) -> Result<Option<Vec<Header>>, Error> {
        let st = self
            .requests
            .get_mut(&stream.sid.0)
            .ok_or(Error::Internal)?;
        if st.trailers_dispatched {
            return Ok(None);
        }
        if let Some(hdrs) = st.received_trailers.clone() {
            st.trailers_dispatched = true;
            return Ok(Some(hdrs));
        }
        Ok(None)
    }

    /// Drain buffered DATA into `out`. Returns `(n, fin)` where
    /// `n` is the bytes appended and `fin` is true once the peer
    /// has finished the stream and we've drained the last DATA
    /// frame.
    pub fn read_data(
        &mut self,
        stream: &RequestStream,
        out: &mut Vec<u8>,
    ) -> Result<(usize, bool), Error> {
        let st = self
            .requests
            .get_mut(&stream.sid.0)
            .ok_or(Error::Internal)?;
        let n = st.body.len();
        out.extend_from_slice(&st.body);
        st.body.clear();
        Ok((n, st.body_fin))
    }

    /// Emit GOAWAY on the control stream with the highest
    /// accepted request stream id (server) / push id (client; v0.5
    /// always sends 0 from the client side since we don't accept
    /// pushes). Idempotent: subsequent calls are no-ops.
    pub fn goaway(&mut self) -> Result<(), Error> {
        if self.goaway_sent {
            return Ok(());
        }
        let id = match self.side {
            Side::Server => self.highest_accepted_request_id,
            Side::Client => 0,
        };
        let mut frame_buf = Vec::new();
        encode_frame(
            &Frame::Goaway {
                stream_or_push_id: id,
            },
            &mut frame_buf,
        )?;
        let Some(sid) = self.local_control_sid else {
            return Err(Error::Internal);
        };
        self.quic.write_stream(StreamId(sid), &frame_buf, false)?;
        self.goaway_sent = true;
        Ok(())
    }

    /// GOAWAY id the peer sent us (`None` until received).
    pub fn peer_goaway(&self) -> Option<u64> {
        self.peer_goaway
    }

    // ============================================================
    // v0.7 lower-level helpers — encoder-stream write + dynamic-
    // table probes. Used by the v0.7 acceptance test (and any
    // caller that wants to drive the dynamic table without an
    // automated encoder-side strategy, which lands in v1.x).
    // ============================================================

    /// Write raw bytes onto our **local QPACK encoder uni-stream**.
    /// The caller is responsible for emitting well-formed
    /// [`crate::EncoderInstruction`]-encoded bytes.
    pub fn write_to_qpack_encoder_stream(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let Some(sid) = self.local_qpack_encoder_sid else {
            return Err(Error::Internal);
        };
        self.quic.write_stream(StreamId(sid), bytes, false)?;
        Ok(())
    }

    /// Number of entries in our reconstruction of the peer
    /// encoder's dynamic table (test probe).
    pub fn peer_dynamic_table_insert_count(&self) -> u64 {
        self.peer_dynamic_table.insert_count()
    }

    /// Byte capacity of the peer encoder's dynamic table, as
    /// last seen via a `SetDynamicTableCapacity` instruction
    /// (test probe).
    pub fn peer_dynamic_table_capacity(&self) -> u64 {
        self.peer_dynamic_table.capacity()
    }

    // ============================================================
    // 1.1.0: upper-layer raw-stream surface for WebTransport
    // (RFC 9220) and similar extensions. Lets a higher layer claim
    // reserved-typed uni streams + opt peer bidis out of L5's
    // H/3 request path, and open its own raw uni / bi streams
    // without L5 framing.
    // ============================================================

    /// Pop the next peer-initiated **reserved-typed** uni stream
    /// L5 hasn't claimed for its own H/3 use (control / QPACK /
    /// push). Returns `(stream_id, raw_type_code)`. Any post-prefix
    /// bytes that arrived in the same QUIC frame as the type byte
    /// are buffered and retrievable via [`read_stream_raw`]; new
    /// bytes flow through L4 normally.
    ///
    /// 1.1.0 addition; backwards-compatible — existing callers
    /// that don't poll this leave reserved streams sitting on the
    /// queue (no flow-control runaway since v0.5 doesn't carry
    /// stream flow-control).
    ///
    /// [`read_stream_raw`]: Connection::read_stream_raw
    pub fn accept_unknown_uni_stream(&mut self) -> Option<(StreamId, u64)> {
        if self.unknown_uni_pending.is_empty() {
            None
        } else {
            let (sid, type_code) = self.unknown_uni_pending.remove(0);
            Some((StreamId(sid), type_code))
        }
    }

    /// Pop the next peer-initiated bidi stream and claim it out
    /// of L5's H/3 request path. After this call,
    /// [`accept_request`] will not return this sid, and L5 never
    /// decodes H/3 frames on it — the upper layer owns I/O via
    /// [`read_stream_raw`] / [`write_stream_raw`].
    ///
    /// Returns `None` when no peer bidi is pending. FIFO with
    /// [`accept_request`]: both methods pop from the same queue;
    /// the caller chooses which path each bidi takes. WT-aware
    /// callers must invoke this before [`accept_request`] when
    /// expecting WebTransport bidis (typically the WT layer
    /// peeks the L4 stream buffer first to decide).
    ///
    /// 1.1.0 addition. WebTransport (RFC 9220) uses this to claim
    /// bidis tagged `[0x54][session_id]`.
    ///
    /// [`accept_request`]: Connection::accept_request
    /// [`read_stream_raw`]: Connection::read_stream_raw
    /// [`write_stream_raw`]: Connection::write_stream_raw
    pub fn accept_unknown_bi_stream(&mut self) -> Option<StreamId> {
        if self.incoming_requests.is_empty() {
            return None;
        }
        let sid = self.incoming_requests.remove(0);
        self.wt_owned_sids.insert(sid);
        Some(StreamId(sid))
    }

    /// Open a new locally-initiated **raw** uni stream — passes
    /// straight through to L4 [`grpcx_quic::QuicConnection::open_uni`].
    /// No H/3 type byte is written; the caller owns framing.
    ///
    /// 1.1.0 addition for WebTransport's uni stream path.
    pub fn open_uni_raw(&mut self) -> StreamId {
        self.quic.open_uni()
    }

    /// Open a new locally-initiated **raw** bidi stream — passes
    /// straight through to L4 [`grpcx_quic::QuicConnection::open_bi`].
    /// L5 records the sid in its `wt_owned_sids` set so the local
    /// bidi never gets treated as an H/3 request bidi (it's
    /// locally opened so `accept_bi` won't see it, but the marker
    /// keeps drain_request_streams skipping it should L5 ever
    /// learn about local bidis).
    ///
    /// 1.1.0 addition for WebTransport's bidi stream path.
    pub fn open_bi_raw(&mut self) -> StreamId {
        let sid = self.quic.open_bi();
        self.wt_owned_sids.insert(sid.0);
        sid
    }

    /// Drain buffered bytes for `sid` into `out`. Returns
    /// `(total_bytes_read, fin)`. Drains 1.1.0's claim-time buffer
    /// (post-prefix bytes captured for reserved-typed uni / bytes
    /// drain_request_streams stashed for a bidi pre-claim) before
    /// falling through to fresh L4 bytes.
    pub fn read_stream_raw(&mut self, sid: StreamId, out: &mut Vec<u8>) -> (usize, bool) {
        let start = out.len();
        if let Some(buf) = self.claimed_uni_buf.remove(&sid.0) {
            out.extend_from_slice(&buf);
        }
        if let Some(buf) = self.claimed_bi_buf.remove(&sid.0) {
            out.extend_from_slice(&buf);
        }
        let (_, fin) = self.quic.read_stream(sid, out);
        (out.len() - start, fin)
    }

    /// Write raw bytes to `sid` — passes through to L4
    /// [`grpcx_quic::QuicConnection::write_stream`]. No H/3
    /// framing.
    pub fn write_stream_raw(
        &mut self,
        sid: StreamId,
        data: &[u8],
        fin: bool,
    ) -> Result<usize, grpcx_quic::Error> {
        self.quic.write_stream(sid, data, fin)
    }

    /// Send one H/3 datagram (RFC 9297) on `stream`.
    /// Encodes the leading `[quarter_stream_id varint][payload]`
    /// shape over L4's `send_datagram`. `stream.0` must be on the
    /// 4-stride class boundary (`stream.0 % 4 == 0`); otherwise
    /// returns `Error::Internal`. 1.2.0 addition.
    pub fn send_h3_datagram(&mut self, stream: StreamId, payload: &[u8]) -> Result<(), Error> {
        if !stream.0.is_multiple_of(4) {
            return Err(Error::Internal);
        }
        let quarter = stream.0 / 4;
        let n = varint_len(quarter);
        let mut wire = Vec::with_capacity(n + payload.len());
        let mut tmp = [0u8; 8];
        write_varint(quarter, &mut tmp)?;
        wire.extend_from_slice(&tmp[..n]);
        wire.extend_from_slice(payload);
        self.quic.send_datagram(&wire)?;
        Ok(())
    }

    /// Pop the next inbound H/3 datagram, decoded into
    /// `(stream_id, payload)`. Returns `None` when nothing is
    /// pending or when the next inbound datagram has a malformed
    /// leading varint (silent drop per RFC 9297 §2.1 — malformed
    /// datagrams are non-fatal). 1.2.0 addition.
    pub fn recv_h3_datagram(&mut self) -> Option<(StreamId, Vec<u8>)> {
        let raw = self.quic.recv_datagram()?;
        let mut cursor: &[u8] = &raw;
        let quarter = read_varint(&mut cursor).ok()?;
        let consumed = raw.len() - cursor.len();
        let stream_id = quarter.checked_mul(4)?;
        Some((StreamId(stream_id), raw[consumed..].to_vec()))
    }

    /// Bytes still queued on our local decoder stream that we
    /// haven't flushed yet (Insert-Count-Increment +
    /// Section-Ack). Test probe.
    pub fn local_decoder_stream_bytes_in_flight(&self) -> usize {
        // Pending counters/queues; the actual write happens in
        // pump's `flush_decoder_instructions`.
        let increment_bytes = if self.pending_insert_count_increment > 0 {
            // Conservative upper bound: 10 bytes per varint.
            10
        } else {
            0
        };
        increment_bytes + self.pending_section_acks.len() * 10
    }
}

// ============================================================
// RequestStream — opaque handle (the actual ops live on
// Connection). Keeps the API shape narrow.
// ============================================================

/// One H/3 request stream handle. The owning [`Connection`]
/// holds all the state; this type is a typed handle the caller
/// passes back to the connection's I/O methods.
#[derive(Debug, Clone, Copy)]
pub struct RequestStream {
    sid: StreamId,
}

impl RequestStream {
    /// The L4 stream id backing this request.
    pub fn stream_id(&self) -> StreamId {
        self.sid
    }
}

// ============================================================
// SETTINGS (de)serialise.
// ============================================================

fn serialize_settings(s: &Settings) -> Vec<SettingPair> {
    let mut pairs = Vec::new();
    if let Some(v) = s.max_field_section_size {
        pairs.push(SettingPair {
            id: SETTINGS_MAX_FIELD_SECTION_SIZE,
            value: v,
        });
    }
    if let Some(v) = s.qpack_max_table_capacity {
        pairs.push(SettingPair {
            id: SETTINGS_QPACK_MAX_TABLE_CAPACITY,
            value: v,
        });
    }
    if let Some(v) = s.qpack_blocked_streams {
        pairs.push(SettingPair {
            id: SETTINGS_QPACK_BLOCKED_STREAMS,
            value: v,
        });
    }
    if s.enable_connect_protocol {
        pairs.push(SettingPair {
            id: SETTINGS_ENABLE_CONNECT_PROTOCOL,
            value: 1,
        });
    }
    pairs
}

fn parse_settings(pairs: &[SettingPair]) -> Settings {
    let mut s = Settings::default();
    for SettingPair { id, value } in pairs {
        match *id {
            SETTINGS_MAX_FIELD_SECTION_SIZE => s.max_field_section_size = Some(*value),
            SETTINGS_QPACK_MAX_TABLE_CAPACITY => s.qpack_max_table_capacity = Some(*value),
            SETTINGS_QPACK_BLOCKED_STREAMS => s.qpack_blocked_streams = Some(*value),
            SETTINGS_ENABLE_CONNECT_PROTOCOL => s.enable_connect_protocol = *value != 0,
            // RFC 9114 §7.2.4: unknown setting IDs MUST be
            // ignored.
            _ => {}
        }
    }
    s
}
