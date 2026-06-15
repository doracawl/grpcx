# `grpcx` Spec — gRPC wire + ALPN negotiation

The on-the-wire layout this crate accepts + produces.
Authoritative source: [gRPC HTTP/2 transport
spec][grpc-http2] + the gRPC-Web spec for the JSON
serialisation alternative. This file is a quick-reference
+ records charter decisions; ambiguity → defer to the
gRPC spec.

[grpc-http2]: https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md

## gRPC message framing

Every gRPC message on the wire (request or response,
streaming chunk or final) is framed as:

```
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+...
| compressed-flag | length (4-byte big-endian) | payload
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+...
```

| Field | Bytes | Notes |
|---|---|---|
| Compressed-Flag | 1 | `0` = uncompressed payload; `1` = compressed. v10 v1.0 only ships uncompressed; compression v1.x. |
| Length | 4 (big-endian) | Length of the `payload` field, NOT counting the 5-byte frame header. |
| Payload | `length` | Serialized protobuf message (encoded via L8's `ProtoCodec`). |

Multiple framed messages MAY appear in sequence on a
single HTTP/3 stream (server-streaming, client-streaming,
bidi-streaming).

## Headers — request (client → server)

Mapped to HTTP/3 (`:method`, `:scheme`, `:path`, `:authority`)
pseudo-headers + regular headers:

| Header | Value | Notes |
|---|---|---|
| `:method` | `POST` | gRPC requests are always POST. |
| `:scheme` | `https` | gRPC requires TLS. |
| `:path` | `/<package>.<Service>/<Method>` | Service + method per the gRPC spec. |
| `:authority` | host:port | Per HTTP/3 spec. |
| `content-type` | `application/grpc` | Or `application/grpc+proto` for explicit proto encoding. |
| `te` | `trailers` | Indicates the client accepts trailers (server response trailers carry the status). |
| `user-agent` | `grpcx/<version>` | This crate's UA. |
| `grpc-timeout` | optional | Per-call deadline (e.g. `5000m` for 5000 milliseconds). |
| `grpc-encoding` | optional | Compression name (`identity` / `gzip` / etc.). v1.0 only sends `identity`. |
| custom metadata | `*` | Arbitrary `key: value` pairs the application sets via `Metadata`. |

## Trailers — response (server → client)

Trailers carry the RPC status and any application
metadata that wasn't sent inline:

| Trailer | Value | Notes |
|---|---|---|
| `grpc-status` | numeric `0..=16` | The `StatusCode` (see table below). |
| `grpc-message` | text | Optional human-readable status detail. |
| `grpc-status-details-bin` | base64 | Optional structured status payload (proto-encoded `google.rpc.Status`); v1.x. |
| custom trailing metadata | `*` | Application headers sent at the end of the response stream. |

### `grpc-message` percent-encoding

Per the gRPC HTTP/2 spec the `grpc-message` value is
**Percent-Encoded**:

- Bytes in `0x20..=0x7E` **except `%` (0x25)** pass
  through unchanged.
- Every other byte (including `%` itself and each byte of
  a multi-byte UTF-8 sequence) is emitted as `%XX`,
  uppercase hex.
- **Decoding is lenient** (spec: implementations "should
  not error" on bad encodings): a `%` not followed by two
  hex digits passes through verbatim; valid `%XX` decodes
  to the raw byte; the decoded byte string is interpreted
  as UTF-8 lossily (invalid sequences → U+FFFD), never
  rejected.

## `StatusCode` table

Numeric values per [grpc-status-codes][gsc]:

[gsc]: https://github.com/grpc/grpc/blob/master/doc/statuscodes.md

| Code | Name | Numeric |
|---|---|---|
| `Ok` | `Ok` | 0 |
| `Cancelled` | `Cancelled` | 1 |
| `Unknown` | `Unknown` | 2 |
| `InvalidArgument` | `InvalidArgument` | 3 |
| `DeadlineExceeded` | `DeadlineExceeded` | 4 |
| `NotFound` | `NotFound` | 5 |
| `AlreadyExists` | `AlreadyExists` | 6 |
| `PermissionDenied` | `PermissionDenied` | 7 |
| `ResourceExhausted` | `ResourceExhausted` | 8 |
| `FailedPrecondition` | `FailedPrecondition` | 9 |
| `Aborted` | `Aborted` | 10 |
| `OutOfRange` | `OutOfRange` | 11 |
| `Unimplemented` | `Unimplemented` | 12 |
| `Internal` | `Internal` | 13 |
| `Unavailable` | `Unavailable` | 14 |
| `DataLoss` | `DataLoss` | 15 |
| `Unauthenticated` | `Unauthenticated` | 16 |

## ALPN negotiation strategy (ROADMAP § L10 charter)

### Non-browser client + server

TLS-ALPN advertises tokens; client and server agree on
the preferred match. v10 supports:

| Token | Transport | Stack |
|---|---|---|
| `grpcx/h3` | HTTP/3 over QUIC | L4 + L5 |
| `grpcx/ws-mux` | WebSocket with application-level mux | L9 + grpcx mux layer |

`grpcx/h3` is the **preferred** token. `grpcx/ws-mux` is
the fallback when QUIC is blocked by middleware or
unsupported on either side. **HTTP/2 (charter revision
2026-06-11: chartered as the interop bridge — see the
transport matrix below) is still not a *fallback*
target** per the 2026-05-28 charter direction.

### Browser client

Browsers can't speak QUIC + HTTP/3 directly to arbitrary
ports (CORS / certificate restrictions). Instead:

1. Client tries WebTransport first (over HTTP/3 — L6).
2. On unavailability falls back to WebSocket+ws-mux (L9
   + grpcx mux).

Servers advertise the WebTransport endpoint via `Alt-Svc`:

```
Alt-Svc: h3-grpcx=":4433"; ma=86400
```

## `grpcx/ws-mux` — application-level multiplexing

Vanilla WebSocket is single-stream. `grpcx/ws-mux` adds
an L10-internal multiplexer. **Locked at v0.6.**

### Carrier

One mux frame = one WebSocket **binary message** (RFC
6455 message boundaries delimit mux frames — no length
field needed). The WS connection is established with a
standard RFC 6455 upgrade handshake on path
`/grpcx-ws-mux`; after the 101 every message is binary.

### Mux frame

```
+----------------------------+-------------------+----------
| stream-id (4-byte big-end) | type|flags (1 B)  | payload
+----------------------------+-------------------+----------
```

| type (low 7 bits) | Name | Payload |
|---|---|---|
| `0x01` | HEADERS | serialized header list (below) |
| `0x02` | DATA | raw bytes (gRPC length-prefixed frames) |
| `0x03` | TRAILERS | serialized header list |
| `0x04` | PING | opaque, echoed back (liveness, 0.17) |
| `0x05` | PONG | the PING payload (liveness, 0.17) |
| `0x06` | RST | empty (eager cancel, 0.22) |

Flag bit `0x80` = **END_STREAM** (this frame closes the
sender's direction of the stream, like HTTP/2's flag).

PING/PONG live on the reserved stream id 0 only (link
liveness — any inbound traffic is proof of life; a quiet
link is probed, no answer past the idle timeout is
death). RST is sid-addressed: the sender abandoned the
call; the receiver drops all per-stream state and stops
producing for the sid — a cancelled-before-dispatch
request never reaches the handler.

Stream IDs: odd = client-initiated, starting at 1,
ascending; even reserved for server-initiated streams
(unused at v1.0 — no push). Stream 0 is reserved.

### Header-list serialization

No QPACK on the fallback path — simple and stateless:

```
entry = name-len (u16 BE) name-bytes value-len (u16 BE) value-bytes
list  = entry*
```

### gRPC mapping

Identical header semantics to the H/3 path (`:method`,
`:path`, `content-type`, `grpc-status`, …):

- request: `HEADERS` + `DATA`* + (last DATA carries
  END_STREAM, or an empty END_STREAM DATA frame)
- response: `HEADERS` + `DATA`* + `TRAILERS|END_STREAM`
- trailers-only error: `HEADERS|END_STREAM` carrying
  `grpc-status` directly

### Masking note

Client→server WS frames are masked per RFC 6455. The mask
key is derived from a caller-provided seed (documented as
NOT cryptographically random — RFC 6455 masking defends
proxy cache poisoning, not confidentiality; TLS provides
the security boundary).

## `grpcx/tcp-mux` — the mux grammar on raw TCP

T7 (charter 2026-06-11): the ws-mux frame grammar above,
shed of its WebSocket shell, for bare-metal links where
neither TLS nor HTTP framing is wanted. Prior knowledge —
no handshake; the connection is ready the moment it
opens. Not in the ALPN chain (there is no TLS to carry
ALPN); selection is by endpoint configuration.

Outer shell: each mux frame travels as one
length-prefixed record on the byte stream:

```
record = length (u32 BE, mux-frame byte count) mux-frame
```

Everything inside the record — frame grammar, header-list
serialization, stream-id rules, gRPC mapping, the
trailers-only error shape — is identical to
`grpcx/ws-mux` (the shared `mux` layer in the
implementation). Records have a 64 MiB sanity cap; a
larger declared length is a peer violation, not a pending
state. No masking (no proxy in the threat model on a raw
TCP link).

## Out-of-scope at v1.0

- gRPC-Web (browser-direct gRPC bridge without
  WebTransport).
- ~~HTTP/2 fallback transport~~ (charter revised
  2026-06-11: H/2 + h2c chartered as interop bridge;
  `grpcx/tcp-mux` chartered as the bare-TCP form. See
  docs/dx-scope.md + the transport matrix.)
- Server reflection (`grpc.reflection.v1`).
- `grpc-status-details-bin` structured status payload.
- Compression (`gzip`, `deflate`, `snappy`).
- Per-call deadline enforcement (header parsed, but not
  enforced at v1.0).
- Interceptor / middleware hooks.

Each is tracked in the v10.0 release REFACTOR's deferral
list.
