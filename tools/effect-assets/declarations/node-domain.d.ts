import type { EventEmitterInstance } from "node:events";

/**
 * Deprecated synchronous domain compatibility. Blitz tracks only explicit
 * scopes and explicitly added emitters; it does not propagate across async
 * continuations or install process-level uncaught-exception handlers.
 */
export declare class Domain implements EventEmitterInstance {
  readonly [key: string]: unknown;
  members: Array<object>;
  domain: Domain | null;

  addListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  on(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  once(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  prependListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  prependOnceListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  removeListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  off(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  removeAllListeners(): this;
  removeAllListeners(eventName: string | symbol): this;
  emit(eventName: string | symbol, ...args: any[]): boolean;
  listeners(eventName: string | symbol): Array<(...args: any[]) => unknown>;
  rawListeners(eventName: string | symbol): Array<(...args: any[]) => unknown>;
  listenerCount(eventName: string | symbol): number;
  listenerCount(eventName: string | symbol, listener: (...args: any[]) => unknown): number;
  eventNames(): Array<string | symbol>;
  getMaxListeners(): number;
  setMaxListeners(n: number): this;

  enter(): void;
  exit(): void;
  add(emitter: object): void;
  remove(emitter: object): void;
  run<Result>(fn: (...args: any[]) => Result, ...args: any[]): Result;
  bind<Func extends (...args: any[]) => any>(callback: Func): Func;
  intercept<Func extends (...args: any[]) => any>(callback: Func): Func;
  /** Legacy cleanup helper retained for old domain fixtures. */
  dispose(): void;
}

export declare const _stack: Domain[];
/** The ESM binding is Node's initial null snapshot, not async context. */
export declare const active: null;
export declare function createDomain(): Domain;
export declare const create: typeof createDomain;

interface DomainModule {
  _stack: Domain[];
  Domain: typeof Domain;
  createDomain: typeof createDomain;
  create: typeof create;
  active: Domain | null | undefined;
}

declare const domain: DomainModule;
export { domain as default };
