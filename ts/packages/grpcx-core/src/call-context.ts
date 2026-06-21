import type { Metadata, PeerCred } from "./types.js";

/**
 * Per-call context — request metadata in, trailing
 * metadata out, optional peer credentials on UDS /
 * shmem transports.
 *
 * Mirrors the Rust `grpcx::CallContext` shape.
 */
export class CallContext {
  private requestMeta: Metadata = [];
  private trailingMeta: Metadata = [];
  private peer: PeerCred | null = null;

  /** The caller's request metadata. */
  requestMetadata(): Metadata {
    return this.requestMeta;
  }

  /** Append one request-metadata entry (client side). */
  addRequestHeader(name: string, value: Uint8Array): void {
    this.requestMeta.push({ name, value });
  }

  /** The trailing metadata (populated by client after the
   *  call completes; server appends before returning). */
  trailing(): Metadata {
    return this.trailingMeta;
  }

  /** Append one trailing-metadata entry (server side). */
  addTrailing(name: string, value: Uint8Array): void {
    this.trailingMeta.push({ name, value });
  }

  /** Transport plumbing — used by client transports
   *  after the response trailers arrive. */
  setTrailing(trailing: Metadata): void {
    this.trailingMeta = trailing;
  }

  /**
   * Kernel-attested peer credentials of the connection
   * peer. Populated by `transport-uds` / `transport-shmem`
   * at accept-time; `null` on the network transports.
   */
  peerCred(): PeerCred | null {
    return this.peer;
  }

  /** Transport plumbing — populated by the server-side
   *  accept path on UDS / shmem before the handler runs. */
  setPeerCred(pc: PeerCred | null): void {
    this.peer = pc;
  }
}
