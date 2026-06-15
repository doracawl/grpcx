#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Layout: the working public surface is [`Session`] — one
//! WebTransport session over a [`grpcx_h3::Connection`]. v0.1
//! ships the **shape** of every public item with
//! `unimplemented!()` bodies; each v0.x checkpoint replaces a
//! slice with real code.
//!
//! Design intent (per
//! `../../REFACTOR-v6-v0.1-bootstrap.md`):
//!
//! 1. WT-over-H/3 only; no H/2 fallback.
//! 2. One H/3 connection hosts N sessions; session id = the
//!    H/3 stream id of the CONNECT (RFC 9220 §3).
//! 3. Stream type byte = `0x54` (draft-ietf-webtrans-http3 §4).
//! 4. Datagrams use RFC 9297 H/3 datagrams with a leading
//!    session-id varint (draft §5).
//! 5. `SETTINGS_ENABLE_CONNECT_PROTOCOL = 1` is the
//!    precondition; L5's `Settings.enable_connect_protocol`
//!    already carries it.
//! 6. Stream handles are opaque; I/O routes through [`Session`].
//! 7. [`Session`] does **not** own the [`grpcx_h3::Connection`];
//!    every I/O call takes `&mut grpcx_h3::Connection`.
//! 8. Buffered datagrams are owned `Vec<u8>` for clarity;
//!    borrow-only form is a v1.x perf knob.
//! 9. No automatic re-tries / reconnect.
//! 10. `CLOSE_WEBTRANSPORT_SESSION` capsule (RFC 9220 §3.3) is
//!     the canonical graceful close.

extern crate alloc;
extern crate std;

#[doc(hidden)]
pub mod __fuzz_helpers;

mod capsule;
mod connect;
mod framing;
mod mux;
mod sizes;
mod varint;

pub use capsule::{
    CAPSULE_TYPE_CLOSE_WEBTRANSPORT_SESSION, Capsule, capsule_wire_len, decode_capsule,
    encode_close_capsule,
};
pub use connect::{
    build_connect_request_headers, build_connect_response_headers, parse_response_status,
    validate_connect_request,
};
pub use framing::{
    WT_STREAM_TYPE, read_wt_datagram, read_wt_stream_prefix, write_wt_datagram,
    write_wt_stream_prefix,
};
pub use mux::SessionMux;

use crate::varint::read_varint;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use grpcx_h3::RequestStream;
use grpcx_quic::StreamId;

// ============================================================
// Session.
// ============================================================

/// One WebTransport session. Multiplexes WT-typed streams +
/// session-tagged datagrams within an underlying
/// [`grpcx_h3::Connection`]. v0.3 ships the CONNECT bootstrap;
/// v0.4 lands the stream dispatch (bidi + uni); v0.5 adds
/// datagrams; v0.6 adds graceful close via the
/// `CLOSE_WEBTRANSPORT_SESSION` capsule + multi-session via
/// [`SessionMux`].
#[derive(Debug)]
pub struct Session {
    session_id: grpcx_quic::StreamId,
    bootstrap_stream: RequestStream,
    side: SessionSide,
    /// Client: flips `true` when `:status = 200` arrives.
    /// Server: flips `true` once we've sent the `200` response.
    ready: bool,
    /// 0.6.0+: capsule-stream input buffer on the bootstrap
    /// bidi (RFC 9297 §3). Accumulated post-CONNECT-headers
    /// DATA payload waiting on full capsule decode.
    capsule_in_buf: Vec<u8>,
    /// 0.6.0+: snapshot of the inbound `CLOSE_WEBTRANSPORT_SESSION`
    /// capsule (RFC 9220 §4.4), once received. Drives
    /// [`Session::is_closed`] / [`Session::close_info`].
    inbound_close: Option<(u32, Vec<u8>)>,
    /// 0.6.0+: `true` once this side has emitted (or received)
    /// the close capsule.
    closed: bool,
    /// Pending inbound datagram payloads classified for this
    /// session — populated by [`Session::poll`], drained by
    /// [`Session::recv_datagram`]. 0.5.0+: real field, no longer
    /// a placeholder.
    inbound_datagrams: Vec<Vec<u8>>,
    /// Peer-opened bidi sids that `poll` has classified as
    /// belonging to this session (matching `[0x54][session_id]`
    /// prefix). Drained by [`Session::accept_bi`].
    pending_peer_bi: Vec<u64>,
    /// Peer-opened uni sids that `poll` classified as belonging
    /// to this session. Drained by [`Session::accept_uni`].
    pending_peer_uni: Vec<u64>,
    /// Peer-opened bidis whose `[0x54][session_id]` prefix
    /// hasn't fully arrived yet. Keyed by sid; value is the
    /// accumulated prefix-region buffer. Once both varints
    /// decode, the sid moves to `pending_peer_bi` and any
    /// post-prefix bytes already in the buffer move to
    /// `unread_bi`.
    unclassified_bi: BTreeMap<u64, Vec<u8>>,
    /// Symmetric for peer-opened uni streams (L5 already peeled
    /// the `0x54` type byte, so this buffer is just the
    /// session-id varint + post-prefix bytes).
    unclassified_uni: BTreeMap<u64, Vec<u8>>,
    /// Post-prefix bytes captured during classification for
    /// bidi streams claimed by this session — drained by
    /// [`Session::read_stream`] before falling through to L4.
    unread_bi: BTreeMap<u64, Vec<u8>>,
    /// Symmetric for uni streams.
    unread_uni: BTreeMap<u64, Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionSide {
    Client,
    Server,
}

impl Session {
    /// Open a client-side session via extended CONNECT
    /// (RFC 9220 §3). Returns once the CONNECT request frame
    /// has been emitted on a new bidi stream;
    /// [`Session::is_ready`] flips `true` after
    /// [`Session::poll_open`] drives the server's
    /// `:status = 200` response in.
    pub fn open(
        h3: &mut grpcx_h3::Connection,
        authority: &[u8],
        path: &[u8],
    ) -> Result<Self, Error> {
        // RFC 9220 §3: client MUST refuse to issue extended
        // CONNECT unless the peer's SETTINGS advertise
        // `SETTINGS_ENABLE_CONNECT_PROTOCOL = 1`.
        match h3.peer_settings() {
            Some(s) if s.enable_connect_protocol => {}
            _ => return Err(Error::ConnectProtocolNotAdvertised),
        }
        let headers = connect::build_connect_request_headers(authority, path);
        let bootstrap_stream = h3.send_request(&headers)?;
        Ok(Self {
            session_id: bootstrap_stream.stream_id(),
            bootstrap_stream,
            side: SessionSide::Client,
            ready: false,
            inbound_datagrams: Vec::new(),
            pending_peer_bi: Vec::new(),
            pending_peer_uni: Vec::new(),
            unclassified_bi: BTreeMap::new(),
            unclassified_uni: BTreeMap::new(),
            unread_bi: BTreeMap::new(),
            unread_uni: BTreeMap::new(),
            capsule_in_buf: Vec::new(),
            inbound_close: None,
            closed: false,
        })
    }

    /// Drive the client-side bootstrap forward. After
    /// [`Session::open`] returned, call [`Session::poll_open`]
    /// once per pump cycle (until `is_ready()` flips) to
    /// consume the server's response. Returns `Ok(true)` when
    /// the session is now ready.
    pub fn poll_open(&mut self, h3: &mut grpcx_h3::Connection) -> Result<bool, Error> {
        if self.ready {
            return Ok(true);
        }
        if matches!(self.side, SessionSide::Server) {
            return Err(Error::Internal);
        }
        if let Some(response_headers) = h3.read_headers(&self.bootstrap_stream)? {
            let status = connect::parse_response_status(&response_headers)?;
            if status != 200 {
                return Err(Error::ConnectRejected(status));
            }
            self.ready = true;
        }
        Ok(self.ready)
    }

    /// Accept the next server-side session whose CONNECT has
    /// arrived. Returns `Ok(None)` when nothing is pending or
    /// the headers on the next pending bidi haven't decoded
    /// yet.
    ///
    /// Server's local `SETTINGS_ENABLE_CONNECT_PROTOCOL` must
    /// already be `true` (set on the [`grpcx_h3::Settings`]
    /// passed to [`grpcx_h3::Connection::new_server`]); WT
    /// CONNECT requests arriving without that advertised are a
    /// protocol violation per RFC 9220 §3.
    pub fn accept(h3: &mut grpcx_h3::Connection) -> Result<Option<Self>, Error> {
        let Some(req) = h3.accept_request()? else {
            return Ok(None);
        };
        // Try to read headers now; if not ready yet, return
        // `None` and the caller retries (we don't push the
        // request back — the caller wraps a thin retry loop).
        // For the v0.3 acceptance test that's fine since the
        // pump cycle eventually delivers HEADERS.
        let Some(headers) = h3.read_headers(&req)? else {
            // Stash by returning a session that's not ready
            // yet AND not classified — easier path is to
            // require callers to pump until headers are
            // present. Surface that constraint via Ok(None).
            return Ok(None);
        };
        let (_authority, _path) = connect::validate_connect_request(&headers)?;
        // Send the 200 OK back.
        let response_headers = connect::build_connect_response_headers();
        h3.write_headers(&req, &response_headers)?;
        Ok(Some(Self {
            session_id: req.stream_id(),
            bootstrap_stream: req,
            side: SessionSide::Server,
            ready: true,
            inbound_datagrams: Vec::new(),
            pending_peer_bi: Vec::new(),
            pending_peer_uni: Vec::new(),
            unclassified_bi: BTreeMap::new(),
            unclassified_uni: BTreeMap::new(),
            unread_bi: BTreeMap::new(),
            unread_uni: BTreeMap::new(),
            capsule_in_buf: Vec::new(),
            inbound_close: None,
            closed: false,
        }))
    }

    /// `true` once the bootstrap CONNECT has been answered
    /// (client side: server returned `:status = 200`; server
    /// side: we sent the `200` response).
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Open a new locally-initiated bidirectional WT stream on
    /// this session. Writes the `[0x54][session_id]` prefix on
    /// the underlying L4 bidi as the first bytes; the caller's
    /// subsequent [`Session::write_stream`] calls append after
    /// that prefix.
    pub fn open_bi(&mut self, h3: &mut grpcx_h3::Connection) -> Result<BidirectionalStream, Error> {
        let sid = h3.open_bi_raw();
        self.write_wt_prefix(h3, sid)?;
        Ok(BidirectionalStream { _sid: sid })
    }

    /// Open a new locally-initiated unidirectional WT stream
    /// (RFC 9220 §4). The `[0x54][session_id]` prefix is written
    /// at open time; the caller's user bytes follow via
    /// [`Session::write_stream`].
    pub fn open_uni(
        &mut self,
        h3: &mut grpcx_h3::Connection,
    ) -> Result<UnidirectionalStream, Error> {
        let sid = h3.open_uni_raw();
        self.write_wt_prefix(h3, sid)?;
        Ok(UnidirectionalStream { _sid: sid })
    }

    fn write_wt_prefix(&self, h3: &mut grpcx_h3::Connection, sid: StreamId) -> Result<(), Error> {
        let mut prefix = Vec::new();
        framing::write_wt_stream_prefix(self.session_id.0, &mut prefix)?;
        h3.write_stream_raw(sid, &prefix, false)
            .map_err(|e| Error::H3(grpcx_h3::Error::Quic(e)))?;
        Ok(())
    }

    /// Pop the next peer-initiated WT bidi stream that
    /// [`Session::poll`] has classified for this session, or
    /// `Ok(None)` if none are pending.
    pub fn accept_bi(&mut self) -> Result<Option<BidirectionalStream>, Error> {
        if self.pending_peer_bi.is_empty() {
            return Ok(None);
        }
        let sid = self.pending_peer_bi.remove(0);
        Ok(Some(BidirectionalStream {
            _sid: StreamId(sid),
        }))
    }

    /// Pop the next peer-initiated WT uni stream classified for
    /// this session, or `Ok(None)` if none are pending.
    pub fn accept_uni(&mut self) -> Result<Option<UnidirectionalStream>, Error> {
        if self.pending_peer_uni.is_empty() {
            return Ok(None);
        }
        let sid = self.pending_peer_uni.remove(0);
        Ok(Some(UnidirectionalStream {
            _sid: StreamId(sid),
        }))
    }

    /// Drive cross-session stream classification: drain L5's
    /// unknown-stream queues, peek `[0x54][session_id]` on each
    /// new candidate, route any matching this session into
    /// [`Session::accept_bi`] / [`Session::accept_uni`].
    ///
    /// Call once per pump cycle on the server side (and on the
    /// client side if expecting server-initiated WT streams).
    /// Idempotent; safe to call when nothing is pending.
    ///
    /// v0.4: single-session deployment. A peer-opened stream
    /// whose session-id doesn't match this session's id reports
    /// [`Error::Internal`] (v0.6 introduces multi-session
    /// dispatch).
    pub fn poll(&mut self, h3: &mut grpcx_h3::Connection) -> Result<(), Error> {
        // Drain L5's queues — these come `pre-typed` for uni
        // (L5 already consumed the `0x54` byte) and untyped for
        // bidi (L5 has no way to classify a bidi's first byte
        // generically). For uni, ignore any reserved type other
        // than 0x54 (v0.4 doesn't know what to do with them; an
        // L7+ owner can claim before us in v1.x).
        while let Some((sid, type_code)) = h3.accept_unknown_uni_stream() {
            if type_code == WT_STREAM_TYPE {
                self.unclassified_uni.entry(sid.0).or_default();
            }
            // Else: silently drop the stream (we never read it).
            // RFC 9114 §6.2.3 forward-compat — non-fatal.
        }
        while let Some(sid) = h3.accept_unknown_bi_stream() {
            self.unclassified_bi.entry(sid.0).or_default();
        }

        // Try to fully classify each unclassified stream.
        let uni_sids: Vec<u64> = self.unclassified_uni.keys().copied().collect();
        for sid in uni_sids {
            let mut new_bytes = Vec::new();
            h3.read_stream_raw(StreamId(sid), &mut new_bytes);
            let buf = self
                .unclassified_uni
                .get_mut(&sid)
                .expect("just-walked sid");
            buf.extend_from_slice(&new_bytes);
            // L5 already peeled the `0x54` type byte; the buffer
            // starts with the session-id varint.
            let mut cursor: &[u8] = buf;
            let parsed = read_varint(&mut cursor);
            if let Ok(session_id) = parsed {
                let consumed = buf.len() - cursor.len();
                if session_id != self.session_id.0 {
                    return Err(Error::Internal);
                }
                let remainder: Vec<u8> = buf[consumed..].to_vec();
                self.unclassified_uni.remove(&sid);
                self.pending_peer_uni.push(sid);
                if !remainder.is_empty() {
                    self.unread_uni.insert(sid, remainder);
                }
            }
            // else: incomplete prefix, wait for more bytes next
            // poll cycle.
        }

        let bi_sids: Vec<u64> = self.unclassified_bi.keys().copied().collect();
        for sid in bi_sids {
            let mut new_bytes = Vec::new();
            h3.read_stream_raw(StreamId(sid), &mut new_bytes);
            let buf = self.unclassified_bi.get_mut(&sid).expect("just-walked sid");
            buf.extend_from_slice(&new_bytes);
            // Bidi: the buffer starts with the full
            // `[0x54 varint][session_id varint]` prefix.
            let mut cursor: &[u8] = buf;
            let type_ = match read_varint(&mut cursor) {
                Ok(v) => v,
                // Truncated — wait for more bytes.
                Err(_) => continue,
            };
            if type_ != WT_STREAM_TYPE {
                return Err(Error::BadStreamFrame);
            }
            let session_id = match read_varint(&mut cursor) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if session_id != self.session_id.0 {
                return Err(Error::Internal);
            }
            let consumed = buf.len() - cursor.len();
            let remainder: Vec<u8> = buf[consumed..].to_vec();
            self.unclassified_bi.remove(&sid);
            self.pending_peer_bi.push(sid);
            if !remainder.is_empty() {
                self.unread_bi.insert(sid, remainder);
            }
        }

        // 0.5.0+: drain L5's H/3 datagram queue and route by
        // session id (RFC 9297 + RFC 9220 §5).
        while let Some((stream_id, payload)) = h3.recv_h3_datagram() {
            if stream_id.0 != self.session_id.0 {
                return Err(Error::Internal);
            }
            self.inbound_datagrams.push(payload);
        }

        self.poll_bootstrap_capsules(h3)?;

        Ok(())
    }

    /// 0.6.0+: drain inbound capsule bytes off the bootstrap
    /// bidi (RFC 9297 §3.2 + RFC 9220 §4.4). Post-CONNECT-
    /// headers payload arrives as H/3 DATA frames; L5's
    /// `read_data` returns the post-frame body. Concatenate and
    /// parse capsules out one at a time. Shared by
    /// [`Session::poll`] (single-session mode) and
    /// [`SessionMux::poll`] (multi-session mode).
    pub(crate) fn poll_bootstrap_capsules(
        &mut self,
        h3: &mut grpcx_h3::Connection,
    ) -> Result<(), Error> {
        let mut new_data = Vec::new();
        let _ = h3.read_data(&self.bootstrap_stream, &mut new_data)?;
        if !new_data.is_empty() {
            self.capsule_in_buf.extend_from_slice(&new_data);
        }
        loop {
            let mut cursor: &[u8] = &self.capsule_in_buf;
            match capsule::decode_capsule(&mut cursor)? {
                Some(cap) => {
                    let consumed = self.capsule_in_buf.len() - cursor.len();
                    self.capsule_in_buf.drain(..consumed);
                    match cap {
                        capsule::Capsule::Close { code, reason } => {
                            if self.inbound_close.is_none() {
                                self.inbound_close = Some((code, reason));
                                self.closed = true;
                            }
                        }
                        capsule::Capsule::Unknown { .. } => {
                            // RFC 9297 §3.3 — unknown capsules are
                            // forward-compat skip; nothing to do.
                        }
                    }
                }
                None => break,
            }
        }
        Ok(())
    }

    /// 0.6.0+: hand a classified peer-opened **uni** stream to
    /// this session. Called by [`SessionMux::poll`] after it has
    /// peeled the WT prefix. The `remainder` is any post-prefix
    /// bytes that arrived in the same QUIC frame.
    pub(crate) fn route_peer_uni(&mut self, sid: u64, remainder: Vec<u8>) {
        self.pending_peer_uni.push(sid);
        if !remainder.is_empty() {
            self.unread_uni.insert(sid, remainder);
        }
    }

    /// 0.6.0+: symmetric for bidi.
    pub(crate) fn route_peer_bi(&mut self, sid: u64, remainder: Vec<u8>) {
        self.pending_peer_bi.push(sid);
        if !remainder.is_empty() {
            self.unread_bi.insert(sid, remainder);
        }
    }

    /// 0.6.0+: deposit one inbound datagram already routed by
    /// session id.
    pub(crate) fn deposit_datagram(&mut self, payload: Vec<u8>) {
        self.inbound_datagrams.push(payload);
    }

    /// Append `data` to the WT stream identified by `sid` on the
    /// underlying L4 connection. `fin` closes the stream
    /// (write-side) when true. Returns the bytes accepted (always
    /// `data.len()` in v0.4 — no flow control).
    pub fn write_stream(
        &mut self,
        h3: &mut grpcx_h3::Connection,
        sid: StreamId,
        data: &[u8],
        fin: bool,
    ) -> Result<usize, Error> {
        h3.write_stream_raw(sid, data, fin)
            .map_err(|e| Error::H3(grpcx_h3::Error::Quic(e)))
    }

    /// Drain bytes from the WT stream `sid` into `out`. Returns
    /// `(bytes_read, fin)`. Drains any prefix-stripped bytes
    /// captured during [`Session::poll`] before falling through
    /// to fresh L4 bytes.
    pub fn read_stream(
        &mut self,
        h3: &mut grpcx_h3::Connection,
        sid: StreamId,
        out: &mut Vec<u8>,
    ) -> (usize, bool) {
        let start = out.len();
        if let Some(buf) = self.unread_bi.remove(&sid.0) {
            out.extend_from_slice(&buf);
        }
        if let Some(buf) = self.unread_uni.remove(&sid.0) {
            out.extend_from_slice(&buf);
        }
        let (_, fin) = h3.read_stream_raw(sid, out);
        (out.len() - start, fin)
    }

    /// Send one unreliable datagram on this session
    /// (RFC 9220 §5 over RFC 9297). Wraps L5's
    /// `send_h3_datagram`, which writes a quarter-session-id
    /// varint prefix in front of `data` over one L4 DATAGRAM
    /// frame.
    pub fn send_datagram(
        &mut self,
        h3: &mut grpcx_h3::Connection,
        data: &[u8],
    ) -> Result<(), Error> {
        h3.send_h3_datagram(self.session_id, data)?;
        Ok(())
    }

    /// Pop the next inbound datagram payload classified for this
    /// session by [`Session::poll`]. Returns `None` if the queue
    /// is empty.
    pub fn recv_datagram(&mut self) -> Option<Vec<u8>> {
        if self.inbound_datagrams.is_empty() {
            None
        } else {
            Some(self.inbound_datagrams.remove(0))
        }
    }

    /// `true` once either side has emitted a
    /// `CLOSE_WEBTRANSPORT_SESSION` capsule and we've seen / sent
    /// it. 0.6.0+ addition.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Snapshot of the inbound `CLOSE_WEBTRANSPORT_SESSION`
    /// capsule (`code` + `reason`), or `None` if the peer hasn't
    /// closed yet. 0.6.0+ addition.
    pub fn close_info(&self) -> Option<(u32, &[u8])> {
        self.inbound_close.as_ref().map(|(c, r)| (*c, r.as_slice()))
    }

    /// Close the session — emits a
    /// `CLOSE_WEBTRANSPORT_SESSION` capsule (RFC 9220 §4.4) on
    /// the bootstrap bidi, then finishes the write side. The
    /// session is marked closed locally so subsequent
    /// [`Session::is_closed`] returns `true`.
    ///
    /// `reason` is bounded to 1024 bytes (RFC 9220 §5.3);
    /// oversize returns `Error::BadStreamFrame`. Calling
    /// `close` twice is a no-op the second time.
    pub fn close(
        &mut self,
        h3: &mut grpcx_h3::Connection,
        code: u32,
        reason: &[u8],
    ) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        let mut wire = Vec::new();
        encode_close_capsule(code, reason, &mut wire)?;
        h3.write_data(&self.bootstrap_stream, &wire)?;
        h3.finish(&self.bootstrap_stream)?;
        self.closed = true;
        Ok(())
    }

    /// Session id — the H/3 request stream id of the CONNECT.
    pub fn session_id(&self) -> grpcx_quic::StreamId {
        self.session_id
    }
}

// ============================================================
// Stream handles.
// ============================================================

/// One bidirectional WebTransport stream handle. I/O goes
/// through the owning [`Session`] (and `grpcx_h3::Connection`)
/// — this type is just a typed reference.
#[derive(Debug, Clone, Copy)]
pub struct BidirectionalStream {
    _sid: grpcx_quic::StreamId,
}

impl BidirectionalStream {
    /// The L4 stream id backing this WT stream.
    pub fn stream_id(&self) -> grpcx_quic::StreamId {
        self._sid
    }
}

/// One unidirectional WebTransport stream handle.
#[derive(Debug, Clone, Copy)]
pub struct UnidirectionalStream {
    _sid: grpcx_quic::StreamId,
}

impl UnidirectionalStream {
    /// The L4 stream id backing this WT stream.
    pub fn stream_id(&self) -> grpcx_quic::StreamId {
        self._sid
    }
}

// ============================================================
// Error.
// ============================================================

/// Failure mode reported by any L6 op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Underlying H/3 error.
    H3(grpcx_h3::Error),
    /// Peer doesn't advertise
    /// `SETTINGS_ENABLE_CONNECT_PROTOCOL = 1`.
    ConnectProtocolNotAdvertised,
    /// CONNECT response carried a `:status` other than `200`.
    ConnectRejected(u16),
    /// Malformed WT stream framing (bad type byte / session-id
    /// varint).
    BadStreamFrame,
    /// Malformed datagram framing (no session-id prefix).
    BadDatagram,
    /// Session already closed.
    SessionClosed,
    /// Implementation-specific failure.
    Internal,
}

impl From<grpcx_h3::Error> for Error {
    fn from(e: grpcx_h3::Error) -> Self {
        Error::H3(e)
    }
}
