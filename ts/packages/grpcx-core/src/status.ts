/**
 * gRPC status codes (0..16). Mirror of the Rust
 * `StatusCode` enum in `grpcx::StatusCode`.
 */
export enum StatusCode {
  Ok = 0,
  Cancelled = 1,
  Unknown = 2,
  InvalidArgument = 3,
  DeadlineExceeded = 4,
  NotFound = 5,
  AlreadyExists = 6,
  PermissionDenied = 7,
  ResourceExhausted = 8,
  FailedPrecondition = 9,
  Aborted = 10,
  OutOfRange = 11,
  Unimplemented = 12,
  Internal = 13,
  Unavailable = 14,
  DataLoss = 15,
  Unauthenticated = 16,
}

/**
 * Application-level RPC status — carried in the `:status`
 * pseudo-header on `END` frames, or in `RESET` payloads.
 *
 * `Status` extends `Error` so handlers can `throw new
 * Status(StatusCode.PermissionDenied, '...')` and the
 * server framework converts it to a wire `RESET` frame.
 */
export class Status extends Error {
  readonly code: StatusCode;
  override readonly message: string;

  constructor(code: StatusCode, message: string = "") {
    super(`grpcx status ${code}: ${message}`);
    this.name = "Status";
    this.code = code;
    this.message = message;
  }
}
