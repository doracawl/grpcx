import type { CallContext } from "./call-context.js";
import type { Metadata } from "./types.js";

/**
 * Abstract transport connection. Concrete subclasses
 * live in `@grpcx/transport-uds` / `@grpcx/transport-shmem`
 * / `@grpcx/transport-tcp-mux` and route the per-RPC calls
 * through `@grpcx/native` to the underlying socket.
 *
 * Generated `XxxClient` stubs (from `grpcx-pb`'s TS
 * backend) depend only on this interface — switching
 * transports does not touch business code.
 */
export abstract class Connection {
  /** Send a unary RPC. */
  abstract callUnary(
    service: string,
    method: string,
    ctx: CallContext,
    body: Uint8Array,
  ): Promise<[Uint8Array, Metadata]>;

  /** Send a server-streaming RPC. Returns an async-
   *  iterable of response body bytes; the trailer
   *  metadata is set on `ctx` once the stream ends. */
  abstract callServerStreaming(
    service: string,
    method: string,
    ctx: CallContext,
    body: Uint8Array,
  ): AsyncIterable<Uint8Array>;

  /** Send a client-streaming RPC. The async-iterable
   *  delivers each request body to the transport;
   *  resolves with the single response body + trailers. */
  abstract callClientStreaming(
    service: string,
    method: string,
    ctx: CallContext,
    bodies: AsyncIterable<Uint8Array>,
  ): Promise<[Uint8Array, Metadata]>;

  /** Send a bidi-streaming RPC. The async-iterable
   *  delivers each request body; the returned async-
   *  iterable yields each response body. Trailer
   *  metadata is set on `ctx` after the response stream
   *  ends. */
  abstract callBidi(
    service: string,
    method: string,
    ctx: CallContext,
    bodies: AsyncIterable<Uint8Array>,
  ): AsyncIterable<Uint8Array>;

  /** Close the underlying transport. Subsequent calls
   *  fail. */
  abstract close(): Promise<void>;
}
