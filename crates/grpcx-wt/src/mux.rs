//! Multi-session dispatch — one [`SessionMux`] coordinates N
//! [`Session`]s sharing the same [`grpcx_h3::Connection`].
//!
//! A WebTransport client / server typically opens / accepts a
//! single session per H/3 connection, in which case
//! [`Session::poll`] (single-session) is enough. When a host
//! multiplexes multiple WT sessions on one H/3 connection — for
//! example a server holding many clients' sessions — the mux
//! routes inbound streams + datagrams by session id (the
//! `[0x54][session_id]` prefix L6 v0.2 settled).
//!
//! Streams / datagrams whose session id doesn't match any
//! registered session are silently dropped (RFC 9220 §4 forward-
//! compat). Lost streams in this single direction are a v1.x
//! observability knob; v0.6 keeps the routing simple.

use crate::framing::WT_STREAM_TYPE;
use crate::varint::read_varint;
use crate::{Error, Session};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use grpcx_quic::StreamId;

/// Central dispatcher that owns N WebTransport sessions sharing
/// one [`grpcx_h3::Connection`]. Streams + datagrams flow in via
/// [`SessionMux::poll`]; the caller drives outbound traffic via
/// the underlying [`Session`] handles obtained from
/// [`SessionMux::get_mut`].
///
/// v0.6 design notes:
/// - The mux does not own the H/3 connection — every I/O call
///   takes `&mut grpcx_h3::Connection`.
/// - A session id whose H/3 stream id is unknown to the mux gets
///   its stream / datagram dropped silently. A future v1.x
///   release adds an "unknown_session_observer" hook.
#[derive(Debug, Default)]
pub struct SessionMux {
    sessions: BTreeMap<u64, Session>,
    /// Streams (uni) whose `[session_id]` varint hasn't fully
    /// arrived yet. Once decoded, the sid + remainder are handed
    /// off to the matching session.
    pending_uni: BTreeMap<u64, Vec<u8>>,
    /// Streams (bi) whose `[0x54][session_id]` prefix is in
    /// flight.
    pending_bi: BTreeMap<u64, Vec<u8>>,
}

impl SessionMux {
    /// Create a fresh empty mux.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a session to the mux, keyed by its session id (the
    /// H/3 stream id of its CONNECT bootstrap bidi).
    pub fn insert(&mut self, session: Session) {
        self.sessions.insert(session.session_id().0, session);
    }

    /// Look up a registered session by id.
    pub fn get(&self, session_id: u64) -> Option<&Session> {
        self.sessions.get(&session_id)
    }

    /// Mutable lookup — the caller uses this to invoke
    /// per-session I/O ([`Session::open_bi`] / `accept_bi` /
    /// `send_datagram` / etc.).
    pub fn get_mut(&mut self, session_id: u64) -> Option<&mut Session> {
        self.sessions.get_mut(&session_id)
    }

    /// Remove a session from the mux (e.g. once `is_closed()`
    /// is observed). Returns the removed session if present.
    pub fn remove(&mut self, session_id: u64) -> Option<Session> {
        self.sessions.remove(&session_id)
    }

    /// Iterate over all registered sessions.
    pub fn iter(&self) -> impl Iterator<Item = (&u64, &Session)> {
        self.sessions.iter()
    }

    /// Number of registered sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// `true` when no sessions are registered.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Drive cross-session classification: drain L5's
    /// unknown-stream + H/3 datagram queues, peek prefixes,
    /// route to matching sessions. Also drives the per-session
    /// bootstrap-bidi capsule drain.
    ///
    /// Call once per pump cycle on the side hosting the mux.
    /// Unknown session ids on the wire are dropped silently —
    /// see the type-level note.
    pub fn poll(&mut self, h3: &mut grpcx_h3::Connection) -> Result<(), Error> {
        // 1. Pull all newly-arrived unknown uni / bi sids out of
        //    L5 and stage them on the mux's pending buffers.
        while let Some((sid, type_code)) = h3.accept_unknown_uni_stream() {
            if type_code == WT_STREAM_TYPE {
                self.pending_uni.entry(sid.0).or_default();
            }
        }
        while let Some(sid) = h3.accept_unknown_bi_stream() {
            self.pending_bi.entry(sid.0).or_default();
        }

        // 2. Classify pending uni streams (L5 already peeled
        //    the `0x54` byte — buffer starts with the session-id
        //    varint).
        let uni_sids: Vec<u64> = self.pending_uni.keys().copied().collect();
        for sid in uni_sids {
            let mut new_bytes = Vec::new();
            h3.read_stream_raw(StreamId(sid), &mut new_bytes);
            let buf = self.pending_uni.get_mut(&sid).expect("just-walked sid");
            buf.extend_from_slice(&new_bytes);
            let mut cursor: &[u8] = buf;
            let Ok(session_id) = read_varint(&mut cursor) else {
                continue;
            };
            let consumed = buf.len() - cursor.len();
            let remainder: Vec<u8> = buf[consumed..].to_vec();
            self.pending_uni.remove(&sid);
            if let Some(session) = self.sessions.get_mut(&session_id) {
                session.route_peer_uni(sid, remainder);
            }
            // Else: silent drop (unknown session id).
        }

        // 3. Classify pending bidi streams (the buffer carries
        //    the full `[0x54][session_id]` prefix).
        let bi_sids: Vec<u64> = self.pending_bi.keys().copied().collect();
        for sid in bi_sids {
            let mut new_bytes = Vec::new();
            h3.read_stream_raw(StreamId(sid), &mut new_bytes);
            let buf = self.pending_bi.get_mut(&sid).expect("just-walked sid");
            buf.extend_from_slice(&new_bytes);
            let mut cursor: &[u8] = buf;
            let Ok(type_) = read_varint(&mut cursor) else {
                continue;
            };
            if type_ != WT_STREAM_TYPE {
                return Err(Error::BadStreamFrame);
            }
            let Ok(session_id) = read_varint(&mut cursor) else {
                continue;
            };
            let consumed = buf.len() - cursor.len();
            let remainder: Vec<u8> = buf[consumed..].to_vec();
            self.pending_bi.remove(&sid);
            if let Some(session) = self.sessions.get_mut(&session_id) {
                session.route_peer_bi(sid, remainder);
            }
        }

        // 4. Route H/3 datagrams by stream id.
        while let Some((stream_id, payload)) = h3.recv_h3_datagram() {
            if let Some(session) = self.sessions.get_mut(&stream_id.0) {
                session.deposit_datagram(payload);
            }
        }

        // 5. Drive each session's bootstrap-bidi capsule drain.
        for session in self.sessions.values_mut() {
            session.poll_bootstrap_capsules(h3)?;
        }

        Ok(())
    }
}
