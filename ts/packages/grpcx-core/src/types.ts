/**
 * One header (name + binary value). The wire format
 * lets header values be arbitrary bytes (`*-bin` headers
 * carry binary, others carry UTF-8). Callers that want
 * a string interpret the bytes themselves.
 */
export interface MetadataEntry {
  name: string;
  value: Uint8Array;
}

/**
 * Ordered list of (name, value) header pairs.
 * Per-call metadata in or out.
 */
export type Metadata = MetadataEntry[];

/**
 * Kernel-attested peer credentials surfaced by the
 * `transport-uds` / `transport-shmem` transports. `pid`
 * is Linux-only (Darwin / FreeBSD `getpeereid` returns
 * uid + gid only).
 */
export interface PeerCred {
  uid: number;
  gid: number | null;
  pid: number | null;
}
