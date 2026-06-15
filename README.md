# grpcx

> Extreme-performance RPC framework — fully in-house from `epoll(2)` to the protobuf-editions codec.

A protobuf-editions-compatible RPC stack with **zero external runtime
dependencies** (one bounded exception — see L3 note below). Every
infrastructure layer — OS IO abstraction, async runtime, crypto, QUIC,
HTTP/3, WebTransport, the protobuf importer itself — is a `grpcx-*`
crate.

**Status:** L1 (`grpcx-io`) v0.8 closed. Linux `epoll` + `io_uring` +
Darwin `kqueue` + FreeBSD `kqueue` live; at parity-or-better with
`mio` on every measured bench across 3 OSes; io_uring lane 1.29× faster
than `mio` epoll at N ∈ {8, 64, 256}. Next: v0.9 doc polish + CI; then
v0.10 fuzz + coverage; then L2 `grpcx-rt`. See
[ROADMAP.md](./ROADMAP.md).

## Project principles

1. **Zero deps (with one bounded crypto exception).**
   `[workspace.dependencies]` is otherwise empty. Each layer is
   replaced by a `grpcx-*` crate before the next layer starts.
   `grpcx-crypto` initially wraps `rustls` + `*ring*` (interim form,
   2026-05-28 → external-cryptographer-review window) — full
   self-implementation deferred so the lab doesn't ship unreviewed
   crypto. See [DEPS_AUDIT.md](./DEPS_AUDIT.md) for the audit trail.
2. **Bottom-up.** No layer enters the next until its perf / API / docs /
   tests are the indisputable best in the Rust ecosystem for its scope.
3. **Protobuf editions only.** We compile edition 2023 and forward. No
   proto2 / proto3 legacy.
4. **Transport-agnostic protocol.** Web clients run over WebTransport
   (HTTP/3); WebSocket + custom mux is the fallback. The wire format
   does not bind to HTTP/3.
5. **Decisions resolve to the upper bound.** Not ROI, not convenience.
6. **Priority order: perf > stability > mem > binsize.** Every design
   tradeoff resolves in that order. See
   [ARCHITECTURE.md](./ARCHITECTURE.md#priorities-project-wide).

## Layers

| L  | Crate            | Replaces             | Status |
|----|------------------|----------------------|--------|
| 1  | `grpcx-io`       | `mio`                | 🚧 v0.8 (next: v0.9 doc polish + CI) |
| 2  | `grpcx-rt`       | `tokio`              | — |
| 3  | `grpcx-crypto`   | `rustls` / `*ring*`† | — (interim wrapper form first) |
| 4  | `grpcx-quic`     | `quinn`              | — |
| 5  | `grpcx-h3`       | `h3`                 | — |
| 6  | `grpcx-wt`       | `wtransport`         | — |
| 7  | `grpcx-idl`‡     | (new — own IDL + zero-copy wire) | — |
| 8  | `grpcx-pb`‡      | `prost` + `protoc`   | — |
| 9  | `grpcx-ws`       | `tungstenite` + mux  | — |
| 10 | `grpcx`          | `tonic`              | — |

† L3 interim form uses `rustls` + `*ring*` until the external
cryptographer-review window opens; self-implementation deferred (see
[DEPS_AUDIT.md § L3](./DEPS_AUDIT.md)).

‡ L7 ↔ L8 swap (2026-05-28): our own IDL is L7 (implemented first);
`.proto` importer is L8 (atop L7's codec).

See [ROADMAP.md](./ROADMAP.md) for the trigger criteria between layers.

## Where we are now (L1 `grpcx-io`)

- **Backends live**: Linux epoll + io_uring, Darwin kqueue, FreeBSD
  kqueue. (Windows IOCP **removed from v1 scope** — no validation
  hardware reachable as of 2026-05-28; tracked in
  [REFACTOR-v1-v0.6-windows-iocp.md](./REFACTOR-v1-v0.6-windows-iocp.md).)
- **Cross-OS bench numbers** — grpcx-io at parity-or-better with mio
  on every measured op; full table in [PERFORMANCE.md](./PERFORMANCE.md).
- **Validation hardware** — Linux on `lx64` (Debian 13 / i7-10700K),
  Darwin on the dev host, FreeBSD on a KVM VM hosted by `lx64`.

## Quickstart (L1)

```rust,no_run
use grpcx_io::{Events, Interest, IoResult, Poll, Token};

fn main() -> IoResult<()> {
    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(1024);

    // SourceFd is available on Linux / macOS / iOS / FreeBSD.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        use grpcx_io::SourceFd;
        let mut source = SourceFd::new(0); // stdin in this toy example
        poll.registry().register(&mut source, Token(0), Interest::READABLE)?;
    }

    poll.poll(&mut events, None)?;
    for event in &events {
        let _ = event.token();
        let _ = event.ready();
    }
    Ok(())
}
```

For the `io_uring`-specific completion API on Linux, use
`grpcx_io::Ring` (sibling to `Poll`, not a substitute — see
[crates/grpcx-io/README.md](./crates/grpcx-io/README.md)).

## Architecture + docs

- [ARCHITECTURE.md](./ARCHITECTURE.md) — Stones and cement under 0-dep + priority axes
- [DEPS_AUDIT.md](./DEPS_AUDIT.md) — Why each layer must be rewritten; L3 wrapper exception
- [ROADMAP.md](./ROADMAP.md) — L1 → L10 layer triggers + project priorities
- [PERFORMANCE.md](./PERFORMANCE.md) — Cross-OS bench numbers + methodology
- [docs/v1-scope.md](./docs/v1-scope.md) — Current version scope (v1 = L1)
- [REFACTOR-v1-v0.X-*.md](./) — Per-checkpoint design notes (1 per closed checkpoint)

## License

Apache-2.0 OR MIT
