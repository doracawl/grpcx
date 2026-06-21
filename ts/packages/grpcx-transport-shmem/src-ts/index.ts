/**
 * `@grpcx/transport-shmem` — client-side shared-memory
 * transport carrying grpcx wire format v2.
 *
 * Hot path:
 *   - Rendezvous (`recvmsg` + `SCM_RIGHTS` + `mmap`) in
 *     C++ (@grpcx/native `connectShmem`).
 *   - Ring push/pop with `AtomicU64` Acquire/Release on
 *     the shmem region — also in C++.
 *   - Wake fd write / read syscalls — C++.
 *
 * Nothing performance-critical runs in V8.
 */

import { CallContext, Connection, Status, StatusCode } from "@grpcx/core";
import type { Metadata, MetadataEntry } from "@grpcx/core";
import {
  Flags,
  FrameType,
  HEADER_LEN,
  SHMEM_SLOT_STRIDE,
  closeFd,
  connectShmem,
  ringPop,
  ringPush,
  wakeRead,
  wakeWrite,
  type Header,
  type NativeMetadataEntry,
} from "@grpcx/native";

const PSEUDO_SERVICE = Buffer.from(":service");
const PSEUDO_METHOD = Buffer.from(":method");
const PSEUDO_KIND = Buffer.from(":kind");

// JS-side metadata block codec. Replaces the N-API
// `encodeMetadataBlock` / `decodeMetadataBlock` cross-
// boundary calls for the hot unary path. The wire format
// is one entry of `name_len u16 LE | value_len u32 LE |
// name | value`, repeated `count` times — see
// `crates/grpcx/src/wire_v2/metadata.rs`.
function jsEncodeMetadataBlock(entries: NativeMetadataEntry[]): Buffer {
  let total = 0;
  for (let i = 0; i < entries.length; i++) {
    const e = entries[i]!;
    total += 6 + e.name.length + e.value.length;
  }
  const out = Buffer.allocUnsafe(total);
  let off = 0;
  for (let i = 0; i < entries.length; i++) {
    const e = entries[i]!;
    out.writeUInt16LE(e.name.length, off);
    out.writeUInt32LE(e.value.length, off + 2);
    off += 6;
    (e.name as Buffer).copy(out, off);
    off += e.name.length;
    (e.value as Buffer).copy(out, off);
    off += e.value.length;
  }
  return out;
}

interface DecodedMetadataBlock {
  entries: NativeMetadataEntry[];
  consumed: number;
}

function jsDecodeMetadataBlock(
  buf: Buffer,
  count: number,
): DecodedMetadataBlock {
  const entries: NativeMetadataEntry[] = new Array(count);
  let off = 0;
  for (let i = 0; i < count; i++) {
    if (off + 6 > buf.length) {
      throw new Error("shmem: truncated metadata entry header");
    }
    const nameLen = buf.readUInt16LE(off);
    const valueLen = buf.readUInt32LE(off + 2);
    off += 6;
    const nameEnd = off + nameLen;
    const valueEnd = nameEnd + valueLen;
    if (valueEnd > buf.length) {
      throw new Error("shmem: truncated metadata entry payload");
    }
    // Zero-copy subarray view into the source Buffer —
    // valid until the source `recvBuf` is overwritten by
    // the next `ringPop`. Callers that need to keep the
    // bytes past that point must copy.
    entries[i] = {
      name: buf.subarray(off, nameEnd),
      value: buf.subarray(nameEnd, valueEnd),
    };
    off = valueEnd;
  }
  return { entries, consumed: off };
}

const KIND_UNARY = 0;
const KIND_SERVER_STREAM = 1;
const KIND_CLIENT_STREAM = 2;
const KIND_BIDI = 3;

interface StreamState {
  bodies: Buffer[];
  resolve: (value: { bodies: Buffer[]; trailing: Metadata }) => void;
  reject: (error: Error) => void;
}

/**
 * Client handle for a `transport-shmem` connection.
 *
 * `call*` methods are synchronous in spirit — they drive
 * the ring + wake-fd loop on the calling thread. Node's
 * event loop pauses during the blocking `wakeRead`;
 * v1.x will lift this onto a libuv worker.
 */
export class ShmemConnection extends Connection {
  private readonly region: Buffer;
  private readonly waitFd: number;
  private readonly signalFd: number;
  private readonly recvBuf: Buffer;
  private nextStreamId = 1;
  private closed = false;
  // When true, both `pushWithWake` and `popBlocking` skip
  // the eventfd `wakeWrite`/`wakeRead` syscalls and spin
  // on the ring in user space. Peer must agree (run the
  // server via `Server::listen_shmem_busy_poll_until`,
  // e.g. `shmem_test_server` with `SHMEM_BUSY_POLL=1`).
  // Mirrors `ShmemConnection::set_busy_poll_unbounded` on
  // the Rust side; closes the per-RPC wake-fd syscall
  // cost on the TS hot path (~3 µs/RPC on lx64).
  private busyPollUnbounded = false;
  // Per (service, method, kind) cache of the pre-encoded
  // 3-entry pseudo-header metadata block bytes. Avoids
  // re-running `Buffer.from(service)` / `Buffer.from(
  // method)` / `Buffer.from([kind])` + `jsEncodeMetadata
  // Block` for the common case (no user metadata) on
  // every unary BEGIN.
  private readonly beginMetaCache = new Map<string, Buffer>();

  private constructor(
    region: Buffer,
    waitFd: number,
    signalFd: number,
    _slotCount: number,
    _slotStride: number,
  ) {
    super();
    this.region = region;
    this.waitFd = waitFd;
    this.signalFd = signalFd;
    this.recvBuf = Buffer.alloc(SHMEM_SLOT_STRIDE);
  }

  /** Connect to a shmem server bound at the AF_UNIX
   *  rendezvous `path`. */
  static async connect(path: string, _authority: string): Promise<ShmemConnection> {
    const r = connectShmem(path);
    return new ShmemConnection(
      r.regionBuffer,
      r.waitFd,
      r.signalFd,
      r.slotCount,
      r.slotStride,
    );
  }

  override async callUnary(
    service: string,
    method: string,
    ctx: CallContext,
    body: Uint8Array,
  ): Promise<[Uint8Array, Metadata]> {
    const streamId = this.allocStreamId();
    this.sendBegin(
      streamId,
      service,
      method,
      KIND_UNARY,
      ctx.requestMetadata(),
      Buffer.from(body),
      true,
    );
    const { bodies, trailing } = this.readResponseStream(true);
    ctx.setTrailing(trailing);
    return [bodies.at(0) ?? Buffer.alloc(0), trailing];
  }

  override async *callServerStreaming(
    service: string,
    method: string,
    ctx: CallContext,
    body: Uint8Array,
  ): AsyncIterable<Uint8Array> {
    const streamId = this.allocStreamId();
    this.sendBegin(
      streamId,
      service,
      method,
      KIND_SERVER_STREAM,
      ctx.requestMetadata(),
      Buffer.from(body),
      true,
    );
    const { bodies, trailing } = this.readResponseStream(false);
    ctx.setTrailing(trailing);
    for (const b of bodies) yield b;
  }

  override async callClientStreaming(
    service: string,
    method: string,
    ctx: CallContext,
    bodies: AsyncIterable<Uint8Array>,
  ): Promise<[Uint8Array, Metadata]> {
    const streamId = this.allocStreamId();
    let first: Uint8Array | null = null;
    const rest: Uint8Array[] = [];
    for await (const b of bodies) {
      if (first === null) first = b;
      else rest.push(b);
    }
    const onlyOne = rest.length === 0;
    this.sendBegin(
      streamId,
      service,
      method,
      KIND_CLIENT_STREAM,
      ctx.requestMetadata(),
      Buffer.from(first ?? new Uint8Array()),
      onlyOne,
    );
    for (let i = 0; i < rest.length; i++) {
      const isLast = i + 1 === rest.length;
      this.sendData(streamId, Buffer.from(rest[i]!), isLast);
    }
    const { bodies: respBodies, trailing } = this.readResponseStream(true);
    ctx.setTrailing(trailing);
    return [respBodies.at(0) ?? Buffer.alloc(0), trailing];
  }

  override async *callBidi(
    service: string,
    method: string,
    ctx: CallContext,
    bodies: AsyncIterable<Uint8Array>,
  ): AsyncIterable<Uint8Array> {
    const streamId = this.allocStreamId();
    let first: Uint8Array | null = null;
    const rest: Uint8Array[] = [];
    for await (const b of bodies) {
      if (first === null) first = b;
      else rest.push(b);
    }
    const onlyOne = rest.length === 0;
    this.sendBegin(
      streamId,
      service,
      method,
      KIND_BIDI,
      ctx.requestMetadata(),
      Buffer.from(first ?? new Uint8Array()),
      onlyOne,
    );
    for (let i = 0; i < rest.length; i++) {
      const isLast = i + 1 === rest.length;
      this.sendData(streamId, Buffer.from(rest[i]!), isLast);
    }
    const { bodies: respBodies, trailing } = this.readResponseStream(false);
    ctx.setTrailing(trailing);
    for (const b of respBodies) yield b;
  }

  override async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    closeFd(this.waitFd);
    closeFd(this.signalFd);
  }

  // ----------------------------------------------------------
  // internals
  // ----------------------------------------------------------

  private allocStreamId(): number {
    const id = this.nextStreamId;
    this.nextStreamId += 2;
    return id;
  }

  private metaToEntries(meta: Metadata): NativeMetadataEntry[] {
    const out: NativeMetadataEntry[] = new Array(meta.length);
    for (let i = 0; i < meta.length; i++) {
      const e = meta[i]!;
      out[i] = { name: Buffer.from(e.name), value: e.value };
    }
    return out;
  }

  private entriesToMeta(entries: NativeMetadataEntry[]): Metadata {
    const out: MetadataEntry[] = new Array(entries.length);
    for (let i = 0; i < entries.length; i++) {
      const e = entries[i]!;
      out[i] = { name: Buffer.from(e.name).toString("utf8"), value: e.value };
    }
    return out;
  }

  private sendBegin(
    streamId: number,
    service: string,
    method: string,
    kind: number,
    requestMeta: Metadata,
    body: Buffer,
    endStream: boolean,
  ): void {
    // Fast path: no user metadata. The 3-entry pseudo
    // header block depends only on (service, method,
    // kind) — cache the encoded bytes per tuple to avoid
    // 3× `Buffer.from()` + `jsEncodeMetadataBlock` per
    // call.
    let metaBuf: Buffer;
    let metaCount: number;
    if (requestMeta.length === 0) {
      const key = `${service}\x00${method}\x00${kind}`;
      let cached = this.beginMetaCache.get(key);
      if (!cached) {
        cached = jsEncodeMetadataBlock([
          { name: PSEUDO_SERVICE, value: Buffer.from(service) },
          { name: PSEUDO_METHOD, value: Buffer.from(method) },
          { name: PSEUDO_KIND, value: Buffer.from([kind]) },
        ]);
        this.beginMetaCache.set(key, cached);
      }
      metaBuf = cached;
      metaCount = 3;
    } else {
      const entries: NativeMetadataEntry[] = [
        { name: PSEUDO_SERVICE, value: Buffer.from(service) },
        { name: PSEUDO_METHOD, value: Buffer.from(method) },
        { name: PSEUDO_KIND, value: Buffer.from([kind]) },
        ...this.metaToEntries(requestMeta),
      ];
      metaBuf = jsEncodeMetadataBlock(entries);
      metaCount = entries.length;
    }
    const payload = Buffer.concat([metaBuf, body]);
    let flags = Flags.HasMetadata;
    if (endStream) flags |= Flags.EndStream;
    const header: Header = {
      streamId,
      frameType: FrameType.Begin,
      flags,
      metadataCount: metaCount,
      payloadLen: payload.length,
    };
    this.pushWithWake(header, payload);
  }

  private sendData(streamId: number, body: Buffer, endStream: boolean): void {
    const header: Header = {
      streamId,
      frameType: FrameType.Data,
      flags: endStream ? Flags.EndStream : 0,
      metadataCount: 0,
      payloadLen: body.length,
    };
    this.pushWithWake(header, body);
  }

  /**
   * Symmetric busy-poll mode. Both ends drop the wake-fd
   * syscalls — producer publishes without writing the
   * signal pipe; consumer spins on the ring head in JS
   * instead of reading the wait pipe. Peer must agree
   * (server runs `Server::listen_shmem_busy_poll_until`).
   */
  setBusyPollUnbounded(on: boolean): void {
    this.busyPollUnbounded = on;
  }

  private pushWithWake(header: Header, payload: Buffer): void {
    if (this.busyPollUnbounded) {
      while (true) {
        const r = ringPush(this.region, 0, header, payload);
        if (r.ok) return;
        if (r.tooLarge) {
          throw new Error("shmem: frame exceeds slot stride");
        }
        // Ring full — busy-spin in JS without any wake.
        // Peer is in symmetric busy-poll mode too.
      }
    }
    while (true) {
      const r = ringPush(this.region, 0, header, payload);
      if (r.ok) {
        wakeWrite(this.signalFd);
        return;
      }
      if (r.tooLarge) {
        throw new Error("shmem: frame exceeds slot stride");
      }
      // Ring full — wake peer and yield briefly.
      wakeWrite(this.signalFd);
      // SAFETY: Node main thread is blocked here. v1
      // accepts this for sequential RPC throughput; v1.x
      // moves to libuv worker.
      this.sleepMicros(10);
    }
  }

  private popBlocking(): Header {
    if (this.busyPollUnbounded) {
      while (true) {
        const r = ringPop(this.region, 0, this.recvBuf);
        if (r.ok) return r.header!;
        // Busy-spin — no wake-fd syscall.
      }
    }
    while (true) {
      const r = ringPop(this.region, 0, this.recvBuf);
      if (r.ok) return r.header!;
      wakeRead(this.waitFd);
    }
  }

  private readResponseStream(
    expectSingle: boolean,
  ): { bodies: Buffer[]; trailing: Metadata } {
    const bodies: Buffer[] = [];
    while (true) {
      const header = this.popBlocking();
      const plen = header.payloadLen;
      switch (header.frameType) {
        case FrameType.Data: {
          if (expectSingle && bodies.length > 0) {
            throw new Error("shmem: extra DATA for unary");
          }
          // Unary fast path: push a zero-copy view into
          // recvBuf instead of allocating a fresh Buffer.
          // Safe only when the caller consumes the body
          // before the next pop() — true for unary
          // (returns immediately on END_STREAM). For
          // server-streaming where bodies accumulate
          // across loop iterations we must copy.
          if (expectSingle && header.flags & Flags.EndStream) {
            bodies.push(this.recvBuf.subarray(0, plen));
            return { bodies, trailing: [] };
          }
          bodies.push(Buffer.from(this.recvBuf.subarray(0, plen)));
          if (header.flags & Flags.EndStream) {
            return { bodies, trailing: [] };
          }
          break;
        }
        case FrameType.End: {
          let inlineBody: Buffer = this.recvBuf.subarray(0, plen);
          let trailing: Metadata = [];
          if (header.flags & Flags.HasMetadata) {
            const md = jsDecodeMetadataBlock(inlineBody, header.metadataCount);
            trailing = this.entriesToMeta(md.entries);
            inlineBody = inlineBody.subarray(md.consumed);
          }
          if (inlineBody.length > 0) {
            if (expectSingle && bodies.length > 0) {
              throw new Error("shmem: extra body on END");
            }
            // Unary: zero-copy view into recvBuf (caller
            // returns immediately); streaming: copy.
            if (expectSingle) {
              bodies.push(inlineBody);
            } else {
              bodies.push(Buffer.from(inlineBody));
            }
          }
          const { status, filtered } = this.extractStatus(trailing);
          if (status && status.code !== StatusCode.Ok) {
            throw new Status(status.code, status.message);
          }
          return { bodies, trailing: filtered };
        }
        case FrameType.Reset: {
          const payload = this.recvBuf.subarray(0, plen);
          const code = payload.readUInt32LE(0);
          const msgLen = payload.readUInt32LE(4);
          const msg = payload
            .subarray(8, 8 + msgLen)
            .toString("utf8");
          throw new Status(code as StatusCode, msg);
        }
        default:
          throw new Error(
            `shmem: unexpected frame type ${header.frameType} from server`,
          );
      }
    }
  }

  private extractStatus(
    trailing: Metadata,
  ): { status: { code: number; message: string } | null; filtered: Metadata } {
    let code: number | null = null;
    let message = "";
    const filtered: Metadata = [];
    for (const e of trailing) {
      if (e.name === ":status" && e.value.length === 4) {
        code = Buffer.from(e.value).readUInt32LE(0);
      } else if (e.name === ":status-msg") {
        message = Buffer.from(e.value).toString("utf8");
      } else {
        filtered.push(e);
      }
    }
    return code !== null
      ? { status: { code, message }, filtered }
      : { status: null, filtered };
  }

  private sleepMicros(us: number): void {
    // Node has no sub-millisecond sleep; busy-spin
    // works for the rare ring-full retry path.
    const deadline = Date.now() + Math.max(1, Math.ceil(us / 1000));
    while (Date.now() < deadline) {
      // spin
    }
    void HEADER_LEN;
  }
}
