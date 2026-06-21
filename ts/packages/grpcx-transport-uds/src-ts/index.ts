/**
 * `@grpcx/transport-uds` — AF_UNIX client transport
 * carrying grpcx wire format v2.
 *
 * Hot path:
 *   - Frame headers encode/decode in C++ (@grpcx/native)
 *   - Metadata blocks encode/decode in C++ (@grpcx/native)
 *   - UDS socket I/O via Node `net.Socket` (libuv epoll/kqueue)
 *
 * Nothing performance-critical runs in V8.
 */

import * as net from "node:net";

import { CallContext, Connection, Status, StatusCode } from "@grpcx/core";
import type { Metadata, MetadataEntry } from "@grpcx/core";
import {
  Flags,
  FrameType,
  HEADER_LEN,
  type Header,
  type NativeMetadataEntry,
} from "@grpcx/native";

// JS-side wire header codec. Replaces the N-API
// `encodeHeader` / `decodeHeader` cross-boundary calls.
// Header layout (16 bytes, all LE):
//   stream_id u32 | frame_type u8 | flags u8 |
//   metadata_count u16 | payload_len u32 | reserved u32
// Mirrors `crates/grpcx/src/wire_v2/codec.rs::encode_header`
// /`decode_header`.
function jsEncodeHeader(h: Header): Buffer {
  const out = Buffer.allocUnsafe(HEADER_LEN);
  out.writeUInt32LE(h.streamId, 0);
  out.writeUInt8(h.frameType, 4);
  out.writeUInt8(h.flags, 5);
  out.writeUInt16LE(h.metadataCount, 6);
  out.writeUInt32LE(h.payloadLen, 8);
  out.writeUInt32LE(0, 12);
  return out;
}

function jsDecodeHeader(buf: Buffer): Header {
  if (buf.length < HEADER_LEN) {
    throw new Error("uds: header buffer too short");
  }
  return {
    streamId: buf.readUInt32LE(0),
    frameType: buf.readUInt8(4),
    flags: buf.readUInt8(5),
    metadataCount: buf.readUInt16LE(6),
    payloadLen: buf.readUInt32LE(8),
  };
}

// JS-side metadata block codec. Avoids N-API
// boundary crossings + V8 Uint8Array allocations on the
// hot path. Wire format mirrors `transport-shmem` and
// `wire_v2/metadata.rs`: each entry is
// `name_len u16 LE | value_len u32 LE | name | value`.
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
      throw new Error("uds: truncated metadata entry header");
    }
    const nameLen = buf.readUInt16LE(off);
    const valueLen = buf.readUInt32LE(off + 2);
    off += 6;
    const nameEnd = off + nameLen;
    const valueEnd = nameEnd + valueLen;
    if (valueEnd > buf.length) {
      throw new Error("uds: truncated metadata entry payload");
    }
    entries[i] = {
      name: buf.subarray(off, nameEnd),
      value: buf.subarray(nameEnd, valueEnd),
    };
    off = valueEnd;
  }
  return { entries, consumed: off };
}

const PSEUDO_SERVICE = Buffer.from(":service");
const PSEUDO_METHOD = Buffer.from(":method");
const PSEUDO_KIND = Buffer.from(":kind");
const PSEUDO_STATUS = Buffer.from(":status");
const PSEUDO_STATUS_MSG = Buffer.from(":status-msg");

const KIND_UNARY = 0;
const KIND_SERVER_STREAM = 1;
const KIND_CLIENT_STREAM = 2;
const KIND_BIDI = 3;

interface StreamState {
  bodies: Buffer[];
  trailing: Metadata;
  resolve: (value: { bodies: Buffer[]; trailing: Metadata }) => void;
  reject: (error: Error) => void;
  pushBody?: (b: Buffer) => void;
  pushEnd?: (trailing: Metadata, err: Error | null) => void;
}

export class UdsConnection extends Connection {
  private readonly socket: net.Socket;
  private nextStreamId = 1;
  private readonly streams = new Map<number, StreamState>();
  private readBuf: Buffer = Buffer.alloc(0);
  private closed = false;
  // Per-(service, method, kind) cache of the pre-encoded
  // 3-entry pseudo-header metadata block bytes. Mirrors
  // the v1.7 `transport-shmem` cache; eliminates 3×
  // `Buffer.from()` + `jsEncodeMetadataBlock` per
  // no-user-metadata unary BEGIN.
  private readonly beginMetaCache = new Map<string, Buffer>();

  private constructor(socket: net.Socket) {
    super();
    this.socket = socket;
    socket.on("data", (chunk) => this.onData(chunk));
    socket.on("end", () => this.onClose(new Error("uds: peer closed")));
    socket.on("error", (err) => this.onClose(err));
  }

  /** Connect to `path` (AF_UNIX socket path). The
   *  `authority` value is reserved for future
   *  vhost-style routing; v1 transports do not consult
   *  it. */
  static async connect(path: string, _authority: string): Promise<UdsConnection> {
    return await new Promise<UdsConnection>((resolve, reject) => {
      const sock = net.createConnection({ path });
      sock.once("connect", () => {
        resolve(new UdsConnection(sock));
      });
      sock.once("error", (err) => reject(err));
    });
  }

  override async callUnary(
    service: string,
    method: string,
    ctx: CallContext,
    body: Uint8Array,
  ): Promise<[Uint8Array, Metadata]> {
    const streamId = this.allocStreamId();
    const begin = this.buildBegin(
      streamId,
      service,
      method,
      KIND_UNARY,
      ctx.requestMetadata(),
      Buffer.from(body),
      /*endStream=*/ true,
    );
    this.socket.write(begin);
    const { bodies, trailing } = await this.awaitResponse(streamId);
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
    const begin = this.buildBegin(
      streamId,
      service,
      method,
      KIND_SERVER_STREAM,
      ctx.requestMetadata(),
      Buffer.from(body),
      true,
    );
    this.socket.write(begin);
    const { bodies, trailing } = await this.awaitResponse(streamId);
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
    const begin = this.buildBegin(
      streamId,
      service,
      method,
      KIND_CLIENT_STREAM,
      ctx.requestMetadata(),
      Buffer.from(first ?? new Uint8Array()),
      onlyOne,
    );
    this.socket.write(begin);
    for (let i = 0; i < rest.length; i++) {
      const isLast = i + 1 === rest.length;
      const r = rest[i]!;
      this.socket.write(this.buildData(streamId, Buffer.from(r), isLast));
    }
    const { bodies: respBodies, trailing } = await this.awaitResponse(streamId);
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
    const begin = this.buildBegin(
      streamId,
      service,
      method,
      KIND_BIDI,
      ctx.requestMetadata(),
      Buffer.from(first ?? new Uint8Array()),
      onlyOne,
    );
    this.socket.write(begin);
    for (let i = 0; i < rest.length; i++) {
      const isLast = i + 1 === rest.length;
      const r = rest[i]!;
      this.socket.write(this.buildData(streamId, Buffer.from(r), isLast));
    }
    const { bodies: respBodies, trailing } = await this.awaitResponse(streamId);
    ctx.setTrailing(trailing);
    for (const b of respBodies) yield b;
  }

  override async close(): Promise<void> {
    this.closed = true;
    await new Promise<void>((res) => {
      this.socket.end(() => res());
    });
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

  private buildBegin(
    streamId: number,
    service: string,
    method: string,
    kind: number,
    requestMeta: Metadata,
    body: Buffer,
    endStream: boolean,
  ): Buffer {
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
    const payloadLen = metaBuf.length + body.length;
    let flags = Flags.HasMetadata;
    if (endStream) flags |= Flags.EndStream;
    const headerBuf = jsEncodeHeader({
      streamId,
      frameType: FrameType.Begin,
      flags,
      metadataCount: metaCount,
      payloadLen,
    });
    return Buffer.concat([headerBuf, metaBuf, body]);
  }

  private buildData(streamId: number, body: Buffer, endStream: boolean): Buffer {
    const headerBuf = jsEncodeHeader({
      streamId,
      frameType: FrameType.Data,
      flags: endStream ? Flags.EndStream : 0,
      metadataCount: 0,
      payloadLen: body.length,
    });
    return Buffer.concat([headerBuf, body]);
  }

  private awaitResponse(
    streamId: number,
  ): Promise<{ bodies: Buffer[]; trailing: Metadata }> {
    return new Promise((resolve, reject) => {
      this.streams.set(streamId, {
        bodies: [],
        trailing: [],
        resolve,
        reject,
      });
    });
  }

  private onData(chunk: Buffer): void {
    this.readBuf = this.readBuf.length === 0
      ? chunk
      : Buffer.concat([this.readBuf, chunk]);
    this.drainFrames();
  }

  private drainFrames(): void {
    while (!this.closed) {
      if (this.readBuf.length < HEADER_LEN) return;
      let header: Header;
      try {
        header = jsDecodeHeader(this.readBuf.subarray(0, HEADER_LEN));
      } catch (err) {
        this.closeAllWithError(err as Error);
        return;
      }
      const totalLen = HEADER_LEN + header.payloadLen;
      if (this.readBuf.length < totalLen) return;
      const payload = this.readBuf.subarray(HEADER_LEN, totalLen);
      this.readBuf = this.readBuf.subarray(totalLen);
      this.handleFrame(header, payload);
    }
  }

  private handleFrame(header: Header, payload: Buffer): void {
    // PING / PONG run on stream 0 — ignore (no liveness loop in v1 TS).
    if (header.frameType === FrameType.Ping || header.frameType === FrameType.Pong) {
      return;
    }
    const st = this.streams.get(header.streamId);
    if (!st) return; // unknown / closed stream

    if (header.frameType === FrameType.Data) {
      if (header.payloadLen > 0) {
        st.bodies.push(Buffer.from(payload));
      } else {
        st.bodies.push(Buffer.alloc(0));
      }
      if (header.flags & Flags.EndStream) {
        this.streams.delete(header.streamId);
        st.resolve({ bodies: st.bodies, trailing: [] });
      }
      return;
    }

    if (header.frameType === FrameType.End) {
      let inlineBody: Buffer = payload;
      let trailing: Metadata = [];
      if (header.flags & Flags.HasMetadata) {
        const md = jsDecodeMetadataBlock(payload, header.metadataCount);
        trailing = this.entriesToMeta(md.entries);
        inlineBody = payload.subarray(md.consumed);
      }
      if (inlineBody.length > 0) {
        st.bodies.push(Buffer.from(inlineBody));
      }
      // Extract :status / :status-msg
      const { status, filtered } = this.extractStatus(trailing);
      this.streams.delete(header.streamId);
      if (status && status.code !== StatusCode.Ok) {
        st.reject(new Status(status.code, status.message));
      } else {
        st.resolve({ bodies: st.bodies, trailing: filtered });
      }
      return;
    }

    if (header.frameType === FrameType.Reset) {
      const code = payload.length >= 4
        ? payload.readUInt32LE(0)
        : (StatusCode.Internal as number);
      const msgLen = payload.length >= 8 ? payload.readUInt32LE(4) : 0;
      const msg = payload.length >= 8 + msgLen
        ? payload.subarray(8, 8 + msgLen).toString("utf8")
        : "";
      this.streams.delete(header.streamId);
      st.reject(new Status(code as StatusCode, msg));
      return;
    }

    if (header.frameType === FrameType.Begin) {
      this.streams.delete(header.streamId);
      st.reject(new Error("uds: server sent BEGIN to client"));
      return;
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
        const buf = Buffer.from(e.value);
        code = buf.readUInt32LE(0);
      } else if (e.name === ":status-msg") {
        message = Buffer.from(e.value).toString("utf8");
      } else {
        filtered.push(e);
      }
    }
    return code !== null ? { status: { code, message }, filtered } : { status: null, filtered };
  }

  private onClose(err: Error): void {
    if (this.closed) return;
    this.closed = true;
    this.closeAllWithError(err);
  }

  private closeAllWithError(err: Error): void {
    for (const [, st] of this.streams) {
      st.reject(err);
    }
    this.streams.clear();
  }
}
