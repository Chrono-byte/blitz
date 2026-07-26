export interface ConsoleConstructorOptions {
  readonly stdout?: unknown;
  readonly stderr?: unknown;
  readonly ignoreErrors?: boolean;
  readonly colorMode?: boolean | "auto";
  readonly inspectOptions?: unknown;
  readonly groupIndentation?: number;
}

export declare class Console {
  constructor(stdout?: unknown, stderr?: unknown, ignoreErrors?: boolean);
  constructor(options: ConsoleConstructorOptions);

  log(message?: unknown, ...optionalParams: unknown[]): void;
  info(message?: unknown, ...optionalParams: unknown[]): void;
  debug(message?: unknown, ...optionalParams: unknown[]): void;
  warn(message?: unknown, ...optionalParams: unknown[]): void;
  error(message?: unknown, ...optionalParams: unknown[]): void;
  dir(object: unknown, options?: unknown): void;
  dirxml(...data: unknown[]): void;
  assert(value?: unknown, message?: unknown, ...optionalParams: unknown[]): void;
  count(label?: unknown): void;
  countReset(label?: unknown): void;
  time(label?: unknown): void;
  timeLog(label?: unknown, ...data: unknown[]): void;
  timeEnd(label?: unknown): void;
  group(...label: unknown[]): void;
  groupCollapsed(...label: unknown[]): void;
  groupEnd(): void;
  table(tabularData: unknown, properties?: readonly string[]): void;
  trace(message?: unknown, ...optionalParams: unknown[]): void;
  clear(): void;
  profile(label?: unknown): void;
  profileEnd(label?: unknown): void;
  timeStamp(label?: unknown): void;
}

export declare const assert: Console["assert"];
export declare const clear: Console["clear"];
export declare const count: Console["count"];
export declare const countReset: Console["countReset"];
export declare const debug: Console["debug"];
export declare const dir: Console["dir"];
export declare const dirxml: Console["dirxml"];
export declare const error: Console["error"];
export declare const group: Console["group"];
export declare const groupCollapsed: Console["groupCollapsed"];
export declare const groupEnd: Console["groupEnd"];
export declare const info: Console["info"];
export declare const log: Console["log"];
export declare const profile: Console["profile"];
export declare const profileEnd: Console["profileEnd"];
export declare const table: Console["table"];
export declare const time: Console["time"];
export declare const timeEnd: Console["timeEnd"];
export declare const timeLog: Console["timeLog"];
export declare const timeStamp: Console["timeStamp"];
export declare const trace: Console["trace"];
export declare const warn: Console["warn"];

declare const consoleInstance: Console;
export { consoleInstance as console, consoleInstance as default };
