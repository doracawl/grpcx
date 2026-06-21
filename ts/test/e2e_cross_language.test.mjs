// T5.3 — Cross-language e2e.
//
// TS clients via `@grpcx/transport-{uds,shmem,tcp-mux}`
// against the three Rust example servers
// (`uds_test_server`, `shmem_test_server`,
// `tcp_mux_v2_test_server`). 4 RPC kinds × 3 transports
// = 12 cross-language RPCs.
//
// Mirrors the T5.2 Rust-only test
// (`crates/grpcx/tests/e2e_three_transports.rs`) but
// from the TS side: same service shape (`ts_e2e.Echo`,
// methods `echo_unary` / `fanout_3` / `concat` /
// `reply`), same expected byte sequences.
//
// Run: `node --test ts/test/e2e_cross_language.test.mjs`
// after the workspace has been built and the Rust
// examples are present at
// `target/release/examples/{uds,shmem,tcp_mux_v2}_test_server`.

import { test } from "node:test";
import assert from "node:assert";
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import * as fs from "node:fs";
import { spawn } from "node:child_process";

import { CallContext } from "@grpcx/core";
import { UdsConnection } from "@grpcx/transport-uds";
import { ShmemConnection } from "@grpcx/transport-shmem";
import { TcpMuxV2Connection } from "@grpcx/transport-tcp-mux";

// test/ -> ts/ -> repo root
const REPO_ROOT = path.resolve(import.meta.dirname, "..", "..");
const RUST_BIN = (name) =>
  path.join(REPO_ROOT, "target", "release", "examples", name);

// ============================================================
// Address helpers — fresh paths / ports per spawn.
// ============================================================

function uniqueSock(tag) {
  return path.join(
    os.tmpdir(),
    `grpcx-xlang-${tag}-${process.pid}-${Date.now()}.sock`,
  );
}

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

async function waitForFile(p, timeoutMs = 5000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (fs.existsSync(p)) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error(`server never bound ${p}`);
}

async function waitForTcp(host, port, timeoutMs = 5000) {
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
  throw new Error(`tcp server never bound ${host}:${port}`);
}

function requireBin(name) {
  const bin = RUST_BIN(name);
  if (!fs.existsSync(bin)) {
    throw new Error(
      `Rust example not built. Run:\n  cargo build --release --features grpcx/transport-shmem,grpcx/transport-tcp-mux --examples\n  expected at: ${bin}`,
    );
  }
  return bin;
}

function spawnDrained(bin, args) {
  const child = spawn(bin, args, { stdio: ["ignore", "pipe", "pipe"] });
  // libuv stdio:"ignore" + Rust eprintln! can SIGPIPE.
  child.stdout?.resume();
  child.stderr?.resume();
  return child;
}

async function killChild(child) {
  child.kill("SIGTERM");
  await new Promise((r) => child.once("exit", () => r()));
}

// ============================================================
// Per-transport runners.
// ============================================================

async function withUdsConn(fn) {
  const bin = requireBin("uds_test_server");
  const sockPath = uniqueSock("uds");
  fs.rmSync(sockPath, { force: true });
  const child = spawnDrained(bin, [sockPath]);
  try {
    await waitForFile(sockPath);
    const conn = await UdsConnection.connect(sockPath, "localhost");
    try {
      await fn(conn);
    } finally {
      await conn.close();
    }
  } finally {
    await killChild(child);
    fs.rmSync(sockPath, { force: true });
  }
}

async function withShmemConn(fn) {
  const bin = requireBin("shmem_test_server");
  const sockPath = uniqueSock("shmem");
  fs.rmSync(sockPath, { force: true });
  const child = spawnDrained(bin, [sockPath]);
  try {
    await waitForFile(sockPath);
    const conn = await ShmemConnection.connect(sockPath, "localhost");
    try {
      await fn(conn);
    } finally {
      await conn.close();
    }
  } finally {
    await killChild(child);
    fs.rmSync(sockPath, { force: true });
  }
}

async function withTcpMuxConn(fn) {
  const bin = requireBin("tcp_mux_v2_test_server");
  const port = await ephemeralPort();
  const addr = `127.0.0.1:${port}`;
  const child = spawnDrained(bin, [addr]);
  try {
    await waitForTcp("127.0.0.1", port);
    const conn = await TcpMuxV2Connection.connect("127.0.0.1", port, "localhost");
    try {
      await fn(conn);
    } finally {
      await conn.close();
    }
  } finally {
    await killChild(child);
  }
}

// ============================================================
// Transport-independent RPC body — 4 kinds, identical
// expected outputs across all three transports.
// ============================================================

async function* iter(...arrays) {
  for (const a of arrays) yield a;
}

async function collect(stream) {
  const out = [];
  for await (const x of stream) out.push(x);
  return out;
}

async function runFourKinds(conn, label) {
  // 1) Unary
  const [u] = await conn.callUnary(
    "ts_e2e.Echo",
    "echo_unary",
    new CallContext(),
    Buffer.from("hello"),
  );
  assert.equal(Buffer.from(u).toString("utf8"), "hello", `${label}: unary`);

  // 2) Server stream
  const ss = await collect(
    conn.callServerStreaming(
      "ts_e2e.Echo",
      "fanout_3",
      new CallContext(),
      Buffer.from("fan"),
    ),
  );
  assert.deepEqual(
    ss.map((b) => Buffer.from(b).toString("utf8")),
    ["fan#0", "fan#1", "fan#2"],
    `${label}: server stream`,
  );

  // 3) Client stream
  const [cs] = await conn.callClientStreaming(
    "ts_e2e.Echo",
    "concat",
    new CallContext(),
    iter(
      new Uint8Array(Buffer.from("a")),
      new Uint8Array(Buffer.from("b")),
      new Uint8Array(Buffer.from("c")),
    ),
  );
  assert.equal(
    Buffer.from(cs).toString("utf8"),
    "a+b+c",
    `${label}: client stream`,
  );

  // 4) Bidi
  const bi = await collect(
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
  assert.deepEqual(
    bi.map((b) => Buffer.from(b).toString("utf8")),
    ["re:a", "re:b", "re:c"],
    `${label}: bidi`,
  );
}

// ============================================================
// The four-kinds × three-transports gate (12 RPCs).
// ============================================================

test("xlang: TS client × Rust UDS server, 4 RPC kinds", async () => {
  await withUdsConn((conn) => runFourKinds(conn, "uds"));
});

test("xlang: TS client × Rust shmem server, 4 RPC kinds", async () => {
  await withShmemConn((conn) => runFourKinds(conn, "shmem"));
});

test("xlang: TS client × Rust tcp-mux-v2 server, 4 RPC kinds", async () => {
  await withTcpMuxConn((conn) => runFourKinds(conn, "tcp-mux-v2"));
});
