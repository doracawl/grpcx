//! H/2 connection state machine — RFC 9113.
//!
//! Same sans-IO shape as L4/L5: `pump(incoming bytes) ->
//! outgoing byte chunks`, byte-stream flavoured (TCP).
//! The request-stream API mirrors L5's H/3 `Connection`
//! so L10's transport wiring is symmetric:
//! `send_request` / `accept_request` / `write_headers` /
//! `write_data` / `write_trailers` / `finish` /
//! `read_headers` / `read_data` / `read_trailers` /
//! `close_stream`.
//!
//! Scope (gRPC-interop subset):
//! - PRIORITY parsed-and-ignored (deprecated).
//! - PUSH_PROMISE → protocol error (we advertise
//!   `ENABLE_PUSH = 0`).
//! - Flow control: full receive-side accounting with
//!   WINDOW_UPDATE replenishment; send side buffers and
//!   respects both windows.
//! - CONTINUATION accepted on receive; our own header
//!   blocks always fit one frame (we cap at max frame
//!   size and gRPC headers are small).

use crate::frame::{
    self, FLAG_ACK, FLAG_END_HEADERS, FLAG_END_STREAM, H2Error, RawFrame, SETTINGS_ENABLE_PUSH,
    SETTINGS_HEADER_TABLE_SIZE, SETTINGS_INITIAL_WINDOW_SIZE, SETTINGS_MAX_FRAME_SIZE,
    TYPE_CONTINUATION, TYPE_DATA, TYPE_GOAWAY, TYPE_HEADERS, TYPE_PING, TYPE_PRIORITY,
    TYPE_PUSH_PROMISE, TYPE_RST_STREAM, TYPE_SETTINGS, TYPE_WINDOW_UPDATE,
};
use crate::hpack::{DynamicTable, decode_header_block, encode_header_block};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const DEFAULT_WINDOW: i64 = 65_535;
const MAX_FRAME_SIZE: usize = 16_384;
/// Replenish the receive window once consumption crosses
/// half the default (one WINDOW_UPDATE per ~32 KiB).
const REPLENISH_THRESHOLD: i64 = DEFAULT_WINDOW / 2;

/// A header as `(name, value)` — same shape as L5's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Lowercase header name (pseudo-headers include the
    /// leading `:`).
    pub name: Vec<u8>,
    /// Raw value bytes.
    pub value: Vec<u8>,
}

/// Handle for one request stream (copyable token, same
/// pattern as L5).
#[derive(Debug, Clone, Copy)]
pub struct RequestStream {
    sid: u32,
}

impl RequestStream {
    /// The H2 stream id backing this request.
    pub fn stream_id(&self) -> u32 {
        self.sid
    }
}

#[derive(Default)]
struct StreamState {
    // ---- receive side ----
    recv_headers: Option<Vec<Header>>,
    headers_dispatched: bool,
    recv_trailers: Option<Vec<Header>>,
    trailers_dispatched: bool,
    body: Vec<u8>,
    end_seen: bool,
    /// Header block under reassembly (HEADERS without
    /// END_HEADERS + CONTINUATIONs).
    partial_block: Option<(Vec<u8>, bool)>, // (bytes, end_stream_flagged)
    recv_window_consumed: i64,
    // ---- send side ----
    send_buf: Vec<u8>,
    send_fin: bool,
    fin_sent: bool,
    send_window: i64,
    /// Encoded trailer block queued behind `send_buf` —
    /// trailers MUST follow the data on the wire, and the
    /// data is window-buffered, so an immediate HEADERS
    /// emission would reorder (our own lenient decoder
    /// tolerated it; real peers reject DATA after
    /// END_STREAM).
    pending_trailers: Option<Vec<u8>>,
    closing: bool,
}

enum Phase {
    /// Client: preface not yet emitted. Server: preface
    /// not yet received.
    Setup,
    Open,
}

#[derive(PartialEq, Clone, Copy)]
enum Side {
    Client,
    Server,
}

/// One end of an HTTP/2 connection.
pub struct Connection {
    side: Side,
    phase: Phase,
    in_buf: Vec<u8>,
    out: Vec<u8>,
    streams: BTreeMap<u32, StreamState>,
    accept_queue: Vec<u32>,
    next_stream_id: u32,
    decode_table: DynamicTable,
    settings_acked: bool,
    peer_settings_received: bool,
    conn_send_window: i64,
    conn_recv_consumed: i64,
    peer_initial_window: i64,
    goaway_received: bool,
    reap_pending: bool,
    /// Recently-reset sids (bounded ring, eager cancel
    /// 0.3.0): writes for them are silent no-ops — a
    /// response landing after the peer's RST_STREAM must
    /// not re-register the stream (or emit frames the
    /// peer treats as a protocol error).
    reset_sids: Vec<u32>,

    // ---- Link liveness (0.2.0, the L4 pattern) ----
    /// Monotonic clock, µs — max over `pump_at` stamps;
    /// stays 0 on never-timed connections (liveness off).
    now_us: u64,
    /// Quiet-link PING interval, µs.
    keepalive_us: u64,
    /// No-inbound death bound, µs.
    idle_timeout_us: u64,
    /// Clock at the last inbound bytes (0 = none yet).
    last_inbound_us: u64,
    /// Last keepalive PING emit time (rate limit).
    last_keepalive_us: u64,
    /// Clock at the first timed pump (bounds a setup
    /// against a peer that never answers; 0 = not yet).
    started_us: u64,
    /// Opaque PING payload counter.
    ping_seq: u64,
    /// Clock at the previous liveness check (gap
    /// forgiveness — see `check_liveness`).
    last_liveness_check_us: u64,
}

/// Link-liveness defaults, µs (lab-scale; see the setters).
const DEFAULT_KEEPALIVE_US: u64 = 2_500_000;
const DEFAULT_IDLE_TIMEOUT_US: u64 = 10_000_000;

impl Connection {
    /// Client side. The preface + SETTINGS go out on the
    /// first [`pump`](Self::pump).
    pub fn new_client() -> Self {
        Self::new(Side::Client)
    }

    /// Server side. Expects the client preface first.
    pub fn new_server() -> Self {
        Self::new(Side::Server)
    }

    fn new(side: Side) -> Self {
        Connection {
            side,
            phase: Phase::Setup,
            in_buf: Vec::new(),
            out: Vec::new(),
            streams: BTreeMap::new(),
            accept_queue: Vec::new(),
            next_stream_id: if side == Side::Client { 1 } else { 2 },
            decode_table: DynamicTable::new(4096),
            settings_acked: false,
            peer_settings_received: false,
            conn_send_window: DEFAULT_WINDOW,
            conn_recv_consumed: 0,
            peer_initial_window: DEFAULT_WINDOW,
            goaway_received: false,
            reap_pending: false,
            reset_sids: Vec::new(),
            now_us: 0,
            keepalive_us: DEFAULT_KEEPALIVE_US,
            idle_timeout_us: DEFAULT_IDLE_TIMEOUT_US,
            last_inbound_us: 0,
            last_keepalive_us: 0,
            started_us: 0,
            ping_seq: 0,
            last_liveness_check_us: 0,
        }
    }

    /// Set the quiet-link PING interval (µs, default
    /// 2.5 s). Any inbound bytes are the liveness proof;
    /// the PING merely elicits some (its ACK). Liveness
    /// runs only on timed pumps ([`pump_at`](Self::pump_at)).
    pub fn set_keepalive_us(&mut self, keepalive_us: u64) {
        self.keepalive_us = keepalive_us.max(1);
    }

    /// Set the no-inbound death bound (µs, default 10 s):
    /// past it, [`pump_at`](Self::pump_at) returns
    /// [`H2Error::LinkDead`]. Also bounds the SETTINGS
    /// exchange against a peer that never answers.
    pub fn set_idle_timeout_us(&mut self, idle_timeout_us: u64) {
        self.idle_timeout_us = idle_timeout_us.max(1);
    }

    /// `true` once SETTINGS have been exchanged both ways.
    pub fn is_ready(&self) -> bool {
        matches!(self.phase, Phase::Open) && self.peer_settings_received && self.settings_acked
    }

    /// `true` once the peer sent GOAWAY.
    pub fn peer_goaway(&self) -> bool {
        self.goaway_received
    }

    /// Feed inbound bytes (or `None`) and collect
    /// outbound byte chunks.
    pub fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, H2Error> {
        let now = self.now_us;
        self.pump_at(incoming, now)
    }

    /// [`pump`](Self::pump) with a caller-supplied
    /// monotonic clock reading (µs) — the sans-IO time
    /// surface (0.2.0, the L4 pattern: the clock only
    /// moves forward, and a never-timed connection never
    /// fires a timer). Drives the link-liveness atom:
    /// quiet past the keepalive interval → a PING goes
    /// out (the receive side already ACKs them); no
    /// inbound past the idle timeout →
    /// [`H2Error::LinkDead`].
    pub fn pump_at(
        &mut self,
        incoming: Option<&[u8]>,
        now_us: u64,
    ) -> Result<Vec<Vec<u8>>, H2Error> {
        self.now_us = self.now_us.max(now_us);
        if self.now_us > 0 && self.started_us == 0 {
            self.started_us = self.now_us;
        }
        if let Some(b) = incoming {
            if !b.is_empty() {
                // Bytes on the wire prove the link.
                self.last_inbound_us = self.now_us.max(1);
            }
            self.in_buf.extend_from_slice(b);
        }
        self.check_liveness()?;

        if matches!(self.phase, Phase::Setup) {
            match self.side {
                Side::Client => {
                    self.out.extend_from_slice(PREFACE);
                    self.emit_settings();
                    self.phase = Phase::Open;
                }
                Side::Server => {
                    if self.in_buf.len() < PREFACE.len() {
                        return Ok(self.take_out());
                    }
                    if &self.in_buf[..PREFACE.len()] != PREFACE {
                        return Err(H2Error::Protocol("bad client preface"));
                    }
                    self.in_buf.drain(..PREFACE.len());
                    self.emit_settings();
                    self.phase = Phase::Open;
                }
            }
        }

        // Drain complete frames.
        while let Some((f, consumed)) = frame::try_decode_frame(&self.in_buf, MAX_FRAME_SIZE)? {
            self.in_buf.drain(..consumed);
            self.handle_frame(f)?;
        }

        // Flush send-side stream buffers within windows.
        self.flush_streams();

        // Replenish receive windows.
        self.replenish();

        // Reap soft-closed drained streams.
        if self.reap_pending {
            self.streams.retain(|_, st| {
                !(st.closing
                    && (st.fin_sent || (st.send_buf.is_empty() && st.pending_trailers.is_none())))
            });
            self.reap_pending = self.streams.values().any(|st| st.closing);
        }

        Ok(self.take_out())
    }

    /// Keepalive PING on a quiet link; death past the idle
    /// timeout. Frozen clock = liveness off.
    fn check_liveness(&mut self) -> Result<(), H2Error> {
        let now = self.now_us;
        if now == 0 || self.started_us == 0 {
            return Ok(());
        }
        // Gap forgiveness: a pump gap wider than the
        // keepalive interval means the driver wasn't
        // watching (a blocking caller between calls) —
        // re-anchor instead of firing a false death.
        if now.saturating_sub(self.last_liveness_check_us) > self.keepalive_us
            && self.last_liveness_check_us > 0
        {
            self.last_inbound_us = now.max(1);
        }
        self.last_liveness_check_us = now;
        let quiet = now.saturating_sub(self.last_inbound_us.max(self.started_us));
        if quiet >= self.idle_timeout_us {
            return Err(H2Error::LinkDead);
        }
        if self.is_ready()
            && quiet >= self.keepalive_us
            && now.saturating_sub(self.last_keepalive_us) >= self.keepalive_us
        {
            self.ping_seq += 1;
            frame::encode_frame(TYPE_PING, 0, 0, &self.ping_seq.to_be_bytes(), &mut self.out);
            self.last_keepalive_us = now;
        }
        Ok(())
    }

    fn take_out(&mut self) -> Vec<Vec<u8>> {
        if self.out.is_empty() {
            Vec::new()
        } else {
            alloc::vec![core::mem::take(&mut self.out)]
        }
    }

    fn emit_settings(&mut self) {
        let mut payload = Vec::new();
        frame::encode_settings(
            &[
                (SETTINGS_ENABLE_PUSH, 0),
                (SETTINGS_MAX_FRAME_SIZE, MAX_FRAME_SIZE as u32),
            ],
            &mut payload,
        );
        frame::encode_frame(TYPE_SETTINGS, 0, 0, &payload, &mut self.out);
    }

    // ---------- frame dispatch ----------

    fn handle_frame(&mut self, f: RawFrame) -> Result<(), H2Error> {
        match f.ty {
            TYPE_SETTINGS => {
                if f.flags & FLAG_ACK != 0 {
                    self.settings_acked = true;
                    return Ok(());
                }
                for (id, value) in frame::parse_settings(&f.payload)? {
                    match id {
                        SETTINGS_INITIAL_WINDOW_SIZE => {
                            let delta = value as i64 - self.peer_initial_window;
                            self.peer_initial_window = value as i64;
                            for st in self.streams.values_mut() {
                                st.send_window += delta;
                            }
                        }
                        SETTINGS_HEADER_TABLE_SIZE => {
                            // Our encoder is stateless; the
                            // peer's decoder cap doesn't
                            // affect us.
                        }
                        _ => {}
                    }
                }
                self.peer_settings_received = true;
                frame::encode_frame(TYPE_SETTINGS, FLAG_ACK, 0, &[], &mut self.out);
            }
            TYPE_PING => {
                if f.flags & FLAG_ACK == 0 {
                    frame::encode_frame(TYPE_PING, FLAG_ACK, 0, &f.payload, &mut self.out);
                }
            }
            TYPE_WINDOW_UPDATE => {
                if f.payload.len() != 4 {
                    return Err(H2Error::BadFrame);
                }
                let inc =
                    (u32::from_be_bytes([f.payload[0], f.payload[1], f.payload[2], f.payload[3]])
                        & 0x7fff_ffff) as i64;
                if f.stream_id == 0 {
                    self.conn_send_window += inc;
                } else if let Some(st) = self.streams.get_mut(&f.stream_id) {
                    st.send_window += inc;
                }
            }
            TYPE_GOAWAY => {
                self.goaway_received = true;
            }
            TYPE_RST_STREAM => {
                self.streams.remove(&f.stream_id);
                self.note_reset(f.stream_id);
            }
            TYPE_PRIORITY => {} // deprecated; ignore
            TYPE_PUSH_PROMISE => {
                return Err(H2Error::Protocol("push not permitted"));
            }
            TYPE_HEADERS => {
                let body = frame::payload_body(TYPE_HEADERS, f.flags, &f.payload)?.to_vec();
                let end_stream = f.flags & FLAG_END_STREAM != 0;
                if f.flags & FLAG_END_HEADERS != 0 {
                    self.complete_header_block(f.stream_id, &body, end_stream)?;
                } else {
                    let st = self.stream_entry(f.stream_id);
                    st.partial_block = Some((body, end_stream));
                }
            }
            TYPE_CONTINUATION => {
                let st = self
                    .streams
                    .get_mut(&f.stream_id)
                    .ok_or(H2Error::Protocol("continuation on unknown stream"))?;
                let Some((mut block, end_stream)) = st.partial_block.take() else {
                    return Err(H2Error::Protocol("continuation without headers"));
                };
                block.extend_from_slice(&f.payload);
                if f.flags & FLAG_END_HEADERS != 0 {
                    self.complete_header_block(f.stream_id, &block, end_stream)?;
                } else {
                    self.stream_entry(f.stream_id).partial_block = Some((block, end_stream));
                }
            }
            TYPE_DATA => {
                let body = frame::payload_body(TYPE_DATA, f.flags, &f.payload)?;
                let len = f.payload.len() as i64; // padding counts for flow control
                self.conn_recv_consumed += len;
                let st = self.stream_entry(f.stream_id);
                st.recv_window_consumed += len;
                st.body.extend_from_slice(body);
                if f.flags & FLAG_END_STREAM != 0 {
                    st.end_seen = true;
                }
            }
            _ => {} // unknown frame types are ignored (§5.5)
        }
        Ok(())
    }

    fn stream_entry(&mut self, sid: u32) -> &mut StreamState {
        let initial = self.peer_initial_window;
        let is_new = !self.streams.contains_key(&sid);
        let st = self.streams.entry(sid).or_insert_with(|| StreamState {
            send_window: initial,
            ..StreamState::default()
        });
        if is_new && self.side == Side::Server && sid % 2 == 1 {
            self.accept_queue.push(sid);
        }
        st
    }

    fn complete_header_block(
        &mut self,
        sid: u32,
        block: &[u8],
        end_stream: bool,
    ) -> Result<(), H2Error> {
        let pairs = decode_header_block(block, &mut self.decode_table)?;
        let headers: Vec<Header> = pairs
            .into_iter()
            .map(|(n, v)| Header {
                name: n.into_bytes(),
                value: v,
            })
            .collect();
        let st = self.stream_entry(sid);
        if st.recv_headers.is_none() {
            st.recv_headers = Some(headers);
        } else if st.recv_trailers.is_none() {
            st.recv_trailers = Some(headers);
        }
        // Third block: dropped (peer bug) — same posture
        // as the H/3 side.
        if end_stream {
            st.end_seen = true;
        }
        Ok(())
    }

    // ---------- flow control ----------

    fn flush_streams(&mut self) {
        let sids: Vec<u32> = self
            .streams
            .iter()
            .filter(|(_, st)| !st.send_buf.is_empty() || (st.send_fin && !st.fin_sent))
            .map(|(s, _)| *s)
            .collect();
        for sid in sids {
            let st = self.streams.get_mut(&sid).expect("collected above");
            loop {
                let budget = st
                    .send_window
                    .min(self.conn_send_window)
                    .min(MAX_FRAME_SIZE as i64);
                if st.send_buf.is_empty() {
                    if st.send_fin && !st.fin_sent {
                        // Trailers (if queued) carry the
                        // END_STREAM; otherwise an empty
                        // DATA does.
                        if let Some(block) = st.pending_trailers.take() {
                            frame::encode_frame(
                                TYPE_HEADERS,
                                FLAG_END_HEADERS | FLAG_END_STREAM,
                                sid,
                                &block,
                                &mut self.out,
                            );
                        } else {
                            frame::encode_frame(
                                TYPE_DATA,
                                FLAG_END_STREAM,
                                sid,
                                &[],
                                &mut self.out,
                            );
                        }
                        st.fin_sent = true;
                    }
                    break;
                }
                if budget <= 0 {
                    break; // window-starved; resume on WINDOW_UPDATE
                }
                let take = (budget as usize).min(st.send_buf.len());
                let chunk: Vec<u8> = st.send_buf.drain(..take).collect();
                let last = st.send_buf.is_empty() && st.send_fin && st.pending_trailers.is_none();
                let flags = if last { FLAG_END_STREAM } else { 0 };
                frame::encode_frame(TYPE_DATA, flags, sid, &chunk, &mut self.out);
                if last {
                    st.fin_sent = true;
                }
                st.send_window -= take as i64;
                self.conn_send_window -= take as i64;
            }
        }
    }

    fn replenish(&mut self) {
        if self.conn_recv_consumed >= REPLENISH_THRESHOLD {
            let inc = self.conn_recv_consumed as u32;
            self.conn_recv_consumed = 0;
            frame::encode_frame(TYPE_WINDOW_UPDATE, 0, 0, &inc.to_be_bytes(), &mut self.out);
        }
        let sids: Vec<u32> = self
            .streams
            .iter()
            .filter(|(_, st)| st.recv_window_consumed >= REPLENISH_THRESHOLD && !st.end_seen)
            .map(|(s, _)| *s)
            .collect();
        for sid in sids {
            let st = self.streams.get_mut(&sid).expect("collected above");
            let inc = st.recv_window_consumed as u32;
            st.recv_window_consumed = 0;
            frame::encode_frame(
                TYPE_WINDOW_UPDATE,
                0,
                sid,
                &inc.to_be_bytes(),
                &mut self.out,
            );
        }
    }

    // ---------- request-stream API (mirrors L5) ----------

    /// Open a client-initiated stream carrying `headers`
    /// (END_HEADERS; stream stays open for data).
    pub fn send_request(&mut self, headers: &[(String, Vec<u8>)]) -> RequestStream {
        let sid = self.next_stream_id;
        self.next_stream_id += 2;
        let mut block = Vec::new();
        encode_header_block(headers, &mut block);
        frame::encode_frame(TYPE_HEADERS, FLAG_END_HEADERS, sid, &block, &mut self.out);
        let initial = self.peer_initial_window;
        self.streams.entry(sid).or_insert_with(|| StreamState {
            send_window: initial,
            ..StreamState::default()
        });
        RequestStream { sid }
    }

    /// Pop the next peer-initiated stream (server side).
    pub fn accept_request(&mut self) -> Option<RequestStream> {
        (!self.accept_queue.is_empty()).then(|| RequestStream {
            sid: self.accept_queue.remove(0),
        })
    }

    fn note_reset(&mut self, sid: u32) {
        if self.reset_sids.len() >= 64 {
            self.reset_sids.remove(0);
        }
        self.reset_sids.push(sid);
    }

    fn is_reset(&self, sid: u32) -> bool {
        self.reset_sids.contains(&sid)
    }

    /// Eager cancel (0.3.0): emit RST_STREAM with
    /// `error_code` (gRPC clients use CANCEL = 0x8) and
    /// drop local stream state. Subsequent writes for the
    /// sid are silent no-ops; the connection survives.
    pub fn send_rst_stream(&mut self, stream: &RequestStream, error_code: u32) {
        if self.is_reset(stream.sid) {
            return;
        }
        frame::encode_frame(
            TYPE_RST_STREAM,
            0,
            stream.sid,
            &error_code.to_be_bytes(),
            &mut self.out,
        );
        self.streams.remove(&stream.sid);
        self.note_reset(stream.sid);
    }

    /// Write a header section on `stream` (response
    /// headers from the server; a second call is the
    /// trailing section).
    pub fn write_headers(&mut self, stream: &RequestStream, headers: &[(String, Vec<u8>)]) {
        self.write_headers_flags(stream, headers, 0);
    }

    /// Write a trailing header section that also ends the
    /// stream (the gRPC trailers shape). Queued behind any
    /// buffered data — the trailer HEADERS goes out only
    /// after `send_buf` drains (window permitting), so the
    /// wire order is HEADERS, DATA*, trailer-HEADERS.
    pub fn write_trailers(&mut self, stream: &RequestStream, headers: &[(String, Vec<u8>)]) {
        if self.is_reset(stream.sid) {
            return;
        }
        let mut block = Vec::new();
        encode_header_block(headers, &mut block);
        let st = self.stream_entry(stream.sid);
        st.send_fin = true;
        if st.send_buf.is_empty() {
            st.fin_sent = true;
            frame::encode_frame(
                TYPE_HEADERS,
                FLAG_END_HEADERS | FLAG_END_STREAM,
                stream.sid,
                &block,
                &mut self.out,
            );
        } else {
            st.pending_trailers = Some(block);
        }
    }

    fn write_headers_flags(
        &mut self,
        stream: &RequestStream,
        headers: &[(String, Vec<u8>)],
        extra_flags: u8,
    ) {
        if self.is_reset(stream.sid) {
            return;
        }
        let mut block = Vec::new();
        encode_header_block(headers, &mut block);
        frame::encode_frame(
            TYPE_HEADERS,
            FLAG_END_HEADERS | extra_flags,
            stream.sid,
            &block,
            &mut self.out,
        );
    }

    /// Queue body bytes (sent within flow-control windows
    /// on subsequent pumps).
    pub fn write_data(&mut self, stream: &RequestStream, data: &[u8]) {
        if self.is_reset(stream.sid) {
            return;
        }
        let st = self.stream_entry(stream.sid);
        st.send_buf.extend_from_slice(data);
    }

    /// Half-close our direction once queued data drains.
    pub fn finish(&mut self, stream: &RequestStream) {
        if self.is_reset(stream.sid) {
            return;
        }
        let st = self.stream_entry(stream.sid);
        st.send_fin = true;
    }

    /// Leading header section, once (dispatch-once).
    pub fn read_headers(&mut self, stream: &RequestStream) -> Option<Vec<Header>> {
        let st = self.streams.get_mut(&stream.sid)?;
        if st.headers_dispatched {
            return None;
        }
        st.recv_headers.clone().inspect(|_| {
            st.headers_dispatched = true;
        })
    }

    /// Trailing header section, once.
    pub fn read_trailers(&mut self, stream: &RequestStream) -> Option<Vec<Header>> {
        let st = self.streams.get_mut(&stream.sid)?;
        if st.trailers_dispatched {
            return None;
        }
        st.recv_trailers.clone().inspect(|_| {
            st.trailers_dispatched = true;
        })
    }

    /// `true` once trailers arrived and weren't read yet.
    pub fn received_trailers_ready(&self, stream: &RequestStream) -> bool {
        self.streams
            .get(&stream.sid)
            .is_some_and(|st| !st.trailers_dispatched && st.recv_trailers.is_some())
    }

    /// `true` once leading headers arrived and weren't
    /// read yet.
    pub fn received_headers_ready(&self, stream: &RequestStream) -> bool {
        self.streams
            .get(&stream.sid)
            .is_some_and(|st| !st.headers_dispatched && st.recv_headers.is_some())
    }

    /// Drain buffered body bytes; returns `(n, end_seen)`.
    pub fn read_data(&mut self, stream: &RequestStream, out: &mut Vec<u8>) -> (usize, bool) {
        let Some(st) = self.streams.get_mut(&stream.sid) else {
            return (0, false);
        };
        let n = st.body.len();
        out.extend_from_slice(&st.body);
        st.body.clear();
        (n, st.end_seen)
    }

    /// Release stream state once the caller is done
    /// (soft-close: reaped after the send queue drains —
    /// the O(calls²) lesson from the H/3 side).
    pub fn close_stream(&mut self, stream: &RequestStream) {
        if let Some(st) = self.streams.get_mut(&stream.sid) {
            if st.send_buf.is_empty() {
                self.streams.remove(&stream.sid);
            } else {
                st.closing = true;
                self.reap_pending = true;
            }
        }
    }
}
