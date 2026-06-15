# `grpcx-ws` Wire Format — RFC 6455 reference

The on-the-wire layout this crate accepts + produces.
Authoritative source: [RFC 6455][rfc] § 5 + § 7. This
file is a quick-reference; ambiguity → defer to the RFC.

[rfc]: https://datatracker.ietf.org/doc/html/rfc6455

## Frame layout (§ 5.2)

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-------+-+-------------+-------------------------------+
|F|R|R|R| opcode|M| Payload len |    Extended payload length    |
|I|S|S|S|  (4)  |A|     (7)     |             (16/64)           |
|N|V|V|V|       |S|             |   (if payload len==126/127)   |
| |1|2|3|       |K|             |                               |
+-+-+-+-+-------+-+-------------+ - - - - - - - - - - - - - - - +
|     Extended payload length continued, if payload len == 127  |
+ - - - - - - - - - - - - - - - +-------------------------------+
|                               |Masking-key, if MASK set to 1  |
+-------------------------------+-------------------------------+
| Masking-key (continued)       |          Payload Data         |
+-------------------------------- - - - - - - - - - - - - - - - +
:                     Payload Data continued ...                :
+ - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - +
|                     Payload Data continued ...                |
+---------------------------------------------------------------+
```

### Header bytes

| Bit(s) | Field | Notes |
|---|---|---|
| Byte 0, bit 7 | `FIN` | 1 = last frame of a message; 0 = continuation follows |
| Byte 0, bits 4-6 | `RSV1/RSV2/RSV3` | MUST be 0 unless an extension negotiated them |
| Byte 0, bits 0-3 | `Opcode` | see table below |
| Byte 1, bit 7 | `MASK` | 1 = payload is XOR-masked with `Masking-key`; client→server MUST be 1 |
| Byte 1, bits 0-6 | `Payload length` | see "length encoding" |

### Opcodes (§ 5.2)

| Code | Name | Class |
|---|---|---|
| `0x0` | Continuation | data |
| `0x1` | Text | data (UTF-8 payload) |
| `0x2` | Binary | data |
| `0x3-0x7` | reserved (non-control) | reject |
| `0x8` | Close | control |
| `0x9` | Ping | control |
| `0xA` | Pong | control |
| `0xB-0xF` | reserved (control) | reject |

Control frames (`Close` / `Ping` / `Pong`):
- MUST have `FIN=1` (not fragmentable).
- Payload length ≤ 125 bytes (no extended length encoding).
- MAY be interleaved with fragments of a data message.

### Length encoding (§ 5.2)

| `Payload length` field | Actual length |
|---|---|
| 0..=125 | the field itself |
| 126 | follows 2 bytes big-endian unsigned (16-bit length) |
| 127 | follows 8 bytes big-endian unsigned (64-bit length); MSB MUST be 0 |

Smallest encoding form is REQUIRED — i.e., `126` MUST NOT
be used for lengths ≤ 125, and `127` MUST NOT be used for
lengths ≤ 65535. v0.2 decoder enforces.

### Masking (§ 5.3)

- Client→server frames MUST set `MASK=1` and follow with a
  4-byte `Masking-key`. Each payload byte `B[i]` is XORed
  with `key[i mod 4]` on the wire.
- Server→client frames MUST set `MASK=0` and MUST NOT
  carry a `Masking-key`.
- The crate's `decode_frame` enforces the directionality
  rule at the caller side (v0.2 ships a separate
  `decode_frame_client_view` / `_server_view` pair, or a
  single `decode_frame` plus a check on `Frame.mask`'s
  presence vs the caller's role).

## Handshake (§ 4)

### Client request (§ 4.2.1)

```
GET /chat HTTP/1.1
Host: server.example.com
Upgrade: websocket
Connection: Upgrade
Sec-WebSocket-Key: <16-byte base64-encoded value>
Sec-WebSocket-Version: 13
Sec-WebSocket-Protocol: <optional subprotocol(s), comma-separated>
Sec-WebSocket-Extensions: <optional extension token(s)>
Origin: <optional>
\r\n
```

### Server response (§ 4.2.2)

```
HTTP/1.1 101 Switching Protocols
Upgrade: websocket
Connection: Upgrade
Sec-WebSocket-Accept: <SHA-1(request_key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"), base64-encoded>
Sec-WebSocket-Protocol: <selected subprotocol, if any>
Sec-WebSocket-Extensions: <selected extension(s), if any>
\r\n
```

The "magic GUID" is fixed by RFC 6455 § 4.2.2 step 5:

```
258EAFA5-E914-47DA-95CA-C5AB0DC85B11
```

### Reference example (§ 1.3)

The RFC's canonical test vector:

| Field | Value |
|---|---|
| `Sec-WebSocket-Key` (request) | `dGhlIHNhbXBsZSBub25jZQ==` |
| Concatenation | `dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11` |
| SHA-1 of concatenation | `b3 7a 4f 2c c0 62 4f 16 90 f6 46 06 cf 38 59 45 b2 be c4 ea` |
| `Sec-WebSocket-Accept` (response) | `s3pPLMBiTxaQ9kYGzzhZRbK+xOo=` |

v0.5's handshake codec is wire-byte-compatible with this
vector.

## Close codes (§ 7.4.1)

Status codes the crate recognises by name:

| Code | Name | Description |
|---|---|---|
| 1000 | Normal | Normal closure |
| 1001 | GoingAway | Endpoint going away (e.g. server shutdown) |
| 1002 | ProtocolError | Protocol error |
| 1003 | UnsupportedData | Endpoint can't accept this data type |
| 1007 | InvalidPayload | Payload doesn't match opcode-implied form (e.g. non-UTF-8 Text) |
| 1008 | PolicyViolation | Generic policy violation |
| 1009 | MessageTooLarge | Message too large to process |
| 1010 | ExtensionRequired | Client required extension(s) server didn't honour |
| 1011 | InternalError | Internal server error |
| 1004, 1005, 1006, 1015 | (reserved) | MUST NOT be sent on the wire |
| 3000-3999 | (IANA-registered, library-defined) | passed through as `Custom(u16)` |
| 4000-4999 | (application private) | passed through as `Custom(u16)` |

`CloseCode::from_u16` rejects the reserved codes.

## Constants

```text
WS_MAGIC_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
WS_VERSION    = 13      // Sec-WebSocket-Version
WS_UPGRADE    = "websocket"
WS_MAX_CONTROL_PAYLOAD = 125   // control frames cap
```

## Out-of-scope at v0.1 (locked at v0.x)

| Construct | Why deferred |
|---|---|
| `permessage-deflate` compression | v0.6 lands the negotiation; real compression v1.x |
| WebSocket-over-HTTP/2 (RFC 8441) | v2.x cross-layer |
| Per-frame timing / rate limiting | Consumer's concern |
| Extension RSV bit allocation | v0.6 surface; consumers register their own RSV semantics |
