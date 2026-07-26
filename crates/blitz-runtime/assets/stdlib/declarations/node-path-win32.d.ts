export interface FormatInputPathObject {
  readonly dir?: string;
  readonly root?: string;
  readonly base?: string;
  readonly name?: string;
  readonly ext?: string;
}

export interface ParsedPath {
  readonly root: string;
  readonly dir: string;
  readonly base: string;
  readonly ext: string;
  readonly name: string;
}

export interface PathModule {
  resolve(...paths: string[]): string;
  normalize(path: string): string;
  isAbsolute(path: string): boolean;
  join(...paths: string[]): string;
  relative(from: string, to: string): string;
  toNamespacedPath(path: string): string;
  dirname(path: string): string;
  basename(path: string, suffix?: string): string;
  extname(path: string): string;
  format(pathObject: FormatInputPathObject): string;
  parse(path: string): ParsedPath;
  matchesGlob(path: string, pattern: string): boolean;
  readonly sep: string;
  readonly delimiter: string;
  readonly win32: PathModule;
  readonly posix: PathModule;
  _makeLong(path: string): string;
}

export declare const resolve: PathModule["resolve"];
export declare const normalize: PathModule["normalize"];
export declare const isAbsolute: PathModule["isAbsolute"];
export declare const join: PathModule["join"];
export declare const relative: PathModule["relative"];
export declare const toNamespacedPath: PathModule["toNamespacedPath"];
export declare const dirname: PathModule["dirname"];
export declare const basename: PathModule["basename"];
export declare const extname: PathModule["extname"];
export declare const format: PathModule["format"];
export declare const parse: PathModule["parse"];
export declare const matchesGlob: PathModule["matchesGlob"];
export declare const sep: string;
export declare const delimiter: string;
export declare const _makeLong: PathModule["_makeLong"];
export declare const posix: PathModule;
export declare const win32: PathModule;

declare const path: PathModule;
export { path as default };
