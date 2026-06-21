import type { CallContext } from "./call-context.js";

/**
 * Per-service dispatcher signature. Generated
 * `registerXxx(server, impl)` functions wrap a handler
 * object into one of these.
 *
 * Returns the list of response message bodies — one for
 * unary / client-streaming, N for server-/bidi-streaming.
 */
export type ServiceDispatcher = (
  method: string,
  ctx: CallContext,
  requestBodies: Uint8Array[],
) => Promise<Uint8Array[]>;

/**
 * Server-side dispatcher registry. Transports'
 * `listenXxx` accept loops decode wire frames, look up
 * the service by name, and invoke its dispatcher.
 */
export class Server {
  private readonly services = new Map<string, ServiceDispatcher>();

  /** Register a service by its fully-qualified
   *  `:service` name. Throws if the name is already
   *  registered. */
  registerService(serviceName: string, dispatcher: ServiceDispatcher): void {
    if (this.services.has(serviceName)) {
      throw new Error(`grpcx: service already registered: ${serviceName}`);
    }
    this.services.set(serviceName, dispatcher);
  }

  /** Look up a dispatcher by service name. Returns
   *  `undefined` if unknown. */
  findService(serviceName: string): ServiceDispatcher | undefined {
    return this.services.get(serviceName);
  }

  /** Number of registered services. */
  serviceCount(): number {
    return this.services.size;
  }
}
