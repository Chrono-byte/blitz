export type OutgoingHttpHeader = string | number | readonly string[];
export type OutgoingHttpHeaders = Record<string, OutgoingHttpHeader | undefined>;

export interface ListenOptions {
  port?: number;
  host?: string;
  hostname?: string;
}

export interface ServerOptions {
  requestTimeout?: number;
  headersTimeout?: number;
  keepAliveTimeout?: number;
}

export interface AddressInfo {
  address: string;
  family: "IPv4" | "IPv6";
  port: number;
}

interface HttpEventEmitter {
  addListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  on(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  once(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  removeListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  off(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  emit(eventName: string | symbol, ...args: any[]): boolean;
}

export class IncomingMessage implements HttpEventEmitter {
  method: string;
  url: string;
  headers: Record<string, string>;
  headersDistinct: Record<string, string[]>;
  rawHeaders: string[];
  httpVersion: string;
  httpVersionMajor: number;
  httpVersionMinor: number;
  complete: boolean;
  destroyed: boolean;
  readable: boolean;
  socket: {
    remoteAddress: string;
    remotePort: number;
    remoteFamily: "IPv4" | "IPv6";
    encrypted: false;
  };
  connection: IncomingMessage["socket"];
  addListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  on(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  once(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  removeListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  off(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  emit(eventName: string | symbol, ...args: any[]): boolean;
  setTimeout(milliseconds: number, callback?: () => void): this;
  destroy(error?: Error): this;
}

export class ServerResponse implements HttpEventEmitter {
  statusCode: number;
  statusMessage?: string;
  headersSent: boolean;
  finished: boolean;
  writableEnded: boolean;
  writableFinished: boolean;
  destroyed: boolean;
  sendDate: boolean;
  addListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  on(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  once(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  removeListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  off(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  emit(eventName: string | symbol, ...args: any[]): boolean;
  setHeader(name: string, value: OutgoingHttpHeader): this;
  getHeader(name: string): string | undefined;
  getHeaders(): Record<string, string>;
  getHeaderNames(): string[];
  hasHeader(name: string): boolean;
  removeHeader(name: string): void;
  writeHead(statusCode: number, headers?: OutgoingHttpHeaders | readonly string[]): this;
  writeHead(statusCode: number, statusMessage: string, headers?: OutgoingHttpHeaders | readonly string[]): this;
  flushHeaders(): void;
  write(chunk: string | Uint8Array, callback?: () => void): boolean;
  write(chunk: string | Uint8Array, encoding: string, callback?: () => void): boolean;
  end(callback?: () => void): this;
  end(chunk: string | Uint8Array, callback?: () => void): this;
  end(chunk: string | Uint8Array, encoding: string, callback?: () => void): this;
  setTimeout(milliseconds: number, callback?: () => void): this;
  destroy(error?: Error): this;
}

export type RequestListener = (request: IncomingMessage, response: ServerResponse) => void;

export class Server implements HttpEventEmitter {
  constructor(requestListener?: RequestListener);
  constructor(options: ServerOptions, requestListener?: RequestListener);
  listening: boolean;
  requestTimeout: number;
  headersTimeout: number;
  keepAliveTimeout: number;
  maxHeadersCount: number | null;
  addListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  on(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  once(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  removeListener(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  off(eventName: string | symbol, listener: (...args: any[]) => unknown): this;
  emit(eventName: string | symbol, ...args: any[]): boolean;
  listen(port?: number, hostname?: string, callback?: () => void): this;
  listen(options: ListenOptions, callback?: () => void): this;
  close(callback?: (error?: Error) => void): this;
  address(): AddressInfo | null;
  ref(): this;
  unref(): this;
  getConnections(callback: (error: Error | null, count: number) => void): void;
}

export function createServer(requestListener?: RequestListener): Server;
export function createServer(options: ServerOptions, requestListener?: RequestListener): Server;

export interface ClientRequest {}
export function request(...args: any[]): ClientRequest;
export function get(...args: any[]): ClientRequest;

export const METHODS: readonly string[];
export const STATUS_CODES: Readonly<Record<number, string>>;
export const maxHeaderSize: number;
export const globalAgent: Readonly<{ protocol: "http:" }>;

declare const http: {
  METHODS: typeof METHODS;
  STATUS_CODES: typeof STATUS_CODES;
  IncomingMessage: typeof IncomingMessage;
  Server: typeof Server;
  ServerResponse: typeof ServerResponse;
  createServer: typeof createServer;
  get: typeof get;
  globalAgent: typeof globalAgent;
  maxHeaderSize: typeof maxHeaderSize;
  request: typeof request;
};
export default http;
