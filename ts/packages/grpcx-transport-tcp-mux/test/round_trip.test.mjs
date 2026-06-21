// TS-side round-trip — `@grpcx/transport-tcp-mux`
// client against a minimal Node `net.Server` impl that
// decodes v2 frames with `@grpcx/native` and echoes the
// request. Validates the client's wire encoding + frame
// dispatch. Cross-language e2e (TS client vs Rust
// `Server::listen_tcp_mux_v2_until`) lives in
// `xlang_rust_server.test.mjs`.

import { test } from "node:test";
import assert from "node:assert";
import * as net from "node:net";

import { CallContext } from "@grpcx/core";
import {
  Flags,
  FrameType,
  HEADER_LEN,
  decodeHeader,
  decodeMetadataBlock,
  encodeHeader,
  encodeMetadataBlock,
} from "@grpcx/native";

import { TcpMuxV2Connection } from "../lib/index.js";

const PSEUDO_STATUS = Buffer.from(":status");

function buildEnd(streamId, body, trailing = []) {
  const trailers = [
    { name: PSEUDO_STATUS, value: Buffer.from([0, 0, 0, 0]) },
    ...trailing,
  ];
  const metaBuf = encodeMetadataBlock(trailers);
  const payload = Buffer.concat([metaBuf, body]);
  const header = encodeHeader({
    streamId,
    frameType: FrameType.End,
    flags: Flags.HasMetadata | Flags.EndStream,
    metadataCount: trailers.length,
    payloadLen: payload.length,
  });
  return Buffer.concat([header, payload]);
}

function buildData(streamId, body) {
  const header = encodeHeader({
    streamId,
    frameType: FrameType.Data,
    flags: 0,
    metadataCount: 0,
    payloadLen: body.length,
  });
  return Buffer.concat([header, body]);
}

function startEchoServer() {
  return new Promise((resolve) => {
    const server = net.createServer((socket) => {
      socket.setNoDelay(true);
      let buf = Buffer.alloc(0);
      const streams = new Map();
      socket.on("data", (chunk) => {
        buf = Buffer.concat([buf, chunk]);
        while (buf.length >= HEADER_LEN) {
          const header = decodeHeader(buf.subarray(0, HEADER_LEN));
          if (buf.length < HEADER_LEN + header.payloadLen) break;
          const payload = buf.subarray(
            HEADER_LEN,
            HEADER_LEN + header.payloadLen,
          );
          buf = buf.subarray(HEADER_LEN + header.payloadLen);
          handleFrame(header, payload);
        }
      });
      function handleFrame(header, payload) {
        if (header.frameType === FrameType.Begin) {
          const md = decodeMetadataBlock(payload, header.metadataCount);
          let method = "";
          let kind = 0;
          for (const e of md.entries) {
            const n = Buffer.from(e.name).toString("utf8");
            if (n === ":method") method = Buffer.from(e.value).toString("utf8");
            else if (n === ":kind") kind = Buffer.from(e.value)[0];
          }
          const body = payload.subarray(md.consumed);
          const st = { method, kind, bodies: [] };
          if (body.length > 0) st.bodies.push(Buffer.from(body));
          streams.set(header.streamId, st);
          if (header.flags & Flags.EndStream) dispatch(header.streamId);
        } else if (header.frameType === FrameType.Data) {
          const st = streams.get(header.streamId);
          if (!st) return;
          if (header.payloadLen > 0) {
            st.bodies.push(Buffer.from(payload));
          }
          if (header.flags & Flags.EndStream) dispatch(header.streamId);
        }
      }
      function dispatch(streamId) {
        const st = streams.get(streamId);
        if (!st) return;
        streams.delete(streamId);
        const { method, bodies } = st;
        switch (method) {
          case "echo_unary": {
            socket.write(buildEnd(streamId, bodies[0] ?? Buffer.alloc(0)));
            break;
          }
          case "fanout_3": {
            for (let i = 0; i < 3; i++) {
              const b = Buffer.concat([
                bodies[0] ?? Buffer.alloc(0),
                Buffer.from([0x23, 0x30 + i]),
              ]);
              socket.write(buildData(streamId, b));
            }
            socket.write(buildEnd(streamId, Buffer.alloc(0)));
            break;
          }
          case "concat": {
            const joined = Buffer.concat(
              bodies.flatMap((b, i) => (i > 0 ? [Buffer.from([0x2b]), b] : [b])),
            );
            socket.write(buildEnd(streamId, joined));
            break;
          }
          case "reply": {
            for (const b of bodies) {
              const out = Buffer.concat([Buffer.from("re:"), b]);
              socket.write(buildData(streamId, out));
            }
            socket.write(buildEnd(streamId, Buffer.alloc(0)));
            break;
          }
          default: {
            socket.write(buildEnd(streamId, Buffer.alloc(0)));
            break;
          }
        }
      }
    });
    // Ephemeral port — kernel-assigned.
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      resolve({ server, port: addr.port });
    });
  });
}

async function* iter(...arrays) {
  for (const a of arrays) yield a;
}

async function collect(stream) {
  const out = [];
  for await (const x of stream) out.push(x);
  return out;
}

test("unary round-trip via TcpMuxV2Connection", async () => {
  const { server, port } = await startEchoServer();
  try {
    const conn = await TcpMuxV2Connection.connect("127.0.0.1", port, "localhost");
    const ctx = new CallContext();
    const [body, _trailing] = await conn.callUnary(
      "ts.Echo",
      "echo_unary",
      ctx,
      Buffer.from("hello"),
    );
    assert.deepEqual(Array.from(body), Array.from(Buffer.from("hello")));
    await conn.close();
  } finally {
    server.close();
  }
});

test("server-streaming via TcpMuxV2Connection", async () => {
  const { server, port } = await startEchoServer();
  try {
    const conn = await TcpMuxV2Connection.connect("127.0.0.1", port, "localhost");
    const stream = conn.callServerStreaming(
      "ts.Echo",
      "fanout_3",
      new CallContext(),
      Buffer.from("fan"),
    );
    const msgs = await collect(stream);
    assert.equal(msgs.length, 3);
    assert.equal(Buffer.from(msgs[0]).toString("utf8"), "fan#0");
    assert.equal(Buffer.from(msgs[1]).toString("utf8"), "fan#1");
    assert.equal(Buffer.from(msgs[2]).toString("utf8"), "fan#2");
    await conn.close();
  } finally {
    server.close();
  }
});

test("client-streaming via TcpMuxV2Connection", async () => {
  const { server, port } = await startEchoServer();
  try {
    const conn = await TcpMuxV2Connection.connect("127.0.0.1", port, "localhost");
    const reqs = iter(
      new Uint8Array(Buffer.from("a")),
      new Uint8Array(Buffer.from("b")),
      new Uint8Array(Buffer.from("c")),
    );
    const [body, _trailing] = await conn.callClientStreaming(
      "ts.Echo",
      "concat",
      new CallContext(),
      reqs,
    );
    assert.equal(Buffer.from(body).toString("utf8"), "a+b+c");
    await conn.close();
  } finally {
    server.close();
  }
});

test("bidi via TcpMuxV2Connection", async () => {
  const { server, port } = await startEchoServer();
  try {
    const conn = await TcpMuxV2Connection.connect("127.0.0.1", port, "localhost");
    const reqs = iter(
      new Uint8Array(Buffer.from("a")),
      new Uint8Array(Buffer.from("b")),
      new Uint8Array(Buffer.from("c")),
    );
    const stream = conn.callBidi("ts.Echo", "reply", new CallContext(), reqs);
    const msgs = await collect(stream);
    assert.equal(msgs.length, 3);
    assert.equal(Buffer.from(msgs[0]).toString("utf8"), "re:a");
    assert.equal(Buffer.from(msgs[1]).toString("utf8"), "re:b");
    assert.equal(Buffer.from(msgs[2]).toString("utf8"), "re:c");
    await conn.close();
  } finally {
    server.close();
  }
});
