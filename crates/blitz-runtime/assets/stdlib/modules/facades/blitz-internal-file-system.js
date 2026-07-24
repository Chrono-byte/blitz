import * as Effect from "effect/Effect";

const bridge = () => {
  const host = globalThis.__blitz_host_v1;
  if (host === undefined || host.abiVersion !== 1) {
    throw new Error("Blitz host ABI v1 is not installed");
  }
  const value = host.fileSystem;
  if (value === undefined) {
    throw new Error("file-system primitive is not installed");
  }
  return value;
};

export const readFile = (path) => Effect.suspend(() => bridge().readFile(path));
export const readTextFile = (path) => Effect.suspend(() => bridge().readTextFile(path));
export const writeFile = (path, bytes) => Effect.suspend(() => bridge().writeFile(path, bytes));
export const writeTextFile = (path, text) => Effect.suspend(() => bridge().writeTextFile(path, text));
