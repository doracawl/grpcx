/**
 * `@grpcx/core` — public TypeScript surface for the
 * grpcx internal-RPC v2 stack. Concrete transports live
 * in sibling packages (`@grpcx/transport-uds`, etc.) and
 * route through `@grpcx/native` for hot-path codec + I/O.
 *
 * RFC: ../../../../_dev/RFC-internal-rpc-arch.md
 * Wire format: ../../../../_dev/SPEC-wire-v2.md
 */

export { CallContext } from "./call-context.js";
export { Connection } from "./connection.js";
export { Server, type ServiceDispatcher } from "./server.js";
export { Status, StatusCode } from "./status.js";
export type { Metadata, MetadataEntry, PeerCred } from "./types.js";
