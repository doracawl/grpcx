// KAT — TS / C++ side reproduces SPEC § 9 byte-for-byte.
//
// T5.1 of `_dev/IMPLEMENTATION-PLAN.md` makes
// `_dev/kat-vectors/9.x-*.hex` the single source of
// truth — both this test and the Rust `kat_wire_v2.rs`
// load the same files (whitespace-tolerant,
// `#`-comments-to-EOL). If the SPEC § 9 bytes change,
// update the hex files; both codecs must keep
// round-tripping them.
//
// Run via `npm test --workspace @grpcx/native` from the
// ts/ workspace root.

import { test } from "node:test";
import assert from "node:assert";
import * as fs from "node:fs";
import * as path from "node:path";
const bytesEq = (a, b) => assert.deepEqual(Array.from(a), Array.from(b));

import {
  HEADER_LEN,
  FrameType,
  Flags,
  encodeHeader,
  decodeHeader,
  encodeMetadataBlock,
  decodeMetadataBlock,
} from "../lib/index.js";

// test/ -> grpcx-native/ -> packages/ -> ts/ -> repo root
const REPO_ROOT = path.resolve(
  import.meta.dirname,
  "..",
  "..",
  "..",
  "..",
);
const KAT_DIR = path.join(REPO_ROOT, "_dev", "kat-vectors");

/** Load a KAT vector from `_dev/kat-vectors/{name}`.
 *  `#` starts an end-of-line comment; whitespace
 *  (including newlines) is ignored; every other token
 *  is a 2-hex-digit byte. Matches the Rust `load_kat`
 *  in `crates/grpcx/tests/kat_wire_v2.rs`. */
function loadKat(name) {
  const text = fs.readFileSync(path.join(KAT_DIR, name), "utf8");
  const bytes = [];
  for (const line of text.split("\n")) {
    const noComment = line.split("#")[0] ?? "";
    for (const tok of noComment.split(/\s+/)) {
      if (tok.length === 0) continue;
      assert.equal(tok.length, 2, `hex byte must be 2 chars, got ${tok}`);
      const b = parseInt(tok, 16);
      assert.ok(!Number.isNaN(b), `bad hex byte ${tok}`);
      bytes.push(b);
    }
  }
  return Buffer.from(bytes);
}

const KAT_9_1 = loadKat("9.1-unary-begin.hex");
const KAT_9_2 = loadKat("9.2-end-no-trailers.hex");
const KAT_9_3 = loadKat("9.3-reset-cancelled.hex");
const KAT_9_4 = loadKat("9.4-ping.hex");

test("KAT 9.1 — minimal unary BEGIN with inline body, header decode", () => {
  assert.equal(HEADER_LEN, 16);
  assert.equal(KAT_9_1.length, 81);

  const header = decodeHeader(KAT_9_1.subarray(0, HEADER_LEN));
  assert.equal(header.streamId, 1);
  assert.equal(header.frameType, FrameType.Begin);
  assert.equal(header.flags, Flags.HasMetadata | Flags.EndStream);
  assert.equal(header.metadataCount, 3);
  assert.equal(header.payloadLen, 65);

  const payload = KAT_9_1.subarray(HEADER_LEN);
  const md = decodeMetadataBlock(payload, header.metadataCount);
  assert.equal(md.entries.length, 3);
  bytesEq(md.entries[0]?.name, Buffer.from(":service"));
  bytesEq(md.entries[0]?.value, Buffer.from("demo.v1.Greeter"));
  bytesEq(md.entries[1]?.name, Buffer.from(":method"));
  bytesEq(md.entries[1]?.value, Buffer.from("SayHello"));
  bytesEq(md.entries[2]?.name, Buffer.from(":kind"));
  bytesEq(md.entries[2]?.value, Buffer.from([0]));
  assert.equal(md.consumed, 62);
  bytesEq(payload.subarray(md.consumed), Buffer.from("Hi!"));
});

test("KAT 9.1 — encode round trip matches KAT bytes", () => {
  const header = encodeHeader({
    streamId: 1,
    frameType: FrameType.Begin,
    flags: Flags.HasMetadata | Flags.EndStream,
    metadataCount: 3,
    payloadLen: 65,
  });
  const meta = encodeMetadataBlock([
    { name: Buffer.from(":service"), value: Buffer.from("demo.v1.Greeter") },
    { name: Buffer.from(":method"), value: Buffer.from("SayHello") },
    { name: Buffer.from(":kind"), value: Buffer.from([0]) },
  ]);
  const wire = Buffer.concat([header, meta, Buffer.from("Hi!")]);
  bytesEq(wire, KAT_9_1);
});

test("KAT 9.2 — minimal END round trip", () => {
  const h = decodeHeader(KAT_9_2);
  assert.equal(h.streamId, 1);
  assert.equal(h.frameType, FrameType.End);
  assert.equal(h.flags, Flags.EndStream);
  assert.equal(h.metadataCount, 0);
  assert.equal(h.payloadLen, 0);
  const enc = encodeHeader(h);
  bytesEq(enc, KAT_9_2);
});

test("KAT 9.3 — RESET with Cancelled", () => {
  assert.equal(KAT_9_3.length, 35);
  const h = decodeHeader(KAT_9_3.subarray(0, HEADER_LEN));
  assert.equal(h.streamId, 5);
  assert.equal(h.frameType, FrameType.Reset);
  assert.equal(h.flags, Flags.EndStream);
  assert.equal(h.payloadLen, 19);
  const payload = KAT_9_3.subarray(HEADER_LEN);
  assert.equal(payload.readUInt32LE(0), 1); // Cancelled
  assert.equal(payload.readUInt32LE(4), 11); // msg_len
  assert.equal(payload.subarray(8, 8 + 11).toString("utf8"), "client gone");
});

test("KAT 9.4 — PING with payload", () => {
  const h = decodeHeader(KAT_9_4.subarray(0, HEADER_LEN));
  assert.equal(h.streamId, 0);
  assert.equal(h.frameType, FrameType.Ping);
  assert.equal(h.flags, 0);
  assert.equal(h.payloadLen, 4);
  bytesEq(
    KAT_9_4.subarray(HEADER_LEN),
    Buffer.from([0x42, 0x13, 0x37, 0x00]),
  );
});

test("rejects unknown frame type", () => {
  const bad = Buffer.alloc(HEADER_LEN);
  bad[4] = 0xff;
  assert.throws(() => decodeHeader(bad), /unknown frame type/);
});

test("rejects buffer too short", () => {
  assert.throws(
    () => decodeHeader(Buffer.alloc(HEADER_LEN - 1)),
    /buffer too short/,
  );
});

test("rejects zero name_len in metadata block", () => {
  const bad = Buffer.from([0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
  assert.throws(
    () => decodeMetadataBlock(bad, 1),
    /invalid metadata name_len/,
  );
});

test("rejects oversized name_len", () => {
  const bad = Buffer.from([0x01, 0x04, 0x00, 0x00, 0x00, 0x00]); // 0x0401 = 1025
  assert.throws(
    () => decodeMetadataBlock(bad, 1),
    /invalid metadata name_len/,
  );
});
