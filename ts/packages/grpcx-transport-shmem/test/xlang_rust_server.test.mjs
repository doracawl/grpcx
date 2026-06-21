// Cross-language e2e — TS `@grpcx/transport-shmem` client
// against the Rust `crates/grpcx/examples/shmem_test_server.rs`
// binary. Proves the C++ ring layout + SCM_RIGHTS
// recvmsg byte-for-byte interoperate with the Rust
// shmem server.

import { test } from "node:test";
import assert from "node:assert";
import * as os from "node:os";
import * as path from "node:path";
import * as fs from "node:fs";
import { spawn } from "node:child_process";

import { CallContext } from "@grpcx/core";
import { ShmemConnection } from "../lib/index.js";

// test/ -> grpcx-transport-shmem/ -> packages/ -> ts/ -> repo root
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
  "shmem_test_server",
);

function uniqueSock(tag) {
  return path.join(
    os.tmpdir(),
    `grpcx-shmem-xlang-${process.pid}-${tag}.sock`,
  );
}

async function waitForSocket(p, timeoutMs = 5000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (fs.existsSync(p)) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error(`shmem server never bound ${p}`);
}

async function withServer(tag, fn) {
  const sockPath = uniqueSock(tag);
  fs.rmSync(sockPath, { force: true });
  if (!fs.existsSync(RUST_BIN)) {
    throw new Error(
      `Rust example not built:\n  cargo build --example shmem_test_server --features transport-shmem --release\n  expected: ${RUST_BIN}`,
    );
  }
  const child = spawn(RUST_BIN, [sockPath], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout?.resume();
  child.stderr?.resume();
  try {
    await waitForSocket(sockPath);
    await fn(sockPath);
  } finally {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", () => r()));
    fs.rmSync(sockPath, { force: true });
  }
}

async function* iter(...arrays) {
  for (const a of arrays) yield a;
}

async function collect(stream) {
  const out = [];
  for await (const x of stream) out.push(x);
  return out;
}

test("xlang shmem: unary against Rust server", async () => {
  await withServer("unary", async (sockPath) => {
    const conn = await ShmemConnection.connect(sockPath, "localhost");
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

test("xlang shmem: server-streaming against Rust server", async () => {
  await withServer("server_stream", async (sockPath) => {
    const conn = await ShmemConnection.connect(sockPath, "localhost");
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

test("xlang shmem: client-streaming against Rust server", async () => {
  await withServer("client_stream", async (sockPath) => {
    const conn = await ShmemConnection.connect(sockPath, "localhost");
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

test("xlang shmem: bidi against Rust server", async () => {
  await withServer("bidi", async (sockPath) => {
    const conn = await ShmemConnection.connect(sockPath, "localhost");
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
