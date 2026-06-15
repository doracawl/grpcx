# Changelog

All notable changes to `grpcx-h2` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [0.3.0] below)

## [0.3.0] - 2026-06-12

**Eager cancel.**

### Added
- `send_rst_stream(stream, code)` — RST_STREAM emitted
  (gRPC clients use CANCEL = 0x8), local state dropped.
- Writes for a reset sid (`write_headers` /
  `write_trailers` / `write_data` / `finish`) are silent
  no-ops via a bounded recently-reset ring — previously a
  response landing after the peer's RST re-registered the
  stream and emitted frames the peer treats as a protocol
  error.

## [0.2.0] - 2026-06-12

**The sans-IO time surface + link liveness** (the L4
pattern):

### Added
- `pump_at(incoming, now_us)` — caller-injected monotonic
  clock (µs); `pump` keeps its exact semantics by
  delegating with a frozen clock (no timer ever fires on
  untimed pumps).
- Link liveness: quiet past the keepalive interval
  (default 2.5 s, `set_keepalive_us`) sends an RFC 9113
  PING (the receive side already ACKs them); no inbound
  bytes past the idle timeout (default 10 s,
  `set_idle_timeout_us`) → the new `H2Error::LinkDead`.
  The SETTINGS exchange against a dead peer is bounded by
  the same window. Gap forgiveness re-anchors after pump
  gaps wider than the keepalive interval (a blocking
  caller between calls).

## [0.1.0] - 2026-06-11

Initial release: RFC 9113 framing + full HPACK +
connection state machine (preface/SETTINGS, streams,
HEADERS+CONTINUATION, two-level flow control), the h2c
interop-bridge core.
