import EventEmitter from "node:events";

// The engine does not expose Buffer, so this facade deliberately stores the
// chunks supplied by callers. Strings and Uint8Array values are accepted in
// byte mode; object mode accepts any non-null value.
const streamStates = new Map();
const defaultHighWaterMarks = { objectMode: 16, bytes: 16384 };
const asyncIterator = typeof Symbol === "function" ? Symbol.asyncIterator : undefined;
const iterator = typeof Symbol === "function" ? Symbol.iterator : undefined;

const defer = callback => {
  if (typeof globalThis.setTimeout === "function") return globalThis.setTimeout(callback, 0);
  return Promise.resolve().then(callback);
};
const isObjectLike = value => value !== null && (typeof value === "object" || typeof value === "function");
const isCallable = value => typeof value === "function";
const isFiniteNumber = value => typeof value === "number" && value === value && value !== Infinity && value !== -Infinity;
const isUint8Array = value => typeof Uint8Array === "function" && value instanceof Uint8Array;

const describe = value => value === null
  ? "null"
  : value === undefined
    ? "undefined"
    : typeof value === "object"
      ? "an instance of Object"
      : `${typeof value} (${String(value)})`;

const invalidArgument = (name, expected, value) => {
  const error = new TypeError(`The "${name}" argument must be ${expected}. Received ${describe(value)}`);
  error.code = "ERR_INVALID_ARG_TYPE";
  return error;
};

const invalidValue = (name, value) => {
  const error = new RangeError(`The value of "${name}" is out of range. Received ${String(value)}`);
  error.code = "ERR_OUT_OF_RANGE";
  return error;
};

const streamError = (message, code) => {
  const error = new Error(message);
  if (code !== undefined) error.code = code;
  return error;
};

const abortReason = signal => {
  if (signal.reason !== undefined) return signal.reason;
  const error = new Error("The operation was aborted");
  error.name = "AbortError";
  error.code = "ABORT_ERR";
  return error;
};

const validateSignal = signal => {
  if (signal === undefined) return;
  if (!isObjectLike(signal)
    || !isCallable(signal.addEventListener)
    || !isCallable(signal.removeEventListener)) {
    throw invalidArgument("signal", "an AbortSignal", signal);
  }
};

const optionsObject = options => {
  if (options === undefined) return {};
  if (options === null || typeof options !== "object") throw invalidArgument("options", "an object", options);
  return options;
};

const highWaterMark = (value, objectMode) => {
  if (value === undefined) return objectMode ? defaultHighWaterMarks.objectMode : defaultHighWaterMarks.bytes;
  if (!isFiniteNumber(value) || value < 0 || Math.floor(value) !== value) throw invalidValue("highWaterMark", value);
  return value;
};

const stateFor = stream => {
  const state = isObjectLike(stream) ? streamStates.get(stream) : undefined;
  if (state === undefined) throw invalidArgument("stream", "a stream", stream);
  return state;
};

const stateOrUndefined = stream => isObjectLike(stream) ? streamStates.get(stream) : undefined;

const baseState = (stream, options) => {
  let state = streamStates.get(stream);
  if (state === undefined) {
    state = {
      autoDestroy: options.autoDestroy !== false,
      emitClose: options.emitClose !== false,
      allowHalfOpen: options.allowHalfOpen !== false,
      destroyed: false,
      closed: false,
      error: undefined,
      errorEmitted: false,
      closeEmitted: false,
      destroyStarted: false,
      destroyMethod: isCallable(options.destroy) ? options.destroy : undefined,
    };
    streamStates.set(stream, state);
  } else {
    if (options.autoDestroy === false) state.autoDestroy = false;
    if (options.emitClose === false) state.emitClose = false;
    if (options.allowHalfOpen === false) state.allowHalfOpen = false;
    if (state.destroyMethod === undefined && isCallable(options.destroy)) state.destroyMethod = options.destroy;
  }
  return state;
};

const callUser = (callback, ...args) => {
  if (!isCallable(callback)) return;
  defer(() => callback(...args));
};

const emitError = (stream, error) => {
  const state = stateOrUndefined(stream);
  if (state === undefined || state.errorEmitted) return;
  state.errorEmitted = true;
  // destroy completion is already deferred; preserve EventEmitter's unhandled
  // error behavior instead of silently swallowing a stream failure.
  stream.emit("error", error);
};

const rejectReadableWaiters = (state, error) => {
  if (!state.readableInitialized) return;
  const waiters = state.asyncWaiters.splice(0);
  for (const waiter of waiters) {
    if (error !== undefined) waiter.reject(error);
    else waiter.resolve({ value: undefined, done: true });
  }
};

const abortPendingWrites = (state, error) => {
  if (!state.writableInitialized) return;
  const pendingError = error === undefined
    ? streamError("The stream was destroyed", "ERR_STREAM_DESTROYED")
    : error;
  const entries = state.writeQueue.splice(0);
  for (const entry of entries) {
    if (!entry.callbackDone) {
      entry.callbackDone = true;
      callUser(entry.callback, pendingError);
    }
  }
  if (state.currentWrite !== undefined && !state.currentWrite.callbackDone) {
    state.currentWrite.callbackDone = true;
    callUser(state.currentWrite.callback, pendingError);
  }
  const finishCallbacks = state.finishCallbacks.splice(0);
  for (const callback of finishCallbacks) callUser(callback, pendingError);
  state.writableLength = 0;
  state.writing = false;
};

const destroyStream = (stream, error, callback) => {
  const state = stateFor(stream);
  if (callback !== undefined && !isCallable(callback)) throw invalidArgument("callback", "a function", callback);
  if (state.destroyed) {
    if (callback !== undefined) callUser(callback, state.error);
    return stream;
  }

  state.destroyed = true;
  state.destroyStarted = true;
  if (error !== undefined && state.error === undefined) state.error = error;
  const destroyError = state.error;
  rejectReadableWaiters(state, destroyError);
  abortPendingWrites(state, destroyError);

  let completed = false;
  const finish = destroyFailure => {
    if (completed) return;
    completed = true;
    if (destroyFailure !== undefined && state.error === undefined) state.error = destroyFailure;
    defer(() => {
      state.closed = true;
      state.destroyStarted = false;
      if (state.error !== undefined) emitError(stream, state.error);
      if (state.emitClose && !state.closeEmitted) {
        state.closeEmitted = true;
        stream.emit("close");
      }
      if (callback !== undefined) callback(state.error);
    });
  };

  const customDestroy = state.destroyMethod !== undefined
    ? state.destroyMethod
    : isCallable(stream._destroy)
      ? stream._destroy
      : undefined;
  if (customDestroy !== undefined) {
    try {
      customDestroy.call(stream, error, finish);
    } catch (destroyFailure) {
      finish(destroyFailure);
    }
  } else {
    finish(error);
  }
  return stream;
};

const readableObjectModeFor = options => options.objectMode === true || options.readableObjectMode === true;
const writableObjectModeFor = options => options.objectMode === true || options.writableObjectMode === true;

const initReadable = (stream, options) => {
  const state = baseState(stream, options);
  if (state.readableInitialized) return state;
  const objectMode = readableObjectModeFor(options);
  state.readableInitialized = true;
  state.readable = true;
  state.readableObjectMode = objectMode;
  state.readableHighWaterMark = highWaterMark(options.readableHighWaterMark ?? options.highWaterMark, objectMode);
  state.readableQueue = [];
  state.readableLength = 0;
  state.reading = false;
  state.draining = false;
  state.flowing = null;
  state.endRequested = false;
  state.endEmitted = false;
  state.endScheduled = false;
  state.readableEventScheduled = false;
  state.asyncWaiters = [];
  state.disturbed = false;
  if (isCallable(options.read)) stream._read = options.read;
  return state;
};

const initWritable = (stream, options) => {
  const state = baseState(stream, options);
  if (state.writableInitialized) return state;
  const objectMode = writableObjectModeFor(options);
  state.writableInitialized = true;
  state.writable = true;
  state.writableObjectMode = objectMode;
  state.writableHighWaterMark = highWaterMark(options.writableHighWaterMark ?? options.highWaterMark, objectMode);
  state.writeQueue = [];
  state.writableLength = 0;
  state.currentWrite = undefined;
  state.writing = false;
  state.writableEnded = false;
  state.writableFinished = false;
  state.finalStarted = false;
  state.needDrain = false;
  state.corked = 0;
  state.finishCallbacks = [];
  if (isCallable(options.write)) stream._write = options.write;
  if (isCallable(options.final)) stream._final = options.final;
  return state;
};

const validateChunk = (chunk, objectMode, name) => {
  if (objectMode) {
    if (chunk === null) throw invalidArgument(name, "a non-null value in object mode", chunk);
    return;
  }
  if (typeof chunk === "string" || isUint8Array(chunk)) return;
  throw invalidArgument(name, "a string or Uint8Array", chunk);
};

const chunkWeight = (chunk, objectMode) => {
  if (objectMode) return 1;
  if (typeof chunk === "string") return chunk.length;
  return chunk.byteLength ?? chunk.length;
};

const takeReadableChunk = state => {
  if (state.readableQueue.length === 0) return undefined;
  const entry = state.readableQueue.shift();
  state.readableLength -= entry.weight;
  if (state.readableLength < 0) state.readableLength = 0;
  state.disturbed = true;
  return entry.chunk;
};

const notifyReadable = stream => {
  const state = stateFor(stream);
  if (state.flowing === true || state.readableEventScheduled || stream.listenerCount("readable") === 0) return;
  state.readableEventScheduled = true;
  defer(() => {
    state.readableEventScheduled = false;
    if (!state.destroyed && !state.endEmitted && stream.listenerCount("readable") !== 0) stream.emit("readable");
  });
};

const settleAsyncWaiters = stream => {
  const state = stateFor(stream);
  while (state.asyncWaiters.length !== 0) {
    if (state.readableQueue.length !== 0) {
      const waiter = state.asyncWaiters.shift();
      waiter.resolve({ value: takeReadableChunk(state), done: false });
      continue;
    }
    if (state.error !== undefined) {
      const waiters = state.asyncWaiters.splice(0);
      for (const waiter of waiters) waiter.reject(state.error);
      return;
    }
    if (state.endEmitted || state.destroyed || state.endRequested) {
      const waiters = state.asyncWaiters.splice(0);
      for (const waiter of waiters) waiter.resolve({ value: undefined, done: true });
    }
    return;
  }
};

const emitReadableEnd = stream => {
  const state = stateFor(stream);
  if (state.destroyed || !state.endRequested || state.endEmitted || state.readableLength !== 0 || state.endScheduled) return;
  state.endScheduled = true;
  defer(() => {
    state.endScheduled = false;
    if (state.destroyed || !state.endRequested || state.endEmitted || state.readableLength !== 0) return;
    state.endEmitted = true;
    settleAsyncWaiters(stream);
    stream.emit("end");
    if (state.writableInitialized && !state.allowHalfOpen && !state.writableEnded) stream.end();
    if (state.autoDestroy && (!state.writableInitialized || state.writableFinished)) destroyStream(stream);
  });
};

const maybeRead = stream => {
  const state = stateFor(stream);
  if (state.destroyed || state.endRequested || state.reading || typeof stream._read !== "function") return;
  state.reading = true;
  try {
    stream._read.call(stream, state.readableHighWaterMark);
  } catch (error) {
    state.reading = false;
    destroyStream(stream, error);
    return;
  }
  state.reading = false;
  settleAsyncWaiters(stream);
  if (state.flowing === true) drainReadable(stream);
};

const drainReadable = stream => {
  const state = stateFor(stream);
  if (state.destroyed || state.flowing !== true || state.draining) return;
  state.draining = true;
  try {
    maybeRead(stream);
    while (!state.destroyed && state.flowing === true && state.readableQueue.length !== 0) {
      const chunk = takeReadableChunk(state);
      stream.emit("data", chunk);
      if (state.readableLength < state.readableHighWaterMark) maybeRead(stream);
    }
    if (!state.destroyed && state.flowing === true) {
      maybeRead(stream);
      emitReadableEnd(stream);
    }
  } finally {
    state.draining = false;
  }
};

const configureReadableListener = (stream, eventName) => {
  const state = stateFor(stream);
  if (eventName === "data") {
    state.flowing = true;
    state.disturbed = true;
    drainReadable(stream);
  } else if (eventName === "readable") {
    state.flowing = false;
    maybeRead(stream);
  }
};

const allPipesDrained = state => state.pipes.every(record => !record.awaitDrain);

const detachPipe = (source, record) => {
  if (record.cleaned) return;
  record.cleaned = true;
  const state = stateFor(source);
  const index = state.pipes.indexOf(record);
  if (index !== -1) state.pipes.splice(index, 1);
  source.removeListener("data", record.onData);
  source.removeListener("end", record.onEnd);
  source.removeListener("error", record.onError);
  record.destination.removeListener("drain", record.onDrain);
  record.destination.removeListener("close", record.onClose);
  record.destination.removeListener("finish", record.onFinish);
  if (record.awaitDrain) record.awaitDrain = false;
  if (allPipesDrained(state) && state.pipes.length !== 0) source.resume();
  record.destination.emit("unpipe", source);
};

function Stream(options) {
  if (!(this instanceof Stream)) throw new TypeError("Stream constructor must be called with new");
  const normalized = optionsObject(options);
  EventEmitter.call(this, normalized);
  baseState(this, normalized);
}

Stream.prototype = Object.create(EventEmitter.prototype);
Stream.prototype.constructor = Stream;
Stream.prototype._destroy = function(error, callback) { callback(error); };
Stream.prototype.destroy = function(error, callback) { return destroyStream(this, error, callback); };

function Readable(options) {
  if (!(this instanceof Readable)) throw new TypeError("Readable constructor must be called with new");
  const normalized = optionsObject(options);
  Stream.call(this, normalized);
  const state = initReadable(this, normalized);
  state.pipes = [];
}

Readable.prototype = Object.create(Stream.prototype);
Readable.prototype.constructor = Readable;
Readable.prototype._read = function(_size) {};
Readable.prototype.push = function(chunk, _encoding) {
  const state = stateFor(this);
  if (chunk === null) {
    if (state.endRequested || state.endEmitted) {
      destroyStream(this, streamError("stream.push() after EOF", "ERR_STREAM_PUSH_AFTER_EOF"));
      return false;
    }
    state.endRequested = true;
    settleAsyncWaiters(this);
    emitReadableEnd(this);
    return false;
  }
  if (state.destroyed || state.endRequested || state.endEmitted) {
    destroyStream(this, streamError("stream.push() after EOF", "ERR_STREAM_PUSH_AFTER_EOF"));
    return false;
  }
  validateChunk(chunk, state.readableObjectMode, "chunk");
  const weight = chunkWeight(chunk, state.readableObjectMode);
  if (weight === 0) return state.readableLength < state.readableHighWaterMark;
  state.readableQueue.push({ chunk, weight });
  state.readableLength += weight;
  settleAsyncWaiters(this);
  notifyReadable(this);
  if (state.flowing === true) drainReadable(this);
  return state.readableLength < state.readableHighWaterMark;
};
Readable.prototype.unshift = function(chunk) {
  const state = stateFor(this);
  if (chunk === null) throw invalidArgument("chunk", "a non-null value", chunk);
  if (state.endEmitted || state.destroyed) throw streamError("stream.unshift() after end event", "ERR_STREAM_UNSHIFT_AFTER_END_EVENT");
  validateChunk(chunk, state.readableObjectMode, "chunk");
  const weight = chunkWeight(chunk, state.readableObjectMode);
  if (weight !== 0) {
    state.readableQueue.unshift({ chunk, weight });
    state.readableLength += weight;
    settleAsyncWaiters(this);
    notifyReadable(this);
    if (state.flowing === true) drainReadable(this);
  }
  return state.readableLength < state.readableHighWaterMark;
};
Readable.prototype.read = function(_size) {
  const state = stateFor(this);
  if (state.destroyed && state.error !== undefined) throw state.error;
  state.disturbed = true;
  let chunk = takeReadableChunk(state);
  if (chunk !== undefined) {
    if (state.readableLength < state.readableHighWaterMark) maybeRead(this);
    if (state.readableLength === 0) emitReadableEnd(this);
    return chunk;
  }
  maybeRead(this);
  chunk = takeReadableChunk(state);
  if (chunk !== undefined) {
    if (state.readableLength < state.readableHighWaterMark) maybeRead(this);
    if (state.readableLength === 0) emitReadableEnd(this);
    return chunk;
  }
  emitReadableEnd(this);
  return null;
};
Readable.prototype.resume = function() {
  const state = stateFor(this);
  if (state.destroyed) return this;
  state.flowing = true;
  state.disturbed = true;
  this.emit("resume");
  drainReadable(this);
  return this;
};
Readable.prototype.pause = function() {
  const state = stateFor(this);
  if (state.flowing !== false) {
    state.flowing = false;
    this.emit("pause");
  }
  return this;
};
Readable.prototype.isPaused = function() { return stateFor(this).flowing === false; };
Readable.prototype.pipe = function(destination, options) {
  const state = stateFor(this);
  if (!isObjectLike(destination) || !isCallable(destination.write) || !isCallable(destination.end)) {
    throw invalidArgument("destination", "a writable stream", destination);
  }
  const normalized = optionsObject(options);
  const record = {
    destination,
    awaitDrain: false,
    cleaned: false,
    onData: chunk => {
      if (record.cleaned) return;
      try {
        if (destination.write(chunk) === false) {
          record.awaitDrain = true;
          this.pause();
        }
      } catch (error) {
        this.unpipe(destination);
        if (isCallable(destination.destroy)) destination.destroy(error);
        else destroyStream(this, error);
      }
    },
    onEnd: () => {
      if (record.cleaned || normalized.end === false) return;
      destination.end();
    },
    onError: error => {
      if (!record.cleaned && isCallable(destination.destroy)) destination.destroy(error);
    },
    onDrain: () => {
      record.awaitDrain = false;
      if (allPipesDrained(state)) this.resume();
    },
    onClose: () => this.unpipe(destination),
    onFinish: () => this.unpipe(destination),
  };
  state.pipes.push(record);
  this.on("data", record.onData);
  this.once("end", record.onEnd);
  this.on("error", record.onError);
  destination.on("drain", record.onDrain);
  destination.once("close", record.onClose);
  destination.once("finish", record.onFinish);
  destination.emit("pipe", this);
  if (state.endEmitted) record.onEnd();
  else this.resume();
  return destination;
};
Readable.prototype.unpipe = function(destination) {
  const state = stateFor(this);
  const records = state.pipes.slice();
  for (const record of records) {
    if (destination === undefined || destination === record.destination) detachPipe(this, record);
  }
  return this;
};
Readable.prototype.on = function(eventName, listener) {
  EventEmitter.prototype.on.call(this, eventName, listener);
  configureReadableListener(this, eventName);
  return this;
};
Readable.prototype.addListener = Readable.prototype.on;
Readable.prototype.once = function(eventName, listener) {
  EventEmitter.prototype.once.call(this, eventName, listener);
  configureReadableListener(this, eventName);
  return this;
};
Readable.prototype.prependListener = function(eventName, listener) {
  EventEmitter.prototype.prependListener.call(this, eventName, listener);
  configureReadableListener(this, eventName);
  return this;
};
Readable.prototype.prependOnceListener = function(eventName, listener) {
  EventEmitter.prototype.prependOnceListener.call(this, eventName, listener);
  configureReadableListener(this, eventName);
  return this;
};
Readable.prototype.removeListener = function(eventName, listener) {
  EventEmitter.prototype.removeListener.call(this, eventName, listener);
  const state = stateFor(this);
  if (eventName === "data" && this.listenerCount("data") === 0 && state.pipes.length === 0) state.flowing = false;
  return this;
};
Readable.prototype.off = Readable.prototype.removeListener;

if (asyncIterator !== undefined) {
  Readable.prototype[asyncIterator] = function() {
    const stream = this;
    const state = stateFor(stream);
    state.flowing = false;
    const result = {
      next: () => {
        if (state.error !== undefined) return Promise.reject(state.error);
        const chunk = stream.read();
        if (chunk !== null) return Promise.resolve({ value: chunk, done: false });
        if (state.error !== undefined) return Promise.reject(state.error);
        if (state.endEmitted || state.destroyed || state.endRequested) return Promise.resolve({ value: undefined, done: true });
        return new Promise((resolve, reject) => state.asyncWaiters.push({ resolve, reject }));
      },
      return: () => {
        stream.pause();
        state.asyncWaiters.splice(0).forEach(waiter => waiter.resolve({ value: undefined, done: true }));
        return Promise.resolve({ value: undefined, done: true });
      },
      throw: error => {
        stream.destroy(error);
        return Promise.reject(error);
      },
    };
    result[asyncIterator] = () => result;
    return result;
  };
}

Readable.from = function(iterableValue, options) {
  if (iterableValue === null || iterableValue === undefined) throw invalidArgument("iterable", "an iterable", iterableValue);
  const normalized = Object.assign({}, optionsObject(options));
  if (normalized.objectMode === undefined && normalized.readableObjectMode === undefined) normalized.objectMode = true;
  let source;
  if (typeof iterableValue === "string") {
    let used = false;
    source = { next: () => {
      if (used) return { value: undefined, done: true };
      used = true;
      return { value: iterableValue, done: false };
    } };
  } else if (asyncIterator !== undefined && isCallable(iterableValue[asyncIterator])) {
    source = iterableValue[asyncIterator]();
  } else if (iterator !== undefined && isCallable(iterableValue[iterator])) {
    source = iterableValue[iterator]();
  } else {
    throw invalidArgument("iterable", "an iterable", iterableValue);
  }

  const stream = new Readable(normalized);
  let pulling = false;
  let done = false;
  stream._read = function() {
    if (pulling || done || stateFor(this).destroyed) return;
    pulling = true;
    let nextResult;
    try {
      nextResult = source.next();
    } catch (error) {
      pulling = false;
      destroyStream(this, error);
      return;
    }
    Promise.resolve(nextResult).then(result => {
      pulling = false;
      if (done || stateFor(stream).destroyed) return;
      if (result.done) {
        done = true;
        stream.push(null);
        return;
      }
      try {
        const accepted = stream.push(result.value);
        if (accepted && stateFor(stream).flowing === true) defer(() => stream._read());
      } catch (error) {
        destroyStream(stream, error);
      }
    }, error => {
      pulling = false;
      destroyStream(stream, error);
    });
  };
  return stream;
};

function Writable(options) {
  if (!(this instanceof Stream)) throw new TypeError("Writable constructor must be called with new");
  const normalized = optionsObject(options);
  Stream.call(this, normalized);
  initWritable(this, normalized);
}

Writable.prototype = Object.create(Stream.prototype);
Writable.prototype.constructor = Writable;
Writable.prototype._write = function(_chunk, _encoding, callback) {
  callback(streamError("The _write() method is not implemented", "ERR_METHOD_NOT_IMPLEMENTED"));
};
Writable.prototype._final = function(callback) { callback(); };
Writable.prototype.write = function(chunk, encoding, callback) {
  const state = stateFor(this);
  let actualEncoding = encoding;
  let actualCallback = callback;
  if (isCallable(actualEncoding)) {
    actualCallback = actualEncoding;
    actualEncoding = "utf8";
  }
  if (actualCallback !== undefined && !isCallable(actualCallback)) throw invalidArgument("callback", "a function", actualCallback);
  if (state.destroyed || state.writableEnded) {
    const error = streamError("write after end", state.destroyed ? "ERR_STREAM_DESTROYED" : "ERR_STREAM_WRITE_AFTER_END");
    callUser(actualCallback, error);
    destroyStream(this, error);
    return false;
  }
  validateChunk(chunk, state.writableObjectMode, "chunk");
  const entry = {
    chunk,
    encoding: typeof actualEncoding === "string" ? actualEncoding : "utf8",
    callback: actualCallback,
    callbackDone: false,
    weight: chunkWeight(chunk, state.writableObjectMode),
  };
  state.writeQueue.push(entry);
  state.writableLength += entry.weight;
  const accepted = state.writableLength < state.writableHighWaterMark;
  if (!accepted) state.needDrain = true;
  processWrites(this);
  return accepted;
};

const completeWrite = (stream, entry, error) => {
  const state = stateFor(stream);
  if (state.currentWrite !== entry) return;
  state.currentWrite = undefined;
  state.writing = false;
  state.writableLength -= entry.weight;
  if (state.writableLength < 0) state.writableLength = 0;
  if (!entry.callbackDone) {
    entry.callbackDone = true;
    callUser(entry.callback, error);
  }
  if (error !== undefined) {
    destroyStream(stream, error);
    return;
  }
  if (state.needDrain && state.writableLength < state.writableHighWaterMark && state.writeQueue.length === 0) {
    state.needDrain = false;
    stream.emit("drain");
  }
  processWrites(stream);
};

const processWrites = stream => {
  const state = stateFor(stream);
  if (state.destroyed || state.writing || state.corked !== 0) return;
  const entry = state.writeQueue.shift();
  if (entry === undefined) {
    maybeFinishWritable(stream);
    return;
  }
  state.currentWrite = entry;
  state.writing = true;
  let called = false;
  const done = error => {
    if (called) return;
    called = true;
    defer(() => completeWrite(stream, entry, error));
  };
  try {
    stream._write.call(stream, entry.chunk, entry.encoding, done);
  } catch (error) {
    done(error);
  }
};

const completeFinal = (stream, error) => {
  const state = stateFor(stream);
  if (state.destroyed) return;
  if (error !== undefined) {
    destroyStream(stream, error);
    return;
  }
  state.writableFinished = true;
  stream.emit("prefinish");
  stream.emit("finish");
  const callbacks = state.finishCallbacks.splice(0);
  for (const callback of callbacks) callUser(callback);
  if (state.readableInitialized && !state.allowHalfOpen && !state.endRequested) stream.push(null);
  if (state.autoDestroy && (!state.readableInitialized || state.endEmitted)) destroyStream(stream);
};

const maybeFinishWritable = stream => {
  const state = stateFor(stream);
  if (state.destroyed || !state.writableEnded || state.writing || state.writeQueue.length !== 0 || state.finalStarted) return;
  state.finalStarted = true;
  let called = false;
  const done = error => {
    if (called) return;
    called = true;
    defer(() => completeFinal(stream, error));
  };
  try {
    stream._final.call(stream, done);
  } catch (error) {
    done(error);
  }
};

Writable.prototype.end = function(chunk, encoding, callback) {
  const state = stateFor(this);
  let actualChunk = chunk;
  let actualEncoding = encoding;
  let actualCallback = callback;
  if (isCallable(actualChunk)) {
    actualCallback = actualChunk;
    actualChunk = undefined;
    actualEncoding = undefined;
  } else if (isCallable(actualEncoding)) {
    actualCallback = actualEncoding;
    actualEncoding = undefined;
  }
  if (actualCallback !== undefined && !isCallable(actualCallback)) throw invalidArgument("callback", "a function", actualCallback);
  if (state.writableEnded) {
    const error = streamError("write after end", "ERR_STREAM_WRITE_AFTER_END");
    callUser(actualCallback, error);
    return this;
  }
  if (actualChunk !== undefined && actualChunk !== null) this.write(actualChunk, actualEncoding);
  state.writableEnded = true;
  if (actualCallback !== undefined) state.finishCallbacks.push(actualCallback);
  processWrites(this);
  return this;
};
Writable.prototype.cork = function() {
  stateFor(this).corked += 1;
  return this;
};
Writable.prototype.uncork = function() {
  const state = stateFor(this);
  if (state.corked !== 0) state.corked -= 1;
  if (state.corked === 0) processWrites(this);
  return this;
};
Writable.prototype.destroy = function(error, callback) { return destroyStream(this, error, callback); };

if (typeof Symbol === "function" && Symbol.hasInstance !== undefined) {
  Object.defineProperty(Writable, Symbol.hasInstance, {
    configurable: true,
    value: value => {
      const state = stateOrUndefined(value);
      return state !== undefined && state.writableInitialized;
    },
  });
}

function Duplex(options) {
  if (!(this instanceof Duplex)) throw new TypeError("Duplex constructor must be called with new");
  const normalized = optionsObject(options);
  Stream.call(this, normalized);
  initReadable(this, normalized);
  initWritable(this, normalized);
  const state = stateFor(this);
  state.pipes = [];
}

Duplex.prototype = Object.create(Readable.prototype);
Duplex.prototype.constructor = Duplex;
for (const name of ["_write", "_final", "write", "end", "cork", "uncork"]) {
  Object.defineProperty(Duplex.prototype, name, Object.getOwnPropertyDescriptor(Writable.prototype, name));
}
Object.defineProperty(Duplex.prototype, "writable", {
  configurable: true,
  enumerable: true,
  get() { const state = stateOrUndefined(this); return state === undefined || !state.writableInitialized ? null : !state.destroyed && !state.writableEnded; },
});
Object.defineProperty(Duplex.prototype, "writableEnded", {
  configurable: true,
  enumerable: true,
  get() { const state = stateOrUndefined(this); return state === undefined ? false : state.writableEnded; },
});
Object.defineProperty(Duplex.prototype, "writableFinished", {
  configurable: true,
  enumerable: true,
  get() { const state = stateOrUndefined(this); return state === undefined ? false : state.writableFinished; },
});
Object.defineProperty(Duplex.prototype, "writableLength", {
  configurable: true,
  enumerable: true,
  get() { const state = stateOrUndefined(this); return state === undefined ? 0 : state.writableLength; },
});
Object.defineProperty(Duplex.prototype, "writableHighWaterMark", {
  configurable: true,
  enumerable: true,
  get() { const state = stateOrUndefined(this); return state === undefined ? 0 : state.writableHighWaterMark; },
});
Object.defineProperty(Duplex.prototype, "writableObjectMode", {
  configurable: true,
  enumerable: true,
  get() { const state = stateOrUndefined(this); return state !== undefined && state.writableObjectMode; },
});
Object.defineProperty(Duplex.prototype, "writableNeedDrain", {
  configurable: true,
  enumerable: true,
  get() { const state = stateOrUndefined(this); return state !== undefined && state.needDrain; },
});
Object.defineProperty(Duplex.prototype, "writableCorked", {
  configurable: true,
  enumerable: true,
  get() { const state = stateOrUndefined(this); return state === undefined ? 0 : state.corked; },
});

function Transform(options) {
  if (!(this instanceof Transform)) throw new TypeError("Transform constructor must be called with new");
  const normalized = optionsObject(options);
  Duplex.call(this, normalized);
  if (isCallable(normalized.transform)) this._transform = normalized.transform;
  if (isCallable(normalized.flush)) this._flush = normalized.flush;
  this._write = transformWrite;
  this._final = transformFinal;
}

Transform.prototype = Object.create(Duplex.prototype);
Transform.prototype.constructor = Transform;
Transform.prototype._transform = function(_chunk, _encoding, callback) {
  callback(streamError("The _transform() method is not implemented", "ERR_METHOD_NOT_IMPLEMENTED"));
};
Transform.prototype._flush = function(callback) { callback(); };

const pushTransformResult = (stream, data) => {
  if (data !== undefined && data !== null) stream.push(data);
};

function transformWrite(chunk, encoding, callback) {
  let called = false;
  const done = (error, data) => {
    if (called) return;
    called = true;
    try {
      if (error === undefined || error === null) pushTransformResult(this, data);
      callback(error === null ? undefined : error);
    } catch (pushError) {
      callback(pushError);
    }
  };
  try {
    this._transform.call(this, chunk, encoding, done);
  } catch (error) {
    done(error);
  }
}

function transformFinal(callback) {
  let called = false;
  const done = (error, data) => {
    if (called) return;
    called = true;
    try {
      if (error === undefined || error === null) {
        pushTransformResult(this, data);
        this.push(null);
        callback();
      } else {
        callback(error);
      }
    } catch (pushError) {
      callback(pushError);
    }
  };
  try {
    this._flush.call(this, done);
  } catch (error) {
    done(error);
  }
}

function PassThrough(options) {
  if (!(this instanceof PassThrough)) throw new TypeError("PassThrough constructor must be called with new");
  Transform.call(this, options);
  this._transform = function(chunk, _encoding, callback) { callback(null, chunk); };
}

PassThrough.prototype = Object.create(Transform.prototype);
PassThrough.prototype.constructor = PassThrough;

const defineGetter = (prototype, name, getter) => Object.defineProperty(prototype, name, {
  configurable: true,
  enumerable: true,
  get: getter,
});

defineGetter(Stream.prototype, "destroyed", function() {
  const state = stateOrUndefined(this);
  return state === undefined ? false : state.destroyed;
});
defineGetter(Stream.prototype, "closed", function() {
  const state = stateOrUndefined(this);
  return state === undefined ? false : state.closed;
});
defineGetter(Stream.prototype, "errored", function() {
  const state = stateOrUndefined(this);
  return state === undefined ? null : state.error ?? null;
});
defineGetter(Readable.prototype, "readable", function() {
  const state = stateOrUndefined(this);
  return state === undefined || !state.readableInitialized ? null : !state.destroyed && !state.endEmitted;
});
defineGetter(Readable.prototype, "readableEnded", function() {
  const state = stateOrUndefined(this);
  return state !== undefined && state.endEmitted;
});
defineGetter(Readable.prototype, "readableFlowing", function() {
  const state = stateOrUndefined(this);
  return state === undefined ? null : state.flowing;
});
defineGetter(Readable.prototype, "readableLength", function() {
  const state = stateOrUndefined(this);
  return state === undefined ? 0 : state.readableLength;
});
defineGetter(Readable.prototype, "readableHighWaterMark", function() {
  const state = stateOrUndefined(this);
  return state === undefined ? 0 : state.readableHighWaterMark;
});
defineGetter(Readable.prototype, "readableObjectMode", function() {
  const state = stateOrUndefined(this);
  return state !== undefined && state.readableObjectMode;
});

defineGetter(Writable.prototype, "writable", function() {
  const state = stateOrUndefined(this);
  return state === undefined || !state.writableInitialized ? null : !state.destroyed && !state.writableEnded;
});
defineGetter(Writable.prototype, "writableEnded", function() {
  const state = stateOrUndefined(this);
  return state !== undefined && state.writableEnded;
});
defineGetter(Writable.prototype, "writableFinished", function() {
  const state = stateOrUndefined(this);
  return state !== undefined && state.writableFinished;
});
defineGetter(Writable.prototype, "writableLength", function() {
  const state = stateOrUndefined(this);
  return state === undefined ? 0 : state.writableLength;
});
defineGetter(Writable.prototype, "writableHighWaterMark", function() {
  const state = stateOrUndefined(this);
  return state === undefined ? 0 : state.writableHighWaterMark;
});
defineGetter(Writable.prototype, "writableObjectMode", function() {
  const state = stateOrUndefined(this);
  return state !== undefined && state.writableObjectMode;
});
defineGetter(Writable.prototype, "writableNeedDrain", function() {
  const state = stateOrUndefined(this);
  return state !== undefined && state.needDrain;
});
defineGetter(Writable.prototype, "writableCorked", function() {
  const state = stateOrUndefined(this);
  return state === undefined ? 0 : state.corked;
});

const isStream = value => {
  const state = stateOrUndefined(value);
  return state !== undefined && (state.readableInitialized || state.writableInitialized);
};

export const isDestroyed = stream => {
  const state = stateOrUndefined(stream);
  return state === undefined ? null : state.destroyed;
};
export const isDisturbed = stream => {
  const state = stateOrUndefined(stream);
  return state === undefined ? false : !!state.disturbed;
};
export const isErrored = stream => {
  const state = stateOrUndefined(stream);
  return state === undefined ? false : state.error !== undefined;
};
export const isReadable = stream => {
  const state = stateOrUndefined(stream);
  return state === undefined || !state.readableInitialized ? null : !state.destroyed && !state.endEmitted;
};
export const isWritable = stream => {
  const state = stateOrUndefined(stream);
  return state === undefined || !state.writableInitialized ? null : !state.destroyed && !state.writableEnded;
};
export const destroy = (stream, error, callback) => {
  if (!isObjectLike(stream) || !isCallable(stream.destroy)) throw invalidArgument("stream", "a stream", stream);
  return stream.destroy(error, callback);
};

export const addAbortSignal = (signal, stream) => {
  validateSignal(signal);
  if (!isStream(stream)) throw invalidArgument("stream", "a stream", stream);
  const abort = () => stream.destroy(abortReason(signal));
  if (signal.aborted) abort();
  else {
    signal.addEventListener("abort", abort, { once: true });
    stream.once("close", () => signal.removeEventListener("abort", abort));
  }
  return stream;
};

export const getDefaultHighWaterMark = objectMode => {
  if (typeof objectMode !== "boolean") throw invalidArgument("objectMode", "of type boolean", objectMode);
  return objectMode ? defaultHighWaterMarks.objectMode : defaultHighWaterMarks.bytes;
};
export const setDefaultHighWaterMark = (objectMode, value) => {
  if (typeof objectMode !== "boolean") throw invalidArgument("objectMode", "of type boolean", objectMode);
  if (!isFiniteNumber(value) || value < 0 || Math.floor(value) !== value) throw invalidValue("value", value);
  if (objectMode) defaultHighWaterMarks.objectMode = value;
  else defaultHighWaterMarks.bytes = value;
};

const finishedOptions = (options, callback) => {
  let normalized = options;
  let actualCallback = callback;
  if (isCallable(normalized)) {
    actualCallback = normalized;
    normalized = {};
  } else {
    normalized = optionsObject(normalized);
  }
  if (!isCallable(actualCallback)) throw invalidArgument("callback", "a function", actualCallback);
  validateSignal(normalized.signal);
  return { options: normalized, callback: actualCallback };
};

export const finished = (stream, options, callback) => {
  if (!isStream(stream)) throw invalidArgument("stream", "a stream", stream);
  const parsed = finishedOptions(options, callback);
  const normalized = parsed.options;
  const state = stateFor(stream);
  const wantReadable = normalized.readable !== false && state.readableInitialized;
  const wantWritable = normalized.writable !== false && state.writableInitialized;
  let readableDone = !wantReadable || state.endEmitted;
  let writableDone = !wantWritable || state.writableFinished;
  let settled = false;
  let cleaned = false;

  const cleanup = () => {
    if (cleaned) return;
    cleaned = true;
    stream.removeListener("end", onEnd);
    stream.removeListener("finish", onFinish);
    stream.removeListener("error", onError);
    stream.removeListener("close", onClose);
    if (normalized.signal !== undefined) normalized.signal.removeEventListener("abort", onAbort);
  };
  const complete = error => {
    if (settled) return;
    settled = true;
    if (normalized.cleanup === true) cleanup();
    defer(() => parsed.callback(error));
  };
  const check = () => {
    if (readableDone && writableDone) complete();
  };
  const onEnd = () => { readableDone = true; check(); };
  const onFinish = () => { writableDone = true; check(); };
  const onError = error => complete(error);
  const onClose = () => {
    if (!settled && !(readableDone && writableDone)) complete(streamError("Premature close", "ERR_STREAM_PREMATURE_CLOSE"));
  };
  const onAbort = () => complete(abortReason(normalized.signal));

  stream.once("end", onEnd);
  stream.once("finish", onFinish);
  stream.once("error", onError);
  stream.once("close", onClose);
  if (normalized.signal !== undefined) {
    if (normalized.signal.aborted) onAbort();
    else normalized.signal.addEventListener("abort", onAbort, { once: true });
  }
  check();
  return cleanup;
};

const pipelineArguments = argumentsValue => {
  const values = Array.prototype.slice.call(argumentsValue);
  let callback = values.pop();
  if (!isCallable(callback)) throw invalidArgument("callback", "a function", callback);
  let options = {};
  const last = values[values.length - 1];
  if (isObjectLike(last) && !isStream(last) && !Array.isArray(last)) options = values.pop();
  let streams = values;
  if (streams.length === 1 && Array.isArray(streams[0])) streams = streams[0].slice();
  if (streams.length < 2) throw invalidArgument("streams", "at least two streams", streams);
  for (const stream of streams) if (!isStream(stream)) throw invalidArgument("stream", "a stream", stream);
  validateSignal(options.signal);
  return { streams, options, callback };
};

export const pipeline = function(...args) {
  const parsed = pipelineArguments(args);
  const streams = parsed.streams;
  const options = parsed.options;
  const callback = parsed.callback;
  const destination = streams[streams.length - 1];
  let settled = false;
  const listeners = [];
  let finishCleanup;
  let abortListener;

  const cleanup = () => {
    if (finishCleanup !== undefined) finishCleanup();
    if (options.signal !== undefined && abortListener !== undefined) options.signal.removeEventListener("abort", abortListener);
    for (const entry of listeners) entry.stream.removeListener("error", entry.listener);
  };
  const settle = error => {
    if (settled) return;
    settled = true;
    cleanup();
    if (error !== undefined) {
      for (const stream of streams) if (!stream.destroyed && isCallable(stream.destroy)) stream.destroy(error);
    }
    defer(() => callback(error));
  };
  const onError = error => settle(error);
  for (const stream of streams) {
    const listener = error => onError(error);
    listeners.push({ stream, listener });
    stream.once("error", listener);
  }
  finishCleanup = finished(destination, { readable: false, writable: true, cleanup: true }, error => settle(error));
  if (options.signal !== undefined) {
    abortListener = () => settle(abortReason(options.signal));
    if (options.signal.aborted) {
      abortListener();
      return destination;
    }
    options.signal.addEventListener("abort", abortListener, { once: true });
  }
  try {
    for (let index = 0; index < streams.length - 1; index += 1) streams[index].pipe(streams[index + 1], { end: true });
  } catch (error) {
    settle(error);
  }
  return destination;
};

const finishedPromise = (stream, options) => new Promise((resolve, reject) => {
  try {
    finished(stream, Object.assign({}, optionsObject(options), { cleanup: true }), error => {
      if (error === undefined) resolve();
      else reject(error);
    });
  } catch (error) {
    reject(error);
  }
});

const pipelinePromise = (...args) => new Promise((resolve, reject) => {
  try {
    pipeline(...args, error => {
      if (error === undefined) resolve();
      else reject(error);
    });
  } catch (error) {
    reject(error);
  }
});

const promises = { finished: finishedPromise, pipeline: pipelinePromise };

Stream.Readable = Readable;
Stream.Writable = Writable;
Stream.Duplex = Duplex;
Stream.Transform = Transform;
Stream.PassThrough = PassThrough;
Stream.finished = finished;
Stream.pipeline = pipeline;
Stream.destroy = destroy;
Stream.addAbortSignal = addAbortSignal;
Stream.isDestroyed = isDestroyed;
Stream.isDisturbed = isDisturbed;
Stream.isErrored = isErrored;
Stream.isReadable = isReadable;
Stream.isWritable = isWritable;
Stream.setDefaultHighWaterMark = setDefaultHighWaterMark;
Stream.getDefaultHighWaterMark = getDefaultHighWaterMark;
Stream.Stream = Stream;
Stream.promises = promises;

export {
  Stream,
  Readable,
  Writable,
  Duplex,
  Transform,
  PassThrough,
  promises,
};
export default Stream;
