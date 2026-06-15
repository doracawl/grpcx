// gRPC ↔ HTTP/3 header + trailer mapping — SPEC.md
// § "Headers" / § "Trailers".
//
// Header bags are modelled as `Metadata` (ordered
// `(name, value-bytes)` pairs); the QPACK byte encoding
// belongs to L5 (`grpcx-h3`) and is wired at v0.3. This
// module owns the *semantic* mapping:
//
//   request:  :method POST, :scheme https, :path
//             /pkg.Svc/Method, :authority, content-type,
//             te: trailers, user metadata passthrough
//   response: :status 200, content-type
//   trailers: grpc-status, grpc-message (percent-encoded),
//             user trailing metadata passthrough
//
// Header names are already lowercase on the HTTP/3 wire;
// parsers here match exact lowercase names.

use crate::{Metadata, RpcError, Status, StatusCode};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// `content-type` value this crate produces. The parser
/// also accepts the bare `application/grpc` and any
/// `application/grpc+<suffix>` form per spec.
pub const CONTENT_TYPE_GRPC: &str = "application/grpc+proto";

/// `user-agent` this crate sends.
pub const USER_AGENT: &str = concat!("grpcx/", env!("CARGO_PKG_VERSION"));

// ============================================================
// Request headers.
// ============================================================

/// Build the request header list for a call to
/// `service`/`method` at `authority`.
///
/// Pseudo-headers come first (HTTP/3 requirement), then
/// the fixed gRPC headers, then the caller's `metadata`
/// passthrough. Caller metadata MUST NOT contain
/// pseudo-headers or the reserved `grpc-*` names — the
/// call layer enforces that at v0.3; this builder
/// transports what it is given.
pub fn encode_request_headers(
    service: &str,
    method: &str,
    authority: &str,
    metadata: &Metadata,
) -> Metadata {
    let mut path = String::with_capacity(service.len() + method.len() + 2);
    path.push('/');
    path.push_str(service);
    path.push('/');
    path.push_str(method);

    let mut headers: Vec<(String, Vec<u8>)> = Vec::with_capacity(7 + metadata.headers.len());
    headers.push((":method".to_string(), b"POST".to_vec()));
    headers.push((":scheme".to_string(), b"https".to_vec()));
    headers.push((":path".to_string(), path.into_bytes()));
    headers.push((":authority".to_string(), authority.as_bytes().to_vec()));
    headers.push((
        "content-type".to_string(),
        CONTENT_TYPE_GRPC.as_bytes().to_vec(),
    ));
    headers.push(("te".to_string(), b"trailers".to_vec()));
    headers.push(("user-agent".to_string(), USER_AGENT.as_bytes().to_vec()));
    headers.extend(metadata.headers.iter().cloned());
    Metadata { headers }
}

/// Parse an incoming request header list into
/// `(service, method, user metadata)`.
///
/// # Errors
/// [`RpcError::Codec`] when:
/// - `:method` ≠ `POST`,
/// - `content-type` is missing or not
///   `application/grpc[+suffix]`,
/// - `:path` is missing or not of the form
///   `/<service>/<method>` with non-empty segments.
pub fn parse_request_headers(headers: &Metadata) -> Result<(String, String, Metadata), RpcError> {
    let mut path: Option<&[u8]> = None;
    let mut method_ok = false;
    let mut content_type_ok = false;
    let mut user = Vec::new();

    for (name, value) in &headers.headers {
        match name.as_str() {
            ":method" => method_ok = value == b"POST",
            ":path" => path = Some(value),
            ":scheme" | ":authority" => {}
            "content-type" => {
                content_type_ok = value.starts_with(b"application/grpc")
                    && (value.len() == 16 || value.get(16) == Some(&b'+'));
            }
            "te" | "user-agent" => {}
            _ => user.push((name.clone(), value.clone())),
        }
    }

    if !method_ok {
        return Err(RpcError::Codec("gRPC request :method must be POST"));
    }
    if !content_type_ok {
        return Err(RpcError::Codec("missing or non-gRPC content-type"));
    }
    let path = path.ok_or(RpcError::Codec("missing :path"))?;
    let path = core::str::from_utf8(path).map_err(|_| RpcError::Codec("non-UTF-8 :path"))?;

    // "/<service>/<method>" — exactly two non-empty
    // segments after the leading slash.
    let rest = path
        .strip_prefix('/')
        .ok_or(RpcError::Codec(":path must start with '/'"))?;
    let (service, method) = rest
        .split_once('/')
        .ok_or(RpcError::Codec(":path must be /service/method"))?;
    if service.is_empty() || method.is_empty() || method.contains('/') {
        return Err(RpcError::Codec(":path must be /service/method"));
    }

    Ok((
        service.to_string(),
        method.to_string(),
        Metadata { headers: user },
    ))
}

// ============================================================
// Response headers + trailers.
// ============================================================

/// Build the response header list (sent before the first
/// response message).
pub fn encode_response_headers() -> Metadata {
    Metadata {
        headers: alloc::vec![
            (":status".to_string(), b"200".to_vec()),
            (
                "content-type".to_string(),
                CONTENT_TYPE_GRPC.as_bytes().to_vec()
            ),
        ],
    }
}

/// Build the trailer list carrying the final RPC status
/// plus the application's trailing metadata.
///
/// `grpc-message` is omitted when empty; otherwise it is
/// percent-encoded per SPEC.md.
pub fn encode_trailers(status: &Status, trailing: &Metadata) -> Metadata {
    let mut headers: Vec<(String, Vec<u8>)> = Vec::with_capacity(2 + trailing.headers.len());
    let code = status.code as i32;
    headers.push(("grpc-status".to_string(), int_to_bytes(code)));
    if !status.message.is_empty() {
        headers.push((
            "grpc-message".to_string(),
            percent_encode(status.message.as_bytes()),
        ));
    }
    headers.extend(trailing.headers.iter().cloned());
    Metadata { headers }
}

/// Parse a trailer list into the final [`Status`] plus
/// the application's trailing metadata.
///
/// # Errors
/// [`RpcError::Codec`] when `grpc-status` is missing or
/// not a decimal `0..=16`.
pub fn parse_trailers(trailers: &Metadata) -> Result<(Status, Metadata), RpcError> {
    let mut code: Option<StatusCode> = None;
    let mut message = String::new();
    let mut user = Vec::new();

    for (name, value) in &trailers.headers {
        match name.as_str() {
            "grpc-status" => {
                let s = core::str::from_utf8(value)
                    .map_err(|_| RpcError::Codec("non-UTF-8 grpc-status"))?;
                let n: i32 = s
                    .parse()
                    .map_err(|_| RpcError::Codec("non-numeric grpc-status"))?;
                code = Some(
                    StatusCode::from_i32(n).ok_or(RpcError::Codec("grpc-status out of range"))?,
                );
            }
            "grpc-message" => message = percent_decode_lossy(value),
            _ => user.push((name.clone(), value.clone())),
        }
    }

    let code = code.ok_or(RpcError::Codec("missing grpc-status trailer"))?;
    Ok((Status { code, message }, Metadata { headers: user }))
}

// ============================================================
// grpc-timeout (gRPC spec "Timeout" header).
// ============================================================

/// Unit suffix of a `grpc-timeout` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutUnit {
    /// `H` — hours.
    Hours,
    /// `M` — minutes.
    Minutes,
    /// `S` — seconds.
    Seconds,
    /// `m` — milliseconds.
    Millis,
    /// `u` — microseconds.
    Micros,
    /// `n` — nanoseconds.
    Nanos,
}

impl TimeoutUnit {
    fn suffix(self) -> u8 {
        match self {
            TimeoutUnit::Hours => b'H',
            TimeoutUnit::Minutes => b'M',
            TimeoutUnit::Seconds => b'S',
            TimeoutUnit::Millis => b'm',
            TimeoutUnit::Micros => b'u',
            TimeoutUnit::Nanos => b'n',
        }
    }

    fn from_suffix(b: u8) -> Option<Self> {
        Some(match b {
            b'H' => TimeoutUnit::Hours,
            b'M' => TimeoutUnit::Minutes,
            b'S' => TimeoutUnit::Seconds,
            b'm' => TimeoutUnit::Millis,
            b'u' => TimeoutUnit::Micros,
            b'n' => TimeoutUnit::Nanos,
            _ => return None,
        })
    }
}

/// A typed `grpc-timeout` header value: at most 8 decimal
/// digits plus a unit suffix (gRPC spec "Timeout").
///
/// Carried per-call through request [`Metadata`] (the
/// header builders pass user metadata through verbatim):
///
/// ```
/// use grpcx::{GrpcTimeout, Metadata, TimeoutUnit};
/// let timeout = GrpcTimeout { value: 5000, unit: TimeoutUnit::Millis };
/// let meta = Metadata { headers: vec![timeout.to_header()] };
/// # let _ = meta;
/// ```
///
/// Deadline *enforcement* (cancelling expired calls)
/// needs a time source and lands with the IO runtime
/// (v0.6+); this type owns the wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrpcTimeout {
    /// Timeout magnitude — at most 8 decimal digits
    /// (0..=99_999_999) per spec.
    pub value: u32,
    /// Magnitude unit.
    pub unit: TimeoutUnit,
}

impl GrpcTimeout {
    /// Render as the `("grpc-timeout", value)` metadata
    /// entry.
    ///
    /// # Panics
    /// If `value` exceeds 8 decimal digits (99 999 999) —
    /// unrepresentable per spec; pick a coarser unit.
    pub fn to_header(self) -> (String, Vec<u8>) {
        assert!(
            self.value <= 99_999_999,
            "grpc-timeout magnitude exceeds 8 decimal digits; use a coarser unit"
        );
        let mut buf = [0u8; 9];
        let mut n = self.value;
        let mut i = 8;
        buf[i] = self.unit.suffix();
        loop {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        ("grpc-timeout".to_string(), buf[i..].to_vec())
    }

    /// The finest spec-representable encoding of a
    /// [`Duration`](core::time::Duration) (8-digit cap per
    /// unit; saturates at 99 999 999 hours).
    pub fn from_duration(d: core::time::Duration) -> Self {
        const MAX: u128 = 99_999_999;
        let nanos = d.as_nanos();
        if nanos <= MAX {
            return GrpcTimeout {
                value: nanos as u32,
                unit: TimeoutUnit::Nanos,
            };
        }
        let micros = d.as_micros();
        if micros <= MAX {
            return GrpcTimeout {
                value: micros as u32,
                unit: TimeoutUnit::Micros,
            };
        }
        let millis = d.as_millis();
        if millis <= MAX {
            return GrpcTimeout {
                value: millis as u32,
                unit: TimeoutUnit::Millis,
            };
        }
        let secs = d.as_secs() as u128;
        if secs <= MAX {
            return GrpcTimeout {
                value: secs as u32,
                unit: TimeoutUnit::Seconds,
            };
        }
        let mins = secs / 60;
        if mins <= MAX {
            return GrpcTimeout {
                value: mins as u32,
                unit: TimeoutUnit::Minutes,
            };
        }
        GrpcTimeout {
            value: (mins / 60).min(MAX) as u32,
            unit: TimeoutUnit::Hours,
        }
    }

    /// This timeout as a [`Duration`](core::time::Duration).
    pub fn as_duration(self) -> core::time::Duration {
        let v = self.value as u64;
        match self.unit {
            TimeoutUnit::Nanos => core::time::Duration::from_nanos(v),
            TimeoutUnit::Micros => core::time::Duration::from_micros(v),
            TimeoutUnit::Millis => core::time::Duration::from_millis(v),
            TimeoutUnit::Seconds => core::time::Duration::from_secs(v),
            TimeoutUnit::Minutes => core::time::Duration::from_secs(v * 60),
            TimeoutUnit::Hours => core::time::Duration::from_secs(v * 3600),
        }
    }

    /// Parse a `grpc-timeout` header value.
    /// Returns `None` for anything outside the spec shape
    /// (1..=8 digits + valid unit suffix).
    pub fn parse(value: &[u8]) -> Option<Self> {
        let (digits, suffix) = value.split_at(value.len().checked_sub(1)?);
        if digits.is_empty() || digits.len() > 8 {
            return None;
        }
        let unit = TimeoutUnit::from_suffix(suffix[0])?;
        let mut n: u32 = 0;
        for &d in digits {
            if !d.is_ascii_digit() {
                return None;
            }
            n = n * 10 + u32::from(d - b'0');
        }
        Some(GrpcTimeout { value: n, unit })
    }

    /// Extract the timeout from a metadata bag (the first
    /// `grpc-timeout` entry that parses).
    pub fn from_metadata(meta: &Metadata) -> Option<Self> {
        meta.headers
            .iter()
            .find(|(n, _)| n == "grpc-timeout")
            .and_then(|(_, v)| Self::parse(v))
    }
}

// ============================================================
// grpc-message percent-coding (SPEC.md § percent-encoding).
// ============================================================

/// Percent-encode a `grpc-message` value: `0x20..=0x7E`
/// except `%` pass through; everything else becomes
/// `%XX` (uppercase hex).
pub fn percent_encode(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for &b in raw {
        if (0x20..=0x7E).contains(&b) && b != b'%' {
            out.push(b);
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push(b'%');
            out.push(HEX[(b >> 4) as usize]);
            out.push(HEX[(b & 0x0F) as usize]);
        }
    }
    out
}

/// Leniently percent-decode a `grpc-message` value: valid
/// `%XX` sequences decode to the raw byte; a `%` not
/// followed by two hex digits passes through verbatim
/// (the spec forbids erroring). The decoded bytes are
/// interpreted as UTF-8 lossily.
pub fn percent_decode_lossy(raw: &[u8]) -> String {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            // `get` bounds the lookahead; both digits must
            // be valid hex or the '%' passes through.
            if let (Some(hi), Some(lo)) = (
                raw.get(i + 1).copied().and_then(hex_val),
                raw.get(i + 2).copied().and_then(hex_val),
            ) {
                bytes.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        bytes.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

fn int_to_bytes(n: i32) -> Vec<u8> {
    // StatusCode is 0..=16 — two decimal digits suffice.
    let n = n as u32;
    if n >= 10 {
        alloc::vec![b'0' + (n / 10) as u8, b'0' + (n % 10) as u8]
    } else {
        alloc::vec![b'0' + n as u8]
    }
}
