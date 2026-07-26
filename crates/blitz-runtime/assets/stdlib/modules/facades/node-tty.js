// TTY is a view over process stdio, not an independent terminal resource.
// Blitz only exposes metadata that the host process actually supplied.
const processValue = globalThis.process;
const processObject = processValue !== null && typeof processValue === "object"
  ? processValue
  : undefined;

const streamFor = fd => {
  if (processObject === undefined || !Number.isInteger(fd)) return undefined;
  if (fd === 0) return processObject.stdin;
  if (fd === 1) return processObject.stdout;
  if (fd === 2) return processObject.stderr;
  return undefined;
};

const unsupported = operation => {
  const error = new Error(`node:tty.${operation} is not implemented by Blitz`);
  error.code = "ERR_NOT_IMPLEMENTED";
  throw error;
};

const terminal = (fd, operation) => {
  const stream = streamFor(fd);
  if (stream === null || typeof stream !== "object") unsupported(`${operation}(${String(fd)})`);
  return stream;
};

const booleanMetadata = (stream, name) => typeof stream[name] === "boolean" ? stream[name] : undefined;
const numberMetadata = (stream, name) => typeof stream[name] === "number" && Number.isFinite(stream[name])
  ? stream[name]
  : undefined;

const colorDepthFrom = (stream, env) => {
  if (typeof stream.getColorDepth === "function") return stream.getColorDepth(env);

  const isTTY = booleanMetadata(stream, "isTTY");
  if (isTTY === undefined) unsupported("getColorDepth");
  if (!isTTY) return 1;

  const values = env !== undefined && env !== null && typeof env === "object" ? env : {};
  if (values.NO_COLOR !== undefined) return 1;
  const forceColor = values.FORCE_COLOR;
  if (forceColor === "0" || forceColor === 0 || forceColor === false) return 1;
  if (forceColor === "1" || forceColor === 1 || forceColor === true) return 4;
  if (forceColor === "2" || forceColor === 2) return 8;
  if (forceColor === "3" || forceColor === 3) return 24;

  const colorTerm = typeof values.COLORTERM === "string" ? values.COLORTERM.toLowerCase() : "";
  if (colorTerm === "truecolor" || colorTerm === "24bit") return 24;
  if (colorTerm.length !== 0) return 4;

  const term = typeof values.TERM === "string" ? values.TERM.toLowerCase() : "";
  if (term.indexOf("256color") !== -1) return 8;
  if (term.indexOf("color") !== -1 || term.indexOf("ansi") !== -1 || term.indexOf("xterm") !== -1) return 4;
  return 1;
};

const validateColorCount = count => {
  if (count === undefined) return 2;
  if (typeof count !== "number" || !Number.isInteger(count) || count < 2) {
    const error = new TypeError("The count argument must be a positive integer");
    error.code = "ERR_INVALID_ARG_VALUE";
    throw error;
  }
  return count;
};

export const isatty = fd => {
  const stream = streamFor(fd);
  return stream !== null
    && typeof stream === "object"
    && booleanMetadata(stream, "isTTY") === true;
};

function ReadStream(fd, _options) {
  if (!(this instanceof ReadStream)) throw new TypeError("Class constructor ReadStream cannot be invoked without 'new'");
  this.fd = fd;
  this._stream = terminal(fd, "ReadStream");
}

Object.defineProperties(ReadStream.prototype, {
  isTTY: { get() { return booleanMetadata(this._stream, "isTTY"); } },
  isRaw: { get() { return booleanMetadata(this._stream, "isRaw"); } },
});
ReadStream.prototype.setRawMode = function(mode) {
  if (typeof this._stream.setRawMode !== "function") unsupported("ReadStream.setRawMode");
  return this._stream.setRawMode(mode);
};

function WriteStream(fd) {
  if (!(this instanceof WriteStream)) throw new TypeError("Class constructor WriteStream cannot be invoked without 'new'");
  this.fd = fd;
  this._stream = terminal(fd, "WriteStream");
}

Object.defineProperties(WriteStream.prototype, {
  columns: { get() { return numberMetadata(this._stream, "columns"); } },
  rows: { get() { return numberMetadata(this._stream, "rows"); } },
  isTTY: { get() { return booleanMetadata(this._stream, "isTTY"); } },
});
WriteStream.prototype.getColorDepth = function(env) {
  return colorDepthFrom(this._stream, env);
};
WriteStream.prototype.hasColors = function(count, env) {
  const requested = validateColorCount(count);
  return requested <= 2 ** this.getColorDepth(env);
};
WriteStream.prototype.getWindowSize = function() {
  if (typeof this._stream.getWindowSize === "function") return this._stream.getWindowSize();
  const columns = this.columns;
  const rows = this.rows;
  if (columns !== undefined && rows !== undefined) return [columns, rows];
  return unsupported("WriteStream.getWindowSize");
};

export { ReadStream, WriteStream };

const tty = Object.freeze({ isatty, ReadStream, WriteStream });
export default tty;
