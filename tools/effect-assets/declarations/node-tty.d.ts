export interface TtyOptions {
  readonly readable?: boolean;
  readonly writable?: boolean;
}

export type TtyEnvironment = Readonly<Record<string, string | undefined>>;

export declare function isatty(fd: number): boolean;

export declare class ReadStream {
  readonly fd: number;
  readonly isTTY: boolean | undefined;
  readonly isRaw: boolean | undefined;

  constructor(fd: number, options?: TtyOptions);
  setRawMode(mode: boolean): this;
}

export declare class WriteStream {
  readonly fd: number;
  readonly columns: number | undefined;
  readonly rows: number | undefined;
  readonly isTTY: boolean | undefined;

  constructor(fd: number);
  getColorDepth(env?: TtyEnvironment): number;
  hasColors(count?: number, env?: TtyEnvironment): boolean;
  getWindowSize(): readonly [number, number];
}

export interface TtyModule {
  readonly isatty: typeof isatty;
  readonly ReadStream: typeof ReadStream;
  readonly WriteStream: typeof WriteStream;
}

declare const tty: TtyModule;
export { tty as default };
