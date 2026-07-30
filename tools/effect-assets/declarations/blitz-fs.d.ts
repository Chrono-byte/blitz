import type * as Context from "effect/Context";
import type * as Effect from "effect/Effect";
import type * as Layer from "effect/Layer";

export type Path = string | URL;

export type FileSystemOperation = "read" | "write";

export type FileSystemErrorReason =
  | "NotFound"
  | "PermissionDenied"
  | "AlreadyExists"
  | "InvalidPath"
  | "NotDirectory"
  | "IsDirectory"
  | "TooManyOpenFiles"
  | "FileTooLarge"
  | "InvalidEncoding"
  | "InvalidData"
  | "Io";

export interface FileSystemError {
  readonly _tag: "FileSystemError";
  readonly reason: FileSystemErrorReason;
  readonly operation: FileSystemOperation;
  readonly path?: string;
  readonly message: string;
}

export interface FileSystemService {
  readonly readFile: (
    path: Path,
  ) => Effect.Effect<Uint8Array, FileSystemError>;
  readonly readTextFile: (
    path: Path,
  ) => Effect.Effect<string, FileSystemError>;
  readonly writeFile: (
    path: Path,
    bytes: Uint8Array,
  ) => Effect.Effect<void, FileSystemError>;
  readonly writeTextFile: (
    path: Path,
    text: string,
  ) => Effect.Effect<void, FileSystemError>;
}

export type FileSystem = "blitz:fs/FileSystem";

export declare const FileSystem: Context.Tag<FileSystem, FileSystemService>;

export declare const readFile: (
  path: Path,
) => Effect.Effect<Uint8Array, FileSystemError, FileSystem>;

export declare const readTextFile: (
  path: Path,
) => Effect.Effect<string, FileSystemError, FileSystem>;

export declare const writeFile: (
  path: Path,
  bytes: Uint8Array,
) => Effect.Effect<void, FileSystemError, FileSystem>;

export declare const writeTextFile: (
  path: Path,
  text: string,
) => Effect.Effect<void, FileSystemError, FileSystem>;

export declare const layerLive: Layer.Layer<FileSystem>;
