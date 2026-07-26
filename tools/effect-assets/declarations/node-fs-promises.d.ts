/** Promise-based filesystem operations backed by Blitz's direct host calls. */
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

export interface ReaddirOptions {
  readonly encoding?: "utf8" | "utf-8" | null;
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
export declare function readFile(path: PathLike, options?: ReadFileOptions | "utf8" | "utf-8" | null): Promise<Uint8Array | string>;
export declare function writeFile(path: PathLike, data: FileData, options?: WriteFileOptions | "utf8" | "utf-8" | null): Promise<void>;
export declare function stat(path: PathLike, options?: StatOptions): Promise<Stats>;
export declare function mkdir(path: PathLike, options?: MkdirOptions): Promise<void>;
export declare function readdir(path: PathLike, options?: ReaddirOptions | "utf8" | "utf-8" | null): Promise<string[] | Dirent[]>;
export declare function rm(path: PathLike, options?: RmOptions): Promise<void>;
export declare function unlink(path: PathLike): Promise<void>;
export declare function rename(oldPath: PathLike, newPath: PathLike): Promise<void>;

declare const fsPromises: {
  readonly constants: typeof constants;
  readonly readFile: typeof readFile;
  readonly writeFile: typeof writeFile;
  readonly stat: typeof stat;
  readonly mkdir: typeof mkdir;
  readonly readdir: typeof readdir;
  readonly rm: typeof rm;
  readonly unlink: typeof unlink;
  readonly rename: typeof rename;
};

export { fsPromises as default };
