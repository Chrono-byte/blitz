export type StreamChunk = string | Uint8Array;

export interface StreamOptions {
  readonly objectMode?: boolean;
  readonly readableObjectMode?: boolean;
  readonly writableObjectMode?: boolean;
  readonly readableHighWaterMark?: number;
  readonly writableHighWaterMark?: number;
  readonly highWaterMark?: number;
  readonly autoDestroy?: boolean;
  readonly emitClose?: boolean;
  readonly allowHalfOpen?: boolean;
  readonly signal?: AbortSignal;
  readonly destroy?: (error: Error | undefined, callback: (error?: Error) => void) => void;
}

export interface ReadableOptions extends StreamOptions {
  readonly read?: (this: Readable, size: number) => void;
}

export interface WritableOptions extends StreamOptions {
  readonly write?: (this: Writable, chunk: StreamChunk, encoding: string, callback: (error?: Error) => void) => void;
  readonly final?: (this: Writable, callback: (error?: Error) => void) => void;
}

export interface TransformOptions extends ReadableOptions, WritableOptions {
  readonly transform?: (this: Transform, chunk: StreamChunk, encoding: string, callback: (error?: Error | null, data?: StreamChunk) => void) => void;
  readonly flush?: (this: Transform, callback: (error?: Error | null, data?: StreamChunk) => void) => void;
}

export interface PipeOptions {
  readonly end?: boolean;
}

export interface FinishedOptions {
  readonly readable?: boolean;
  readonly writable?: boolean;
  readonly cleanup?: boolean;
  readonly signal?: AbortSignal;
}

export interface PipelineOptions {
  readonly signal?: AbortSignal;
}

interface StreamEventEmitter {
  addListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  on(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  once(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  prependListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  prependOnceListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  removeListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  off(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  removeAllListeners(eventName?: string | symbol): this;
  emit(eventName: string | symbol, ...args: any[]): boolean;
  listeners(eventName: string | symbol): ((...args: any[]) => unknown)[];
  rawListeners(eventName: string | symbol): ((...args: any[]) => unknown)[];
  listenerCount(eventName: string | symbol, listener?: (...args: any[]) => unknown): number;
  eventNames(): (string | symbol)[];
  getMaxListeners(): number;
  setMaxListeners(n: number): this;
}

export interface StreamInstance extends StreamEventEmitter {
  readonly destroyed: boolean;
  readonly closed: boolean;
  readonly errored: Error | null;
  destroy(error?: Error, callback?: (error?: Error) => void): this;
}

export class Stream implements StreamInstance {
  readonly destroyed: boolean;
  readonly closed: boolean;
  readonly errored: Error | null;
  addListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  on(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  once(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  prependListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  prependOnceListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  removeListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  off(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  removeAllListeners(eventName?: string | symbol): this;
  emit(eventName: string | symbol, ...args: any[]): boolean;
  listeners(eventName: string | symbol): ((...args: any[]) => unknown)[];
  rawListeners(eventName: string | symbol): ((...args: any[]) => unknown)[];
  listenerCount(eventName: string | symbol, listener?: (...args: any[]) => unknown): number;
  eventNames(): (string | symbol)[];
  getMaxListeners(): number;
  setMaxListeners(n: number): this;
  destroy(error?: Error, callback?: (error?: Error) => void): this;
  static Readable: typeof Readable;
  static Writable: typeof Writable;
  static Duplex: typeof Duplex;
  static Transform: typeof Transform;
  static PassThrough: typeof PassThrough;
  static pipeline: typeof pipeline;
  static finished: typeof finished;
  static destroy: typeof destroy;
  static addAbortSignal: typeof addAbortSignal;
  static isDestroyed: typeof isDestroyed;
  static isDisturbed: typeof isDisturbed;
  static isErrored: typeof isErrored;
  static isReadable: typeof isReadable;
  static isWritable: typeof isWritable;
  static setDefaultHighWaterMark: typeof setDefaultHighWaterMark;
  static getDefaultHighWaterMark: typeof getDefaultHighWaterMark;
  static Stream: typeof Stream;
  static promises: typeof promises;
}

export class Readable extends Stream {
  constructor(options?: ReadableOptions);
  readonly readable: boolean | null;
  readonly readableEnded: boolean;
  readonly readableFlowing: boolean | null;
  readonly readableLength: number;
  readonly readableHighWaterMark: number;
  readonly readableObjectMode: boolean;
  _read(size: number): void;
  push(chunk: StreamChunk | null): boolean;
  unshift(chunk: StreamChunk): boolean;
  read(size?: number): StreamChunk | null;
  resume(): this;
  pause(): this;
  isPaused(): boolean;
  pipe<T extends Writable>(destination: T, options?: PipeOptions): T;
  unpipe(destination?: Writable): this;
  [Symbol.asyncIterator](): AsyncIterableIterator<StreamChunk>;
  static from(iterable: Iterable<StreamChunk> | AsyncIterable<StreamChunk>, options?: ReadableOptions): Readable;
}

export class Writable extends Stream {
  constructor(options?: WritableOptions);
  readonly writable: boolean | null;
  readonly writableEnded: boolean;
  readonly writableFinished: boolean;
  readonly writableLength: number;
  readonly writableHighWaterMark: number;
  readonly writableObjectMode: boolean;
  readonly writableNeedDrain: boolean;
  readonly writableCorked: number;
  _write(chunk: StreamChunk, encoding: string, callback: (error?: Error) => void): void;
  write(chunk: StreamChunk, encoding?: string, callback?: (error?: Error) => void): boolean;
  write(chunk: StreamChunk, callback?: (error?: Error) => void): boolean;
  end(callback?: (error?: Error) => void): this;
  end(chunk: StreamChunk, callback?: (error?: Error) => void): this;
  end(chunk: StreamChunk, encoding?: string, callback?: (error?: Error) => void): this;
  cork(): this;
  uncork(): this;
}

export class Duplex extends Readable {
  constructor(options?: TransformOptions);
  readonly writable: boolean | null;
  readonly writableEnded: boolean;
  readonly writableFinished: boolean;
  readonly writableLength: number;
  readonly writableHighWaterMark: number;
  readonly writableObjectMode: boolean;
  readonly writableNeedDrain: boolean;
  readonly writableCorked: number;
  _write(chunk: StreamChunk, encoding: string, callback: (error?: Error) => void): void;
  write(chunk: StreamChunk, encoding?: string, callback?: (error?: Error) => void): boolean;
  write(chunk: StreamChunk, callback?: (error?: Error) => void): boolean;
  end(callback?: (error?: Error) => void): this;
  end(chunk: StreamChunk, callback?: (error?: Error) => void): this;
  end(chunk: StreamChunk, encoding?: string, callback?: (error?: Error) => void): this;
  cork(): this;
  uncork(): this;
}

export class Transform extends Duplex {
  constructor(options?: TransformOptions);
  _transform(chunk: StreamChunk, encoding: string, callback: (error?: Error | null, data?: StreamChunk) => void): void;
}

export class PassThrough extends Transform {
  constructor(options?: TransformOptions);
}

export function destroy<T extends StreamInstance>(stream: T, error?: Error, callback?: (error?: Error) => void): T;
export function addAbortSignal<T extends StreamInstance>(signal: AbortSignal, stream: T): T;
export function isDestroyed(stream: unknown): boolean | null;
export function isDisturbed(stream: unknown): boolean;
export function isErrored(stream: unknown): boolean;
export function isReadable(stream: unknown): boolean | null;
export function isWritable(stream: unknown): boolean | null;
export function getDefaultHighWaterMark(objectMode: boolean): number;
export function setDefaultHighWaterMark(objectMode: boolean, value: number): void;
export function finished(stream: StreamInstance, callback: (error?: Error) => void): () => void;
export function finished(stream: StreamInstance, options: FinishedOptions, callback: (error?: Error) => void): () => void;
export function pipeline(...streams: [...StreamInstance[], (error?: Error) => void]): StreamInstance;
export function pipeline(...streams: [...StreamInstance[], PipelineOptions, (error?: Error) => void]): StreamInstance;

export interface StreamPromises {
  readonly finished: (stream: StreamInstance, options?: FinishedOptions) => Promise<void>;
  readonly pipeline: (...streams: Array<StreamInstance | PipelineOptions>) => Promise<void>;
}

export const promises: StreamPromises;

declare const streamDefault: typeof Stream;
export { streamDefault as default };
