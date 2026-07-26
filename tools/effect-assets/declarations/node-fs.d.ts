/** The string and file-URL paths supported by Blitz's initial node:fs slice. */
export type PathLike = string | URL;
export type FileData = string | Uint8Array;

export interface NodeFsError extends Error {
  readonly code: string;
  readonly errno?: number;
  readonly syscall?: string;
  readonly path?: string;
  readonly dest?: string;
}

export interface ReadFileOptions {
  readonly encoding?: "utf8" | "utf-8" | null;
}

export interface WriteFileOptions extends ReadFileOptions {
  readonly flag?: "w" | "wx" | "xw" | "a" | "ax" | "xa" | "w+" | "wx+" | "xw+" | "a+" | "ax+" | "xa+";
}

export interface StatOptions {
  readonly bigint?: false;
}

export interface MkdirOptions {
  readonly recursive?: boolean;
}

export interface ReaddirOptions extends ReadFileOptions {
  readonly withFileTypes?: boolean;
}

export interface RmOptions {
  readonly recursive?: boolean;
  readonly force?: boolean;
  readonly maxRetries?: 0;
  readonly retryDelay?: 0;
}

export interface Stats {
  readonly dev: number;
  readonly ino: number;
  readonly mode: number;
  readonly nlink: number;
  readonly uid: number;
  readonly gid: number;
  readonly rdev: number;
  readonly size: number;
  readonly blksize: number;
  readonly blocks: number;
  readonly atimeMs: number;
  readonly mtimeMs: number;
  readonly ctimeMs: number;
  readonly birthtimeMs: number;
  readonly atime: Date;
  readonly mtime: Date;
  readonly ctime: Date;
  readonly birthtime: Date;
  isFile(): boolean;
  isDirectory(): boolean;
  isBlockDevice(): boolean;
  isCharacterDevice(): boolean;
  isSymbolicLink(): boolean;
  isFIFO(): boolean;
  isSocket(): boolean;
}

export interface Dirent {
  readonly name: string;
  isFile(): boolean;
  isDirectory(): boolean;
  isBlockDevice(): boolean;
  isCharacterDevice(): boolean;
  isSymbolicLink(): boolean;
  isFIFO(): boolean;
  isSocket(): boolean;
}

export interface Constants {
  readonly COPYFILE_EXCL: 1;
  readonly COPYFILE_FICLONE: 2;
  readonly COPYFILE_FICLONE_FORCE: 4;
  readonly F_OK: 0;
  readonly R_OK: 4;
  readonly W_OK: 2;
  readonly X_OK: 1;
  readonly UV_FS_COPYFILE_EXCL: 1;
  readonly UV_FS_COPYFILE_FICLONE: 2;
  readonly UV_FS_COPYFILE_FICLONE_FORCE: 4;
}

export declare const constants: Constants;
export declare const Stats: { new (value: unknown): Stats };
export declare const Dirent: { new (value: unknown): Dirent };

export declare function readFileSync(path: PathLike, options?: ReadFileOptions | "utf8" | "utf-8" | null): Uint8Array | string;
export declare function writeFileSync(path: PathLike, data: FileData, options?: WriteFileOptions | "utf8" | "utf-8" | null): void;
export declare function statSync(path: PathLike, options?: StatOptions): Stats;
export declare function mkdirSync(path: PathLike, options?: MkdirOptions): void;
export declare function readdirSync(path: PathLike, options?: ReaddirOptions | "utf8" | "utf-8" | null): string[] | Dirent[];
export declare function rmSync(path: PathLike, options?: RmOptions): void;
export declare function unlinkSync(path: PathLike): void;
export declare function renameSync(oldPath: PathLike, newPath: PathLike): void;
export declare function existsSync(path: PathLike): boolean;

export declare function readFile(path: PathLike, callback: (error: NodeFsError | null, data: Uint8Array) => void): void;
export declare function readFile(path: PathLike, options: ReadFileOptions | "utf8" | "utf-8" | null, callback: (error: NodeFsError | null, data: Uint8Array | string) => void): void;
export declare function writeFile(path: PathLike, data: FileData, callback: (error: NodeFsError | null) => void): void;
export declare function writeFile(path: PathLike, data: FileData, options: WriteFileOptions | "utf8" | "utf-8" | null, callback: (error: NodeFsError | null) => void): void;
export declare function stat(path: PathLike, callback: (error: NodeFsError | null, stats: Stats) => void): void;
export declare function stat(path: PathLike, options: StatOptions, callback: (error: NodeFsError | null, stats: Stats) => void): void;
export declare function mkdir(path: PathLike, callback: (error: NodeFsError | null) => void): void;
export declare function mkdir(path: PathLike, options: MkdirOptions, callback: (error: NodeFsError | null) => void): void;
export declare function readdir(path: PathLike, callback: (error: NodeFsError | null, entries: string[]) => void): void;
export declare function readdir(path: PathLike, options: ReaddirOptions | "utf8" | "utf-8" | null, callback: (error: NodeFsError | null, entries: string[] | Dirent[]) => void): void;
export declare function rm(path: PathLike, callback: (error: NodeFsError | null) => void): void;
export declare function rm(path: PathLike, options: RmOptions, callback: (error: NodeFsError | null) => void): void;
export declare function unlink(path: PathLike, callback: (error: NodeFsError | null) => void): void;
export declare function rename(oldPath: PathLike, newPath: PathLike, callback: (error: NodeFsError | null) => void): void;

export interface PromisesApi {
  readonly constants: Constants;
  readonly readFile: typeof import("node:fs/promises").readFile;
  readonly writeFile: typeof import("node:fs/promises").writeFile;
  readonly stat: typeof import("node:fs/promises").stat;
  readonly mkdir: typeof import("node:fs/promises").mkdir;
  readonly readdir: typeof import("node:fs/promises").readdir;
  readonly rm: typeof import("node:fs/promises").rm;
  readonly unlink: typeof import("node:fs/promises").unlink;
  readonly rename: typeof import("node:fs/promises").rename;
}

export declare const promises: PromisesApi;

declare const fs: {
  readonly constants: typeof constants;
  readonly promises: PromisesApi;
  readonly Stats: typeof Stats;
  readonly Dirent: typeof Dirent;
  readonly readFile: typeof readFile;
  readonly readFileSync: typeof readFileSync;
  readonly writeFile: typeof writeFile;
  readonly writeFileSync: typeof writeFileSync;
  readonly stat: typeof stat;
  readonly statSync: typeof statSync;
  readonly mkdir: typeof mkdir;
  readonly mkdirSync: typeof mkdirSync;
  readonly readdir: typeof readdir;
  readonly readdirSync: typeof readdirSync;
  readonly rm: typeof rm;
  readonly rmSync: typeof rmSync;
  readonly unlink: typeof unlink;
  readonly unlinkSync: typeof unlinkSync;
  readonly rename: typeof rename;
  readonly renameSync: typeof renameSync;
  readonly existsSync: typeof existsSync;
};

export { fs as default };
