// SPDX-License-Identifier: MPL-2.0
/** Public host globals installed by `blitz-runtime`. */

interface BlitzNetAddr {
  readonly transport: "tcp";
  readonly hostname: string;
  readonly port: number;
}

interface BlitzServeHandlerInfo {
  readonly remoteAddr: BlitzNetAddr;
}

interface BlitzServeOptions {
  readonly hostname?: string;
  readonly port?: number;
}

interface BlitzHttpServer {
  readonly addr: BlitzNetAddr;
  readonly finished: Promise<void>;

  /** Stop accepting requests and close active connections. Idempotent. */
  shutdown(): Promise<void>;
  /** Keep the runtime alive while this server is listening. */
  ref(): void;
  /** Allow the runtime to exit while leaving this server open. */
  unref(): void;

  /** @deprecated Use `addr.hostname`. */
  readonly hostname: string;
  /** @deprecated Use `addr.port`. */
  readonly port: number;
}

type BlitzServeHandler = (
  request: Request,
  info: BlitzServeHandlerInfo,
) => Response | Promise<Response>;

interface BlitzStdin {
  /** Blitz line-editor extension. */
  onLine(callback: (line: string) => void): void;
  /** Blitz line-editor extension. */
  onClose(callback: () => void): void;
  /** Blitz line-editor extension. */
  setPrompt(prompt: string): void;
  /** Blitz line-editor extension. */
  close(): void;
}

interface BlitzStdout {
  /** Blitz string-output extension. */
  write(message: string): void;
  /** Blitz string-output extension. */
  writeLine(message: string): void;
}

interface BlitzNamespace {
  serve(handler: BlitzServeHandler): BlitzHttpServer;
  serve(options: BlitzServeOptions, handler: BlitzServeHandler): BlitzHttpServer;
  upgradeWebSocket(
    request: Request,
  ): { readonly response: Response; readonly socket: WebSocket };
  readonly stdin: BlitzStdin;
  readonly stdout: BlitzStdout;
}

declare const Blitz: BlitzNamespace;
