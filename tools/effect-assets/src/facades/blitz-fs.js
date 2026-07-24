import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as HostFileSystem from "blitz-internal:file-system";

export const FileSystem = Context.GenericTag("blitz:fs/FileSystem");

const liveService = Object.freeze({
  readFile: HostFileSystem.readFile,
  readTextFile: HostFileSystem.readTextFile,
  writeFile: HostFileSystem.writeFile,
  writeTextFile: HostFileSystem.writeTextFile
});

export const readFile = Effect.serviceFunctionEffect(
  FileSystem,
  (service) => service.readFile
);

export const readTextFile = Effect.serviceFunctionEffect(
  FileSystem,
  (service) => service.readTextFile
);

export const writeFile = Effect.serviceFunctionEffect(
  FileSystem,
  (service) => service.writeFile
);

export const writeTextFile = Effect.serviceFunctionEffect(
  FileSystem,
  (service) => service.writeTextFile
);

export const layerLive = Layer.succeed(FileSystem, liveService);
