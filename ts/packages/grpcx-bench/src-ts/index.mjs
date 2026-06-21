// T5.4 TS-side bench harness — RFC § 8 gates for the
// TS + N-API surface.
//
// Spawns the matching Rust example server
// (`uds_test_server` / `shmem_test_server`) from
// `target/release/examples/`, runs N unary calls of
// `payload`-byte body through the TS transport
// package, and emits a JSON result file under
// `packages/grpcx-bench/results/`.
//
// Two gates only — see README.md.

import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import * as fs from "node:fs";
import * as process from "node:process";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { CallContext } from "@grpcx/core";
import { UdsConnection } from "@grpcx/transport-uds";
import { ShmemConnection } from "@grpcx/transport-shmem";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// src-ts/ -> grpcx-bench/ -> packages/ -> ts/ -> repo root
const REPO_ROOT = path.resolve(__dirname, "..", "..", "..", "..");
const RUST_BIN = (name) =>
  path.join(REPO_ROOT, "target", "release", "examples", name);

// ============================================================
// CLI
// ============================================================

function parseArgs(argv) {
  const out = {
    gate: "all",
    samples: 10_000,
    payload: 64,
    output: null,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--gate") out.gate = argv[++i];
    else if (a === "--samples") out.samples = Number(argv[++i]);
    else if (a === "--payload") out.payload = Number(argv[++i]);
    else if (a === "--output") out.output = argv[++i];
    else if (a === "-h" || a === "--help") {
      console.log(`@grpcx/bench — T5.4 TS-side harness
  --gate <name>     all (default) | shmem-rtt | uds-rtt
  --samples N       RTT samples after warm-up (default 10000)
  --payload N       echo body bytes (default 64)
  --output FILE     result-file path override`);
      process.exit(0);
    } else {
      throw new Error(`unknown arg: ${a}`);
    }
  }
  return out;
}

// ============================================================
// Per-gate runners
// ============================================================

function uniqueSock(tag) {
  return path.join(
    os.tmpdir(),
    `grpcx-bench-${tag}-${process.pid}-${Date.now()}.sock`,
  );
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

async function waitForFile(p, timeoutMs = 5000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (fs.existsSync(p)) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error(`server never bound ${p}`);
}

function spawnDrained(bin, args, env) {
  const child = spawn(bin, args, {
    stdio: ["ignore", "pipe", "pipe"],
    env: env ? { ...process.env, ...env } : process.env,
  });
  child.stdout?.resume();
  child.stderr?.resume();
  return child;
}

async function killChild(child) {
  child.kill("SIGTERM");
  await new Promise((r) => child.once("exit", () => r()));
}

function stats(nanos) {
  nanos.sort((a, b) => a - b);
  const n = nanos.length;
  const median = nanos[Math.floor(n / 2)];
  const p99 = nanos[Math.floor(n * 0.99)];
  const sum = nanos.reduce((a, b) => a + b, 0);
  const mean = Math.round(sum / n);
  return {
    samples: n,
    min_ns: nanos[0],
    median_ns: median,
    p99_ns: p99,
    max_ns: nanos[n - 1],
    mean_ns: mean,
  };
}

async function rttGate({ ConnFactory, binName, binEnv, label, args }) {
  const bin = requireBin(binName);
  const sock = uniqueSock(label);
  fs.rmSync(sock, { force: true });
  const child = spawnDrained(bin, [sock], binEnv);
  try {
    await waitForFile(sock);
    const conn = await ConnFactory(sock);
    try {
      const body = Buffer.alloc(args.payload, 0x42);
      // Warm-up.
      for (let i = 0; i < 2_000; i++) {
        await conn.callUnary(
          "ts_e2e.Echo",
          "echo_unary",
          new CallContext(),
          body,
        );
      }
      const nanos = new Array(args.samples);
      for (let i = 0; i < args.samples; i++) {
        const t0 = process.hrtime.bigint();
        await conn.callUnary(
          "ts_e2e.Echo",
          "echo_unary",
          new CallContext(),
          body,
        );
        nanos[i] = Number(process.hrtime.bigint() - t0);
      }
      return stats(nanos);
    } finally {
      await conn.close();
    }
  } finally {
    await killChild(child);
    fs.rmSync(sock, { force: true });
  }
}

const GATES = {
  "shmem-rtt": (args) =>
    rttGate({
      ConnFactory: async (sock) => {
        const c = await ShmemConnection.connect(sock, "bench");
        // Symmetric busy-poll: peer is `shmem_test_server`
        // spawned with `SHMEM_BUSY_POLL=1` below, which
        // runs `listen_shmem_busy_poll_until`. Drops the
        // per-RPC wake-fd syscalls (~3 µs on lx64).
        c.setBusyPollUnbounded(true);
        return c;
      },
      binName: "shmem_test_server",
      binEnv: { SHMEM_BUSY_POLL: "1" },
      label: "shmem",
      args,
    }),
  "uds-rtt": (args) =>
    rttGate({
      ConnFactory: (sock) => UdsConnection.connect(sock, "bench"),
      binName: "uds_test_server",
      label: "uds",
      args,
    }),
};

// ============================================================
// Fingerprint + output
// ============================================================

function fingerprint() {
  let hostname = "unknown";
  try {
    hostname = (spawnSync("hostname").stdout?.toString() ?? "").trim();
  } catch {}
  if (!hostname) hostname = "unknown";
  let gitSha = "unknown";
  try {
    const out = spawnSync("git", ["rev-parse", "--short", "HEAD"], {
      cwd: REPO_ROOT,
    });
    gitSha = (out.stdout?.toString() ?? "").trim() || "unknown";
  } catch {}
  return {
    hostname,
    os: process.platform,
    arch: process.arch,
    node_version: process.version,
    cpu_count: os.cpus().length,
    git_sha: gitSha,
    bench_timestamp: process.env.BENCH_TIMESTAMP ?? "0",
  };
}

function resultsPath(args) {
  if (args.output) return args.output;
  const fp = fingerprint();
  const dir = path.join(__dirname, "..", "results");
  return path.join(dir, `${fp.hostname}-${fp.arch}-${fp.bench_timestamp}.json`);
}

// ============================================================
// Main
// ============================================================

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const which =
    args.gate === "all" ? Object.keys(GATES) : [args.gate];
  if (!which.every((g) => GATES[g])) {
    throw new Error(`unknown gate: ${args.gate}`);
  }
  const root = {
    fingerprint: fingerprint(),
    config: { samples: args.samples, payload: args.payload },
  };
  for (const g of which) {
    process.stderr.write(`== running gate: ${g}\n`);
    root[g] = await GATES[g](args);
  }
  const out = resultsPath(args);
  fs.mkdirSync(path.dirname(out), { recursive: true });
  fs.writeFileSync(out, JSON.stringify(root) + "\n");
  console.log(out);
  console.log(JSON.stringify(root));
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
