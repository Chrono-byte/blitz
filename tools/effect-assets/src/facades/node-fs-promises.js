import * as HostFileSystem from "blitz-internal:node-file-system";

const codedError = (code, message, ErrorConstructor = Error) => {
  const error = new ErrorConstructor(message);
  error.code = code;
  return error;
};

const errorFromResult = result => {
  if (result !== null && typeof result === "object" && result.code !== undefined) {
    const ErrorConstructor = result.code === "ERR_INVALID_ARG_TYPE" ? TypeError : Error;
    const error = new ErrorConstructor(result.message ?? result.code);
    error.code = result.code;
    if (result.errno !== undefined) error.errno = result.errno;
    if (result.syscall !== undefined) error.syscall = result.syscall;
    if (result.path !== undefined) error.path = result.path;
    if (result.dest !== undefined) error.dest = result.dest;
    return error;
  }
  return codedError("ERR_SYSTEM_ERROR", "The Node filesystem host returned an invalid result");
};

const unwrap = result => {
  if (result !== null && typeof result === "object" && result.ok === true) return result.value;
  throw errorFromResult(result);
};

const validatePath = value => {
  if (typeof value === "string") return value;
  if (value !== null && typeof value === "object" && typeof value.href === "string") return value;
  throw codedError("ERR_INVALID_ARG_TYPE", "The path argument must be a string or a file URL", TypeError);
};

const validateEncoding = value => {
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "string") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The encoding option must be a string", TypeError);
  }
  if (value.toLowerCase() === "utf8" || value.toLowerCase() === "utf-8") return "utf8";
  throw codedError("ERR_INVALID_ARG_VALUE", `The encoding '${value}' is not supported by Blitz`);
};

const readOptions = options => {
  if (options === undefined || options === null) return undefined;
  if (typeof options === "string") return validateEncoding(options);
  if (typeof options !== "object") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The options argument must be a string or an object", TypeError);
  }
  if (options.signal !== undefined) {
    throw codedError("ERR_NOT_IMPLEMENTED", "AbortSignal cancellation is not implemented by Blitz");
  }
  if (options.flag !== undefined) {
    throw codedError("ERR_NOT_IMPLEMENTED", "readFile flags are not implemented by Blitz");
  }
  return validateEncoding(options.encoding);
};

const writeOptions = (options, defaultFlag = "w") => {
  if (options === undefined || options === null) return defaultFlag;
  if (typeof options === "string") {
    validateEncoding(options);
    return defaultFlag;
  }
  if (typeof options !== "object") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The options argument must be a string or an object", TypeError);
  }
  if (options.signal !== undefined) {
    throw codedError("ERR_NOT_IMPLEMENTED", "AbortSignal cancellation is not implemented by Blitz");
  }
  if (options.mode !== undefined) {
    throw codedError("ERR_NOT_IMPLEMENTED", "file modes are not implemented by Blitz");
  }
  validateEncoding(options.encoding);
  const flag = options.flag === undefined ? defaultFlag : options.flag;
  if (typeof flag !== "string") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The flag option must be a string", TypeError);
  }
  if (!["w", "wx", "xw", "a", "ax", "xa", "w+", "wx+", "xw+", "a+", "ax+", "xa+"].includes(flag)) {
    throw codedError("ERR_INVALID_ARG_VALUE", `The flag '${flag}' is not supported by Blitz`);
  }
  return flag;
};

const validateStatOptions = options => {
  if (options === undefined || options === null) return;
  if (typeof options !== "object") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The options argument must be an object", TypeError);
  }
  if (options.signal !== undefined) {
    throw codedError("ERR_NOT_IMPLEMENTED", "AbortSignal cancellation is not implemented by Blitz");
  }
  if (options.bigint === true) {
    throw codedError("ERR_NOT_IMPLEMENTED", "BigInt filesystem statistics are not implemented by Blitz");
  }
};

const validateDirectoryOptions = options => {
  if (options === undefined || options === null) return false;
  if (typeof options === "string") {
    validateEncoding(options);
    return false;
  }
  if (typeof options !== "object") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The options argument must be an object", TypeError);
  }
  if (options.signal !== undefined) {
    throw codedError("ERR_NOT_IMPLEMENTED", "AbortSignal cancellation is not implemented by Blitz");
  }
  validateEncoding(options.encoding);
  if (options.withFileTypes !== undefined && typeof options.withFileTypes !== "boolean") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The withFileTypes option must be a boolean", TypeError);
  }
  return options.withFileTypes === true;
};

const validateMkdirOptions = options => {
  if (options === undefined || options === null) return false;
  if (typeof options !== "object") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The options argument must be an object", TypeError);
  }
  if (options.signal !== undefined) {
    throw codedError("ERR_NOT_IMPLEMENTED", "AbortSignal cancellation is not implemented by Blitz");
  }
  if (options.mode !== undefined) {
    throw codedError("ERR_NOT_IMPLEMENTED", "directory modes are not implemented by Blitz");
  }
  if (options.recursive !== undefined && typeof options.recursive !== "boolean") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The recursive option must be a boolean", TypeError);
  }
  return options.recursive === true;
};

const validateRmOptions = options => {
  if (options === undefined || options === null) return { recursive: false, force: false };
  if (typeof options !== "object") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The options argument must be an object", TypeError);
  }
  if (options.signal !== undefined) {
    throw codedError("ERR_NOT_IMPLEMENTED", "AbortSignal cancellation is not implemented by Blitz");
  }
  if (options.recursive !== undefined && typeof options.recursive !== "boolean") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The recursive option must be a boolean", TypeError);
  }
  if (options.force !== undefined && typeof options.force !== "boolean") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The force option must be a boolean", TypeError);
  }
  if ((options.maxRetries !== undefined && options.maxRetries !== 0)
    || (options.retryDelay !== undefined && options.retryDelay !== 0)) {
    throw codedError("ERR_NOT_IMPLEMENTED", "rm retries are not implemented by Blitz");
  }
  return { recursive: options.recursive === true, force: options.force === true };
};

// The host operation is direct, but both its start and completion cross the
// VM Promise job boundary. No cancellation is claimed without worker/event
// integration.
const later = operation => Promise.resolve().then(operation);

class Stats {
  constructor(value) {
    this.dev = value.dev;
    this.ino = value.ino;
    this.mode = value.mode;
    this.nlink = value.nlink;
    this.uid = value.uid;
    this.gid = value.gid;
    this.rdev = value.rdev;
    this.size = value.size;
    this.blksize = value.blksize;
    this.blocks = value.blocks;
    this.atimeMs = value.atimeMs;
    this.mtimeMs = value.mtimeMs;
    this.ctimeMs = value.ctimeMs;
    this.birthtimeMs = value.birthtimeMs;
    this.atime = new Date(this.atimeMs);
    this.mtime = new Date(this.mtimeMs);
    this.ctime = new Date(this.ctimeMs);
    this.birthtime = new Date(this.birthtimeMs);
    this._isFile = value.isFile === true;
    this._isDirectory = value.isDirectory === true;
    this._isSymbolicLink = value.isSymbolicLink === true;
  }

  isFile() { return this._isFile; }
  isDirectory() { return this._isDirectory; }
  isBlockDevice() { return false; }
  isCharacterDevice() { return false; }
  isSymbolicLink() { return this._isSymbolicLink; }
  isFIFO() { return false; }
  isSocket() { return false; }
}

class Dirent {
  constructor(value) {
    this.name = value.name;
    this._isFile = value.isFile === true;
    this._isDirectory = value.isDirectory === true;
    this._isSymbolicLink = value.isSymbolicLink === true;
  }

  isFile() { return this._isFile; }
  isDirectory() { return this._isDirectory; }
  isBlockDevice() { return false; }
  isCharacterDevice() { return false; }
  isSymbolicLink() { return this._isSymbolicLink; }
  isFIFO() { return false; }
  isSocket() { return false; }
}

export const readFile = (path, options) => {
  const encoding = readOptions(options);
  return later(() => unwrap(HostFileSystem.readFile(validatePath(path), encoding)));
};

export const writeFile = (path, data, options) => {
  const flag = writeOptions(options);
  return later(() => unwrap(HostFileSystem.writeFile(validatePath(path), data, flag)));
};

export const appendFile = (path, data, options) => {
  const flag = writeOptions(options, "a");
  return later(() => unwrap(HostFileSystem.writeFile(validatePath(path), data, flag)));
};

export const stat = (path, options) => {
  validateStatOptions(options);
  return later(() => new Stats(unwrap(HostFileSystem.stat(validatePath(path)))));
};

export const mkdir = (path, options) => {
  const recursive = validateMkdirOptions(options);
  return later(() => unwrap(HostFileSystem.mkdir(validatePath(path), recursive)));
};

export const readdir = (path, options) => {
  const withFileTypes = validateDirectoryOptions(options);
  return later(() => {
    const values = unwrap(HostFileSystem.readdir(validatePath(path)));
    return values.map(value => withFileTypes ? new Dirent(value) : value.name);
  });
};

export const rm = (path, options) => {
  const values = validateRmOptions(options);
  return later(() => unwrap(HostFileSystem.rm(validatePath(path), values.recursive, values.force)));
};

export const unlink = path =>
  later(() => unwrap(HostFileSystem.unlink(validatePath(path))));

export const rename = (oldPath, newPath) =>
  later(() => unwrap(HostFileSystem.rename(validatePath(oldPath), validatePath(newPath))));

export const constants = Object.freeze({
  COPYFILE_EXCL: 1,
  COPYFILE_FICLONE: 2,
  COPYFILE_FICLONE_FORCE: 4,
  F_OK: 0,
  R_OK: 4,
  W_OK: 2,
  X_OK: 1,
  UV_FS_COPYFILE_EXCL: 1,
  UV_FS_COPYFILE_FICLONE: 2,
  UV_FS_COPYFILE_FICLONE_FORCE: 4,
});

const fsPromises = {
  constants,
  readFile,
  writeFile,
  appendFile,
  stat,
  mkdir,
  readdir,
  rm,
  unlink,
  rename,
};

export default fsPromises;
