import * as HostCrypto from "blitz-internal:node-crypto-random";

const codedError = (code, message, ErrorConstructor = Error) => {
  const error = new ErrorConstructor(message);
  error.code = code;
  return error;
};

const errorConstructor = code => {
  if (code === "ERR_INVALID_ARG_TYPE" || code === "ERR_INVALID_ARG_VALUE") return TypeError;
  if (code === "ERR_OUT_OF_RANGE" || code === "ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH") return RangeError;
  return Error;
};

const errorFromResult = result => {
  if (result !== null && typeof result === "object" && result.ok === false) {
    const ErrorConstructor = errorConstructor(result.code);
    const error = new ErrorConstructor(result.message ?? result.code ?? "Node crypto operation failed");
    if (result.code !== undefined) error.code = result.code;
    if (result.operation !== undefined) error.operation = result.operation;
    if (result.syscall !== undefined) error.syscall = result.syscall;
    return error;
  }
  return codedError("ERR_CRYPTO_HOST_INVALID_RESULT", "The Node crypto host returned an invalid result");
};

const unwrap = result => {
  if (result !== null && typeof result === "object" && result.ok === true) return result.value;
  throw errorFromResult(result);
};

const requireUint8Array = (value, argument) => {
  if (!(value instanceof Uint8Array)) {
    throw codedError(
      "ERR_INVALID_ARG_TYPE",
      `The ${argument} argument must be a Uint8Array or Buffer in the Blitz crypto slice`,
      TypeError,
    );
  }
  return value;
};

const requireCallback = callback => {
  if (typeof callback !== "function") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The callback argument must be a function", TypeError);
  }
  return callback;
};

const defer = operation => Promise.resolve().then(operation);
const MAX_RANDOM_BYTES = 16 * 1024 * 1024;
const MAX_RANDOM_INT_RANGE = 2 ** 48 - 1;

const randomByteCount = (value, argument = "size") => {
  if (typeof value !== "number") {
    throw codedError("ERR_INVALID_ARG_TYPE", `The ${argument} argument must be a number`, TypeError);
  }
  if (!Number.isInteger(value) || value < 0 || value > MAX_RANDOM_BYTES) {
    throw codedError("ERR_OUT_OF_RANGE", `The value of ${argument} is out of range`, RangeError);
  }
  return value;
};

const randomIntBounds = (min, max) => {
  if (!Number.isSafeInteger(min) || !Number.isSafeInteger(max)) {
    throw codedError("ERR_INVALID_ARG_TYPE", "min and max must be safe integers", TypeError);
  }
  if (max <= min || max - min > MAX_RANDOM_INT_RANGE) {
    throw codedError("ERR_OUT_OF_RANGE", "The randomInt range is out of range", RangeError);
  }
  return { min, max };
};

const fillBounds = (buffer, offset, size) => {
  const actualOffset = offset === undefined ? 0 : randomByteCount(offset, "offset");
  if (actualOffset > buffer.length) {
    throw codedError("ERR_OUT_OF_RANGE", "The randomFill offset is out of range", RangeError);
  }
  const actualSize = size === undefined ? buffer.length - actualOffset : randomByteCount(size);
  if (actualSize > buffer.length - actualOffset) {
    throw codedError("ERR_OUT_OF_RANGE", "The randomFill size is out of range", RangeError);
  }
  return { offset: actualOffset, size: actualSize };
};

export const randomUUID = options => {
  if (options !== undefined && (options === null || typeof options !== "object")) {
    throw codedError("ERR_INVALID_ARG_TYPE", "The options argument must be an object", TypeError);
  }
  if (options !== undefined && options.disableEntropyCache !== undefined && typeof options.disableEntropyCache !== "boolean") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The disableEntropyCache option must be a boolean", TypeError);
  }
  return unwrap(HostCrypto.randomUUID(options));
};

export const randomBytes = (size, callback) => {
  const actualSize = randomByteCount(size);
  if (callback !== undefined) {
    const actualCallback = requireCallback(callback);
    defer(() => unwrap(HostCrypto.randomBytes(actualSize))).then(
      value => actualCallback(null, value),
      error => actualCallback(error),
    );
    return undefined;
  }
  return unwrap(HostCrypto.randomBytes(actualSize));
};

const fillArguments = (offset, size, callback) => {
  let actualOffset = offset;
  let actualSize = size;
  let actualCallback = callback;
  if (typeof actualOffset === "function") {
    actualCallback = actualOffset;
    actualOffset = undefined;
    actualSize = undefined;
  } else if (typeof actualSize === "function") {
    actualCallback = actualSize;
    actualSize = undefined;
  }
  return {
    offset: actualOffset,
    size: actualSize,
    callback: requireCallback(actualCallback),
  };
};

export const randomFill = (buffer, offset, size, callback) => {
  const target = requireUint8Array(buffer, "buffer");
  const parsed = fillArguments(offset, size, callback);
  const bounds = fillBounds(target, parsed.offset, parsed.size);
  defer(() => unwrap(HostCrypto.randomFill(target, bounds.offset, bounds.size))).then(
    value => parsed.callback(null, value),
    error => parsed.callback(error),
  );
  return undefined;
};

export const randomFillSync = (buffer, offset, size) => {
  const target = requireUint8Array(buffer, "buffer");
  const bounds = fillBounds(target, offset, size);
  return unwrap(HostCrypto.randomFill(target, bounds.offset, bounds.size));
};

export const randomInt = (min, max, callback) => {
  let actualMin = 0;
  let actualMax = min;
  let actualCallback = callback;
  if (typeof max === "function") {
    actualCallback = max;
  } else if (max !== undefined) {
    actualMin = min;
    actualMax = max;
  }
  const bounds = randomIntBounds(actualMin, actualMax);
  if (actualCallback !== undefined) {
    const callbackValue = requireCallback(actualCallback);
    defer(() => unwrap(HostCrypto.randomInt(bounds.min, bounds.max))).then(
      value => callbackValue(null, value),
      error => callbackValue(error),
    );
    return undefined;
  }
  return unwrap(HostCrypto.randomInt(bounds.min, bounds.max));
};

export const timingSafeEqual = (a, b) => {
  const left = requireUint8Array(a, "a");
  const right = requireUint8Array(b, "b");
  return unwrap(HostCrypto.timingSafeEqual(left, right));
};

const crypto = {
  randomBytes,
  randomFill,
  randomFillSync,
  randomInt,
  randomUUID,
  timingSafeEqual,
};

export default crypto;
