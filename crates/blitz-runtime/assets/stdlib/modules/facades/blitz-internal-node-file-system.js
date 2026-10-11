import { host } from "blitz-internal:host";

const bridge = () => {
  if (host === undefined || host.abiVersion !== 1) {
    throw new Error("Blitz host ABI v1 is not installed");
  }
  const value = host.nodeFileSystem;
  if (value === undefined) {
    throw new Error("node-file-system primitive is not installed");
  }
  return value;
};

export const readFile = (path, encoding) => bridge().readFile(path, encoding);
export const writeFile = (path, data, flag) => bridge().writeFile(path, data, flag);
export const stat = path => bridge().stat(path);
export const mkdir = (path, recursive) => bridge().mkdir(path, recursive);
export const readdir = path => bridge().readdir(path);
export const rm = (path, recursive, force) => bridge().rm(path, recursive, force);
export const unlink = path => bridge().unlink(path);
export const rename = (from, to) => bridge().rename(from, to);
export const exists = path => bridge().exists(path);
