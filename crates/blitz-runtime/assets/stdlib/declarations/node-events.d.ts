export type EventName = string | symbol;
export type EventListener = (...args: any[]) => unknown;

export interface EventEmitterOptions {
  captureRejections?: boolean;
}

export interface EventEmitterInstance {
  addListener(eventName: EventName, listener: EventListener): this;
  on(eventName: EventName, listener: EventListener): this;
  once(eventName: EventName, listener: EventListener): this;
  prependListener(eventName: EventName, listener: EventListener): this;
  prependOnceListener(eventName: EventName, listener: EventListener): this;
  removeListener(eventName: EventName, listener: EventListener): this;
  off(eventName: EventName, listener: EventListener): this;
  removeAllListeners(): this;
  removeAllListeners(eventName: EventName): this;
  emit(eventName: EventName, ...args: any[]): boolean;
  listeners(eventName: EventName): EventListener[];
  rawListeners(eventName: EventName): EventListener[];
  listenerCount(eventName: EventName): number;
  listenerCount(eventName: EventName, listener: EventListener): number;
  eventNames(): EventName[];
  getMaxListeners(): number;
  setMaxListeners(n: number): this;
}

export interface EventEmitterConstructor {
  new (options?: EventEmitterOptions): EventEmitterInstance;
  readonly EventEmitter: EventEmitterConstructor;
  readonly usingDomains: boolean;
  defaultMaxListeners: number;
  captureRejections: boolean;
  readonly errorMonitor: typeof errorMonitor;
  readonly captureRejectionSymbol: typeof captureRejectionSymbol;
  readonly once: typeof once;
  readonly on: typeof on;
  readonly getEventListeners: typeof getEventListeners;
  readonly getMaxListeners: typeof getMaxListeners;
  readonly listenerCount: typeof listenerCount;
  readonly setMaxListeners: typeof setMaxListeners;
  readonly addAbortListener: typeof addAbortListener;
}

export interface EventEmitterAsyncIterator extends AsyncIterableIterator<any[]> {}
export interface AbortListenerDisposable {
  dispose(): void;
  readonly [key: symbol]: (() => void) | undefined;
}

export declare function addAbortListener(signal: AbortSignal, listener: (event: Event) => void): AbortListenerDisposable;
export declare function once(emitter: EventEmitterInstance, eventName: EventName): Promise<any[]>;
export declare function once(emitter: EventEmitterInstance, eventName: EventName, options: { signal?: AbortSignal }): Promise<any[]>;
export declare function on(emitter: EventEmitterInstance, eventName: EventName): EventEmitterAsyncIterator;
export declare function on(
  emitter: EventEmitterInstance,
  eventName: EventName,
  options: { signal?: AbortSignal; close?: EventName | readonly EventName[] },
): EventEmitterAsyncIterator;
export declare function getEventListeners(emitter: EventEmitterInstance, eventName: EventName): EventListener[];
export declare function getMaxListeners(emitter: EventEmitterInstance): number;
export declare function listenerCount(emitter: EventEmitterInstance, eventName: EventName): number;
export declare function setMaxListeners(n: number, ...emitters: EventEmitterInstance[]): void;

export declare const usingDomains: boolean;
export declare const captureRejectionSymbol: unique symbol;
export declare const errorMonitor: unique symbol;
export declare const captureRejections: boolean;
export declare const defaultMaxListeners: number;
declare const EventEmitter: EventEmitterConstructor;
export { EventEmitter, EventEmitter as default };
