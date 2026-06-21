// Cross-language e2e — TS `@grpcx/transport-tcp-mux`
// client against the Rust
// `crates/grpcx/examples/tcp_mux_v2_test_server.rs`
// binary. Validates that the C++ wire codec
// interoperates byte-for-byte with the Rust codec on a
// live TCP socket.

import { test } from "node:test";
import assert from "node:assert";
import * as net from "node:net";
import * as path from "node:path";
import * as fs from "node:fs";
import { spawn } from "node:child_process";

import { CallContext } from "@grpcx/core";
import { TcpMuxV2Connection } from "../lib/index.js";

// test/ -> grpcx-transport-tcp-mux/ -> packages/ -> ts/ -> repo root
const REPO_ROOT = path.resolve(
  import.meta.dirname,
  "..",
  "..",
  "..",
  "..",
);
const RUST_BIN = path.join(
  REPO_ROOT,
  "target",
  "release",
  "examples",
  "tcp_mux_v2_test_server",
);

function ephemeralPort() {
  return new Promise((resolve, reject) => {
    const probe = net.createServer();
    probe.listen(0, "127.0.0.1", () => {
      const port = probe.address().port;
      probe.close(() => resolve(port));
    });
    probe.once("error", reject);
  });
}

async function waitForPort(host, port, timeoutMs = 5000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const ok = await new Promise((res) => {
      const s = net.createConnection({ host, port });
      s.once("connect", () => {
        s.destroy();
        res(true);
      });
      s.once("error", () => res(false));
    });
    if (ok) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error(`tcp-mux-v2 server never bound ${host}:${port}`);
}

async function withServer(tag, fn) {
  const port = await ephemeralPort();
  const addr = `127.0.0.1:${port}`;
  if (!fs.existsSync(RUST_BIN)) {
    throw new Error(
      `Rust example not built. Run:\n  cargo build --example tcp_mux_v2_test_server --features transport-tcp-mux --release\n  expected at: ${RUST_BIN}`,
    );
  }
  const child = spawn(RUST_BIN, [addr], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  // Drain stdout/stderr so the server doesn't deadlock on
  // a full pipe (the libuv stdio:"ignore" SIGPIPE quirk).
  child.stdout?.resume();
  child.stderr?.resume();
  try {
    await waitForPort("127.0.0.1", port);
    await fn("127.0.0.1", port);
  } finally {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", () => r()));
  }
  // tag kept for test-readability + future per-test
  // resource isolation; not currently used in addr.
  void tag;
}

async function* iter(...arrays) {
  for (const a of arrays) yield a;
}

async function collect(stream) {
  const out = [];
  for await (const x of stream) out.push(x);
  return out;
}

test("xlang: unary against Rust tcp-mux-v2 server", async () => {
  await withServer("unary", async (host, port) => {
    const conn = await TcpMuxV2Connection.connect(host, port, "localhost");
    const [body] = await conn.callUnary(
      "ts_e2e.Echo",
      "echo_unary",
      new CallContext(),
      Buffer.from("hello"),
    );
    assert.equal(Buffer.from(body).toString("utf8"), "hello");
    await conn.close();
  });
});

test("xlang: server-streaming against Rust tcp-mux-v2 server", async () => {
  await withServer("server_stream", async (host, port) => {
    const conn = await TcpMuxV2Connection.connect(host, port, "localhost");
    const msgs = await collect(
      conn.callServerStreaming(
        "ts_e2e.Echo",
        "fanout_3",
        new CallContext(),
        Buffer.from("fan"),
      ),
    );
    const greetings = msgs.map((b) => Buffer.from(b).toString("utf8"));
    assert.deepEqual(greetings, ["fan#0", "fan#1", "fan#2"]);
    await conn.close();
  });
});

test("xlang: client-streaming against Rust tcp-mux-v2 server", async () => {
  await withServer("client_stream", async (host, port) => {
    const conn = await TcpMuxV2Connection.connect(host, port, "localhost");
    const [body] = await conn.callClientStreaming(
      "ts_e2e.Echo",
      "concat",
      new CallContext(),
      iter(
        new Uint8Array(Buffer.from("a")),
        new Uint8Array(Buffer.from("b")),
        new Uint8Array(Buffer.from("c")),
      ),
    );
    assert.equal(Buffer.from(body).toString("utf8"), "a+b+c");
    await conn.close();
  });
});

test("xlang: bidi against Rust tcp-mux-v2 server", async () => {
  await withServer("bidi", async (host, port) => {
    const conn = await TcpMuxV2Connection.connect(host, port, "localhost");
    const msgs = await collect(
      conn.callBidi(
        "ts_e2e.Echo",
        "reply",
        new CallContext(),
        iter(
          new Uint8Array(Buffer.from("a")),
          new Uint8Array(Buffer.from("b")),
          new Uint8Array(Buffer.from("c")),
        ),
      ),
    );
    const greetings = msgs.map((b) => Buffer.from(b).toString("utf8"));
    assert.deepEqual(greetings, ["re:a", "re:b", "re:c"]);
    await conn.close();
  });
});
