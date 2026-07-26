import type { EventEmitterInstance, EventListener } from "node:events";

export interface ReadlineInput extends EventEmitterInstance {
  pause?(): this;
  resume?(): this;
  [Symbol.asyncIterator]?(): AsyncIterator<string | Uint8Array>;
}

export interface ReadlineOutput {
  write(chunk: string, callback?: () => void): boolean;
}

export type ReadlineChunk = string | Uint8Array;
export type Completer = (line: string) =>
  | readonly [readonly string[], string]
  | Promise<readonly [readonly string[], string]>;

export interface InterfaceOptions {
  readonly input: ReadlineInput | AsyncIterable<ReadlineChunk>;
  readonly output?: ReadlineOutput;
  readonly completer?: Completer | ((line: string, callback: (error: Error | null, result: readonly [readonly string[], string]) => void) => void);
  readonly terminal?: boolean;
  readonly prompt?: string;
  readonly crlfDelay?: number;
  readonly historySize?: number;
  readonly removeHistoryDuplicates?: boolean;
  readonly escapeCodeTimeout?: number;
  readonly tabSize?: number;
  readonly signal?: AbortSignal;
}

export interface QuestionOptions {
  readonly signal?: AbortSignal;
}

export class Interface implements EventEmitterInstance {
  constructor(options: InterfaceOptions);
  constructor(input: ReadlineInput | AsyncIterable<ReadlineChunk>, output?: ReadlineOutput, completer?: InterfaceOptions["completer"], terminal?: boolean);
  readonly input: ReadlineInput | AsyncIterable<ReadlineChunk>;
  readonly output: ReadlineOutput | undefined;
  readonly terminal: boolean;
  readonly closed: boolean;
  readonly completer: InterfaceOptions["completer"];
  addListener(eventName: string | symbol, listener: EventListener): this;
  on(eventName: string | symbol, listener: EventListener): this;
  once(eventName: string | symbol, listener: EventListener): this;
  prependListener(eventName: string | symbol, listener: EventListener): this;
  prependOnceListener(eventName: string | symbol, listener: EventListener): this;
  removeListener(eventName: string | symbol, listener: EventListener): this;
  off(eventName: string | symbol, listener: EventListener): this;
  removeAllListeners(): this;
  removeAllListeners(eventName: string | symbol): this;
  emit(eventName: string | symbol, ...args: any[]): boolean;
  listeners(eventName: string | symbol): EventListener[];
  rawListeners(eventName: string | symbol): EventListener[];
  listenerCount(eventName: string | symbol): number;
  listenerCount(eventName: string | symbol, listener: EventListener): number;
  eventNames(): (string | symbol)[];
  getMaxListeners(): number;
  setMaxListeners(n: number): this;
  setPrompt(prompt: string): this;
  getPrompt(): string;
  prompt(preserveCursor?: boolean): this;
  pause(): this;
  resume(): this;
  close(): this;
  question(query: string, callback: (answer: string) => void): this;
  question(query: string, options: QuestionOptions, callback: (answer: string) => void): this;
  write(data: string | Uint8Array, key?: { readonly name?: string }): this;
  [Symbol.asyncIterator](): AsyncIterableIterator<string>;
}

export declare function createInterface(options: InterfaceOptions): Interface;
export declare function createInterface(input: ReadlineInput | AsyncIterable<ReadlineChunk>, output?: ReadlineOutput, completer?: InterfaceOptions["completer"], terminal?: boolean): Interface;
export declare function clearLine(stream: ReadlineOutput, dir: -1 | 0 | 1, callback?: () => void): boolean;
export declare function clearLine(stream: ReadlineOutput, callback?: () => void): boolean;
export declare function clearScreenDown(stream: ReadlineOutput, callback?: () => void): boolean;
export declare function cursorTo(stream: ReadlineOutput, x: number, callback?: () => void): boolean;
export declare function cursorTo(stream: ReadlineOutput, x: number, y: number, callback?: () => void): boolean;
export declare function moveCursor(stream: ReadlineOutput, dx: number, dy: number, callback?: () => void): boolean;
export declare function emitKeypressEvents(stream: ReadlineInput, interface?: Interface): void;

export interface PromiseReadlineInterface extends Interface {
  question(query: string, options?: QuestionOptions): Promise<string>;
}

export interface ReadlineEditor {
  cursorTo(x: number, y?: number): this;
  moveCursor(dx: number, dy: number): this;
  clearLine(dir: -1 | 0 | 1): this;
  clearScreenDown(): this;
  commit(): Promise<null>;
  rollback(): this;
}

export interface ReadlinePromises {
  readonly Interface: {
    new (options: InterfaceOptions): PromiseReadlineInterface;
  };
  readonly Readline: {
    new (stream: ReadlineOutput): ReadlineEditor;
  };
  readonly createInterface: (options: InterfaceOptions) => PromiseReadlineInterface;
}

export const promises: ReadlinePromises;

declare const readline: {
  readonly Interface: typeof Interface;
  readonly clearLine: typeof clearLine;
  readonly clearScreenDown: typeof clearScreenDown;
  readonly createInterface: typeof createInterface;
  readonly cursorTo: typeof cursorTo;
  readonly emitKeypressEvents: typeof emitKeypressEvents;
  readonly moveCursor: typeof moveCursor;
  readonly promises: ReadlinePromises;
};

export { readline as default };
