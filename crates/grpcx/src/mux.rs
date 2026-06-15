// The mux layer shared by `grpcx/ws-mux` and
// `grpcx/tcp-mux` (T7 shed the WebSocket shell off the
// v0.6 mux): frame grammar, header-list codec, per-stream
// receive routing, and the shell-agnostic unary/dispatch
// logic. Pure `no_std + alloc`.
//
// A mux frame is `stream-id(u32-BE) | type|flags(u8) |
// payload` — the payload of exactly one outer-shell unit
// (one WS binary message on ws-mux; one length-prefixed
// record on tcp-mux).

use crate::{
    MessageFrame, Metadata, RpcError, Server, Status, StatusCode, decode_all_frames, encode_frame,
    encode_response_headers, encode_trailers, parse_request_headers, parse_trailers,
};
use alloc::collections::BTreeMap;
use alloc::string::ToString;
use alloc::vec::Vec;

// ============================================================
// Mux frame codec (SPEC.md § grpcx/ws-mux frame grammar,
// shared verbatim by § grpcx/tcp-mux).
// ============================================================

/// Mux frame type: header section.
pub const MUX_HEADERS: u8 = 0x01;
/// Mux frame type: body bytes (gRPC length-prefixed
/// frames ride inside).
pub const MUX_DATA: u8 = 0x02;
/// Mux frame type: trailing header section.
pub const MUX_TRAILERS: u8 = 0x03;
/// Mux frame type: link-liveness probe (0.17, stream id 0
/// only). The receiver answers with [`MUX_PONG`]; both
/// carry an opaque payload echoed back.
pub const MUX_PING: u8 = 0x04;
/// Mux frame type: answer to [`MUX_PING`] (stream id 0).
pub const MUX_PONG: u8 = 0x05;
/// Mux frame type: cancel the stream (eager cancel,
/// 0.22) — the sender abandoned the call; the receiver
/// drops per-sid state and stops producing for it. Empty
/// payload.
pub const MUX_RST: u8 = 0x06;
/// Flag bit: this frame ends the sender's direction.
pub const MUX_END_STREAM: u8 = 0x80;

/// Encode one mux frame (`stream-id | type|flags |
/// payload`) — the payload of exactly one outer-shell
/// unit.
pub fn encode_mux_frame(stream_id: u32, ty: u8, end_stream: bool, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.extend_from_slice(&stream_id.to_be_bytes());
    out.push(ty | if end_stream { MUX_END_STREAM } else { 0 });
    out.extend_from_slice(payload);
    out
}

/// Decode one mux frame. Returns
/// `(stream_id, type, end_stream, payload)`.
///
/// # Errors
/// [`RpcError::Codec`] — message shorter than the 5-byte
/// frame header, stream id 0, or unknown frame type.
pub fn decode_mux_frame(msg: &[u8]) -> Result<(u32, u8, bool, &[u8]), RpcError> {
    if msg.len() < 5 {
        return Err(RpcError::Codec("mux: short frame"));
    }
    let sid = u32::from_be_bytes([msg[0], msg[1], msg[2], msg[3]]);
    let ty = msg[4] & 0x7F;
    let end = msg[4] & MUX_END_STREAM != 0;
    if matches!(ty, MUX_PING | MUX_PONG) {
        // Link-level control frames live on the reserved
        // stream id 0 (0.17).
        if sid != 0 {
            return Err(RpcError::Codec("mux: ping/pong on a stream id"));
        }
        return Ok((sid, ty, end, &msg[5..]));
    }
    if sid == 0 {
        return Err(RpcError::Codec("mux: stream id 0 is reserved"));
    }
    if !matches!(ty, MUX_HEADERS | MUX_DATA | MUX_TRAILERS | MUX_RST) {
        return Err(RpcError::Codec("mux: unknown frame type"));
    }
    Ok((sid, ty, end, &msg[5..]))
}

/// Serialize a header list: `u16-BE name-len, name,
/// u16-BE value-len, value` per entry.
///
/// # Panics
/// If any name or value exceeds 65 535 bytes (the wire
/// can't carry it; header values that large are a caller
/// bug).
pub fn encode_header_list(meta: &Metadata) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, value) in &meta.headers {
        let nl = u16::try_from(name.len()).expect("mux header name exceeds u16");
        let vl = u16::try_from(value.len()).expect("mux header value exceeds u16");
        out.extend_from_slice(&nl.to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&vl.to_be_bytes());
        out.extend_from_slice(value);
    }
    out
}

/// Parse a serialized header list.
///
/// # Errors
/// [`RpcError::Codec`] on truncated entries or non-UTF-8
/// header names.
pub fn decode_header_list(mut bytes: &[u8]) -> Result<Metadata, RpcError> {
    let mut headers = Vec::new();
    while !bytes.is_empty() {
        if bytes.len() < 2 {
            return Err(RpcError::Codec("mux: truncated header list"));
        }
        let nl = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        if bytes.len() < 2 + nl + 2 {
            return Err(RpcError::Codec("mux: truncated header list"));
        }
        let name = core::str::from_utf8(&bytes[2..2 + nl])
            .map_err(|_| RpcError::Codec("mux: non-UTF-8 header name"))?
            .to_string();
        let vl = u16::from_be_bytes([bytes[2 + nl], bytes[3 + nl]]) as usize;
        if bytes.len() < 4 + nl + vl {
            return Err(RpcError::Codec("mux: truncated header list"));
        }
        let value = bytes[4 + nl..4 + nl + vl].to_vec();
        headers.push((name, value));
        bytes = &bytes[4 + nl + vl..];
    }
    Ok(Metadata { headers })
}

// ============================================================
// Per-stream receive routing (both sides, both shells).
// ============================================================

/// Per-stream receive state.
#[derive(Default)]
pub(crate) struct RecvStream {
    pub(crate) leading: Option<Metadata>,
    pub(crate) body: Vec<u8>,
    pub(crate) trailers: Option<Metadata>,
    pub(crate) end_seen: bool,
}

/// Route one mux frame into per-stream state. A
/// link-level PING is answered: the mux-encoded PONG comes
/// back for the caller to wrap in its shell (length
/// prefix / WS frame) and send; a PONG is pure inbound
/// traffic (the liveness proof) and routes to nothing.
pub(crate) fn route_mux_frame(
    streams: &mut BTreeMap<u32, RecvStream>,
    msg: &[u8],
) -> Result<Option<Vec<u8>>, RpcError> {
    let (sid, ty, end, payload) = decode_mux_frame(msg)?;
    if ty == MUX_PING {
        return Ok(Some(encode_mux_frame(0, MUX_PONG, false, payload)));
    }
    if ty == MUX_PONG {
        return Ok(None);
    }
    if ty == MUX_RST {
        // Peer abandoned the call: drop the state; the
        // serve loop's reply paths skip sids with no
        // state. (A cancelled-before-dispatch request
        // never completes, so it never dispatches.)
        streams.remove(&sid);
        return Ok(None);
    }
    let st = streams.entry(sid).or_default();
    match ty {
        MUX_HEADERS => {
            let meta = decode_header_list(payload)?;
            if st.leading.is_none() {
                st.leading = Some(meta);
            } else if st.trailers.is_none() {
                // A second HEADERS section is the trailing
                // section (mirrors the H/3 shape).
                st.trailers = Some(meta);
            }
            // A third section is dropped (peer bug).
        }
        MUX_DATA => st.body.extend_from_slice(payload),
        MUX_TRAILERS => {
            let meta = decode_header_list(payload)?;
            if st.trailers.is_none() {
                st.trailers = Some(meta);
            }
        }
        _ => unreachable!("decode_mux_frame validated the type"),
    }
    if end {
        st.end_seen = true;
    }
    Ok(None)
}

// ============================================================
// Link liveness (0.17) — the shared mux-shell atom.
// ============================================================

/// Per-connection liveness state for the mux transports
/// (tcp-mux / ws-mux — both are L10-resident, so wall
/// clock is used directly). Same semantics as the L4/h2
/// atoms: any inbound bytes prove the link; quiet past the
/// keepalive interval sends a [`MUX_PING`]; no inbound
/// past the idle timeout is death.
pub(crate) struct LinkLiveness {
    keepalive: core::time::Duration,
    idle_timeout: core::time::Duration,
    started: Option<std::time::Instant>,
    last_inbound: Option<std::time::Instant>,
    last_ping: Option<std::time::Instant>,
    /// Previous check time (gap forgiveness — see `check`).
    last_check: Option<std::time::Instant>,
}

impl LinkLiveness {
    pub(crate) fn new() -> Self {
        LinkLiveness {
            keepalive: core::time::Duration::from_micros(2_500_000),
            idle_timeout: core::time::Duration::from_micros(10_000_000),
            started: None,
            last_inbound: None,
            last_ping: None,
            last_check: None,
        }
    }

    pub(crate) fn configure(
        &mut self,
        keepalive: core::time::Duration,
        idle_timeout: core::time::Duration,
    ) {
        self.keepalive = keepalive;
        self.idle_timeout = idle_timeout;
    }

    /// Inbound bytes observed — the liveness proof.
    pub(crate) fn on_inbound(&mut self) {
        self.last_inbound = Some(std::time::Instant::now());
    }

    /// Per-pump check. `Err` = the link is dead;
    /// `Ok(true)` = a keepalive PING is due now (`ready`
    /// gates pinging before the shell handshake is up —
    /// death still applies, bounding a handshake against a
    /// dead peer).
    pub(crate) fn check(&mut self, ready: bool, dead_msg: &'static str) -> Result<bool, RpcError> {
        let now = std::time::Instant::now();
        // Gap forgiveness: a check gap wider than the
        // keepalive interval means the driver wasn't
        // watching (a blocking caller between calls) —
        // re-anchor instead of firing a false death.
        if let Some(prev) = self.last_check
            && now.duration_since(prev) > self.keepalive
        {
            self.last_inbound = Some(now);
        }
        self.last_check = Some(now);
        let started = *self.started.get_or_insert(now);
        let anchor = self.last_inbound.map_or(started, |i| i.max(started));
        let quiet = now.duration_since(anchor);
        if quiet >= self.idle_timeout {
            return Err(RpcError::Transport(dead_msg));
        }
        if ready
            && quiet >= self.keepalive
            && self
                .last_ping
                .is_none_or(|p| now.duration_since(p) >= self.keepalive)
        {
            self.last_ping = Some(now);
            return Ok(true);
        }
        Ok(false)
    }
}

// ============================================================
// Client-side unary completion (shell-agnostic — mirrors
// the H/3 unary semantics including the trailers-only
// error fast path).
// ============================================================

/// Every response message + the trailing metadata of one
/// completed stream.
pub(crate) type CompletedCall = (Vec<Vec<u8>>, Metadata);

/// Check a stream for completion, returning EVERY
/// response message (the streaming shape). `poll_unary_mux`
/// wraps this for the 0-or-1-message case.
pub(crate) fn poll_completed_mux(
    streams: &mut BTreeMap<u32, RecvStream>,
    sid: u32,
) -> Result<Option<CompletedCall>, RpcError> {
    let Some(st) = streams.get(&sid) else {
        return Err(RpcError::Codec("mux: unknown stream"));
    };
    let Some(leading) = &st.leading else {
        return Ok(None);
    };

    // Trailers-only response.
    if leading.headers.iter().any(|(n, _)| n == "grpc-status") {
        let (status, trailing) = parse_trailers(leading)?;
        streams.remove(&sid);
        return if status.code == StatusCode::Ok {
            Ok(Some((Vec::new(), trailing)))
        } else {
            Err(RpcError::Status(status))
        };
    }
    if !leading
        .headers
        .iter()
        .any(|(n, v)| n == ":status" && v == b"200")
    {
        streams.remove(&sid);
        return Err(RpcError::Transport("mux: non-200 response"));
    }

    let Some(trailers) = &st.trailers else {
        return Ok(None);
    };
    let (status, trailing) = parse_trailers(trailers)?;
    let body = st.body.clone();
    streams.remove(&sid);
    if status.code != StatusCode::Ok {
        return Err(RpcError::Status(status));
    }
    let frames = decode_all_frames(&body)?;
    if frames.iter().any(|f| f.compressed) {
        return Err(RpcError::Codec("unexpected compressed response"));
    }
    Ok(Some((
        frames.into_iter().map(|f| f.payload).collect(),
        trailing,
    )))
}

#[cfg(feature = "transport-ws-mux")]
pub(crate) fn poll_unary_mux(
    streams: &mut BTreeMap<u32, RecvStream>,
    sid: u32,
) -> Result<Option<(Vec<u8>, Metadata)>, RpcError> {
    Ok(
        poll_completed_mux(streams, sid)?.map(|(mut msgs, trailing)| {
            let first = if msgs.is_empty() {
                Vec::new()
            } else {
                msgs.swap_remove(0)
            };
            (first, trailing)
        }),
    )
}

// ============================================================
// Server-side dispatch (shell-agnostic): a completed
// request stream in, the bare response mux frames out.
// The shell wraps each frame in its own unit (WS binary
// message / length-prefixed record).
// ============================================================

pub(crate) fn dispatch_one_mux(
    server: &Server,
    sid: u32,
    st: RecvStream,
) -> Result<Vec<Vec<u8>>, RpcError> {
    let leading = st.leading.expect("caller checked leading.is_some()");
    let (service, method, user_meta) = match parse_request_headers(&leading) {
        Ok(t) => t,
        Err(_) => {
            return Ok(respond_status_mux(
                sid,
                &status_of(StatusCode::InvalidArgument, "malformed request"),
            ));
        }
    };
    let frames = match decode_all_frames(&st.body) {
        Ok(f) => f,
        Err(_) => {
            return Ok(respond_status_mux(
                sid,
                &status_of(StatusCode::InvalidArgument, "bad frame"),
            ));
        }
    };
    if frames.iter().any(|f| f.compressed) {
        return Ok(respond_status_mux(
            sid,
            &status_of(StatusCode::Unimplemented, "compression not negotiated"),
        ));
    }
    let requests: Vec<Vec<u8>> = frames.into_iter().map(|f| f.payload).collect();

    let Some(svc) = server.find_service(&service) else {
        return Ok(respond_status_mux(
            sid,
            &status_of(StatusCode::Unimplemented, "unknown service"),
        ));
    };

    match crate::dispatch_streaming_deadline(svc, &method, &user_meta, &requests) {
        Ok((responses, trailing)) => {
            let mut out = Vec::with_capacity(responses.len() + 2);
            out.push(encode_mux_frame(
                sid,
                MUX_HEADERS,
                false,
                &encode_header_list(&encode_response_headers()),
            ));
            for resp in responses {
                let mut wire = Vec::new();
                encode_frame(
                    &MessageFrame {
                        compressed: false,
                        payload: resp,
                    },
                    &mut wire,
                );
                out.push(encode_mux_frame(sid, MUX_DATA, false, &wire));
            }
            out.push(encode_mux_frame(
                sid,
                MUX_TRAILERS,
                true,
                &encode_header_list(&encode_trailers(&Status::default(), &trailing)),
            ));
            Ok(out)
        }
        Err(RpcError::Status(status)) => Ok(respond_status_mux(sid, &status)),
        Err(_) => Ok(respond_status_mux(
            sid,
            &status_of(StatusCode::Internal, "service failure"),
        )),
    }
}

/// Trailers-only error response: one HEADERS|END_STREAM
/// section carrying `:status 200` + `grpc-status`.
pub(crate) fn respond_status_mux(sid: u32, status: &Status) -> Vec<Vec<u8>> {
    let mut hdrs = encode_response_headers();
    hdrs.headers
        .extend(encode_trailers(status, &Metadata::default()).headers);
    alloc::vec![encode_mux_frame(
        sid,
        MUX_HEADERS,
        true,
        &encode_header_list(&hdrs)
    )]
}

fn status_of(code: StatusCode, msg: &str) -> Status {
    Status {
        code,
        message: msg.to_string(),
    }
}
