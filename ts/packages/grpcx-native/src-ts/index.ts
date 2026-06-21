/**
 * `@grpcx/native` — N-API binding for the grpcx wire
 * format v2 codec and the minimal protobuf encoder /
 * decoder.
 *
 * Hot path lives in C++; this file is only the type-safe
 * facade. Normative refs:
 *   - Wire grammar: `_dev/SPEC-wire-v2.md`
 *   - RFC: `_dev/RFC-internal-rpc-arch.md` § 6.
 */

import { createRequire } from "node:module";

// node-gyp-build is intentionally pure-runtime; using it
// through `require` lets the package work both from
// prebuilt binaries (`prebuilds/`) and from a local
// `node-gyp rebuild`.
const requireCJS = createRequire(import.meta.url);
const native = requireCJS("node-gyp-build")(
  // Walk up to package root (lib -> package).
  new URL("..", import.meta.url).pathname,
);

// ---- wire format v2 ----

export const HEADER_LEN: number = native.HEADER_LEN;

/** Frame type byte values (SPEC § 4). */
export enum FrameType {
  Begin = 0x01,
  Data = 0x02,
  End = 0x03,
  Reset = 0x04,
  Ping = 0x05,
  Pong = 0x06,
}

/** Flag bits on the flags byte (SPEC § 2.2). */
export const Flags = {
  EndStream: 0x01,
  HasMetadata: 0x02,
  Continuation: 0x04,
} as const;

/** Decoded v2 frame header. */
export interface Header {
  streamId: number;
  frameType: FrameType;
  flags: number;
  metadataCount: number;
  payloadLen: number;
}

/** One metadata entry, viewed as raw bytes. */
export interface NativeMetadataEntry {
  name: Uint8Array;
  value: Uint8Array;
}

/**
 * Encode a 16-byte v2 frame header. The returned
 * `Buffer` is sized exactly `HEADER_LEN`.
 */
export function encodeHeader(header: Header): Buffer {
  return native.encodeHeader(header);
}

/**
 * Decode a 16-byte v2 frame header. `buf` must contain
 * at least `HEADER_LEN` bytes; bytes after position 16
 * are ignored.
 */
export function decodeHeader(buf: Buffer | Uint8Array): Header {
  return native.decodeHeader(buf);
}

/**
 * Encode a metadata block (no header) into a `Buffer`.
 */
export function encodeMetadataBlock(entries: NativeMetadataEntry[]): Buffer {
  return native.encodeMetadataBlock(entries);
}

/**
 * Decode `count` metadata entries from the start of
 * `buf`. Returns the decoded entries plus the number of
 * bytes consumed (so the caller can slice the remaining
 * payload — typically the message body).
 */
export function decodeMetadataBlock(
  buf: Buffer | Uint8Array,
  count: number,
): { entries: NativeMetadataEntry[]; consumed: number } {
  return native.decodeMetadataBlock(buf, count);
}

// ---- protobuf codec ----

/** A field descriptor entry emitted by `grpcx-pb`'s TS
 *  backend. */
export interface FieldDescriptor {
  tag: number;
  name: string;
  type: string;
  label: string;
}

/**
 * Encode a JS message object into protobuf wire bytes.
 * `descriptor` describes the field layout (`tag` / `type`
 * / etc.) — typically a top-level `const FooDescriptor`
 * emitted by `grpcx-pb codegen_ts`.
 */
export function encodeMessage(
  descriptor: FieldDescriptor[],
  message: object,
): Buffer {
  return native.encodeMessage(descriptor, message);
}

/**
 * Decode a protobuf wire-bytes buffer into a plain JS
 * object using `descriptor` to interpret tags.
 */
export function decodeMessage<T = unknown>(
  descriptor: FieldDescriptor[],
  buf: Buffer | Uint8Array,
): T {
  return native.decodeMessage(descriptor, buf) as T;
}

// ---- transport-shmem ----

/**
 * Slot stride (per-frame byte budget) inside a shmem
 * ring. Matches the Rust constant.
 */
export const SHMEM_SLOT_STRIDE: number = native.SHMEM_SLOT_STRIDE;

/**
 * Shmem ring role indicator passed to {@link ringPush}
 * and {@link ringPop}. `0` is the client side (push to
 * c2s, pop from s2c); `1` is the server side (push to
 * s2c, pop from c2s). v1 TypeScript callers use `0`.
 */
export type ShmemRole = 0 | 1;

/** Rendezvous handshake output. */
export interface ShmemConnectResult {
  regionBuffer: Buffer;
  waitFd: number;
  signalFd: number;
  slotCount: number;
  slotStride: number;
  regionLen: number;
}

/** Result of a `ringPush` attempt. */
export interface RingPushResult {
  ok: boolean;
  full: boolean;
  tooLarge: boolean;
}

/** Result of a `ringPop` attempt. */
export interface RingPopResult {
  ok: boolean;
  empty: boolean;
  header?: Header;
  payloadLen?: number;
}

/**
 * Open a shmem connection by performing the AF_UNIX
 * rendezvous handshake against the server at `path`.
 * Returns the mmap'd region (as a Buffer) plus the wake
 * fds and ring dimensions.
 */
export function connectShmem(path: string): ShmemConnectResult {
  return native.connectShmem(path);
}

/**
 * Push one v2 frame into the send ring. Caller fills
 * the `header` (already aware of payload length) and
 * provides `payload` bytes. Returns `{ ok: false, full:
 * true }` if the ring is at capacity.
 */
export function ringPush(
  region: Buffer,
  role: ShmemRole,
  header: Header,
  payload: Buffer | Uint8Array,
): RingPushResult {
  return native.ringPush(region, role, header, payload);
}

/**
 * Pop one v2 frame from the recv ring into the caller's
 * `outPayload` buffer (≥ SHMEM_SLOT_STRIDE bytes). On
 * success, returns the decoded header + `payloadLen`
 * (the number of valid bytes written into `outPayload`).
 */
export function ringPop(
  region: Buffer,
  role: ShmemRole,
  outPayload: Buffer | Uint8Array,
): RingPopResult {
  return native.ringPop(region, role, outPayload);
}

/** Write one byte to the wake fd. Best-effort. */
export function wakeWrite(fd: number): void {
  native.wakeWrite(fd);
}

/** Block until one byte is drained from the wake fd. */
export function wakeRead(fd: number): void {
  native.wakeRead(fd);
}

/** Close a raw fd handed back from {@link connectShmem}. */
export function closeFd(fd: number): void {
  native.closeFd(fd);
}
