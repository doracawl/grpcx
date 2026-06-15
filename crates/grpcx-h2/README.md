# grpcx-h2

HTTP/2 framing + HPACK (RFC 7540/9113 + RFC 7541) —
**the interop bridge** of the grpcx stack (charter
revision 2026-06-11): grpcx endpoints speak to existing
tonic / grpc-go / grpc-java deployments over standard
gRPC-over-HTTP/2, in both `h2` (TLS) and `h2c`
(cleartext prior-knowledge) forms.

Positioning: `grpcx/h3` is the primary transport; this
crate exists for ecosystem interop and per-endpoint
migration, not as a fallback. Same sans-IO `pump` shape
as L4/L5; `no_std + alloc`.

Acceptance bar: live bidirectional interop e2e against
tonic (the comparator harness already hosts it).
