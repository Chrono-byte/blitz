const states = new WeakMap();
const eventTargetMaxListeners = new WeakMap();
const errorMonitor = Symbol("events.errorMonitor");
const captureRejectionSymbol = typeof Symbol.for === "function"
  ? Symbol.for("nodejs.rejection")
  : Symbol("nodejs.rejection");
let defaultMaxListenersValue = 10;

const isObjectLike = value => value !== null && (typeof value === "object" || typeof value === "function");
const sameEvent = (left, right) => left === right;
const isEmitter = value => isObjectLike(value) && states.has(value);

const maxListeners = value => {
  if (typeof value !== "number" || value !== value) {
    throw new TypeError("The \"n\" argument must be of type number. Received " + String(value));
  }
  if (value < 0) {
    throw new RangeError("The value of \"n\" is out of range. It must be a non-negative number. Received " + String(value));
  }
  return value;
};

const stateFor = emitter => {
  const state = isObjectLike(emitter) ? states.get(emitter) : undefined;
  if (state === undefined) {
    throw new TypeError("The \"emitter\" argument must be an instance of EventEmitter");
  }
  return state;
};

const recordFor = (state, eventName) => {
  for (const record of state.events) {
    if (sameEvent(record.name, eventName)) return record;
  }
  return undefined;
};

const recordForOrCreate = (state, eventName) => {
  const existing = recordFor(state, eventName);
  if (existing !== undefined) return existing;
  const created = { name: eventName, listeners: [] };
  state.events.push(created);
  return created;
};

const removeRecordIfEmpty = (state, record) => {
  if (record.listeners.length !== 0) return;
  const index = state.events.indexOf(record);
  if (index !== -1) state.events.splice(index, 1);
};

const emitRemoveListener = (emitter, eventName, listener) => {
  const state = states.get(emitter);
  const record = state === undefined ? undefined : recordFor(state, "removeListener");
  if (record !== undefined && record.listeners.length !== 0) {
    emitter.emit("removeListener", eventName, listener);
  }
};

const add = (emitter, eventName, listener, prepend, once) => {
  const state = stateFor(emitter);
  if (typeof listener !== "function") {
    throw new TypeError("The \"listener\" argument must be of type function. Received " + String(listener));
  }

  let wrapper = listener;
  if (once) {
    wrapper = function(...args) {
      emitter.removeListener(eventName, wrapper);
      return listener.apply(this, args);
    };
    wrapper.listener = listener;
  }

  const newListenerRecord = recordFor(state, "newListener");
  if (newListenerRecord !== undefined && newListenerRecord.listeners.length !== 0) {
    emitter.emit("newListener", eventName, wrapper);
  }

  const record = recordForOrCreate(state, eventName);
  const entry = { listener, wrapper, once };
  if (prepend) record.listeners.unshift(entry);
  else record.listeners.push(entry);
  return emitter;
};

const removeOne = (emitter, eventName, listener, notify) => {
  const state = stateFor(emitter);
  const record = recordFor(state, eventName);
  if (record === undefined) return emitter;

  for (let index = record.listeners.length - 1; index >= 0; index -= 1) {
    const entry = record.listeners[index];
    if (entry.listener !== listener && entry.wrapper !== listener) continue;
    record.listeners.splice(index, 1);
    removeRecordIfEmpty(state, record);
    if (notify) emitRemoveListener(emitter, eventName, entry.listener);
    return emitter;
  }
  return emitter;
};

const abortReason = signal => {
  if (signal.reason !== undefined) return signal.reason;
  const error = new Error("The operation was aborted");
  error.name = "AbortError";
  return error;
};

const captureRejection = (emitter, eventName, args, result) => {
  const state = states.get(emitter);
  if (state === undefined || !state.captureRejections || result === undefined || result === null) return;
  if (typeof result.then !== "function") return;
  const reject = reason => {
    const rejectionHandler = emitter[captureRejectionSymbol];
    if (typeof rejectionHandler === "function") {
      rejectionHandler.call(emitter, reason, eventName, ...args);
    } else {
      emitter.emit("error", reason);
    }
  };
  try {
    result.then(undefined, reject);
  } catch (error) {
    emitter.emit("error", error);
  }
};

const invoke = (emitter, eventName, entry, args) => {
  const result = entry.wrapper.apply(emitter, args);
  captureRejection(emitter, eventName, args, result);
};

function EventEmitter(options) {
  if (!(this instanceof EventEmitter)) return new EventEmitter(options);
  EventEmitter.init.call(this, options);
}

EventEmitter.prototype.addListener = function(eventName, listener) {
  return add(this, eventName, listener, false, false);
};
EventEmitter.prototype.on = EventEmitter.prototype.addListener;
EventEmitter.prototype.prependListener = function(eventName, listener) {
  return add(this, eventName, listener, true, false);
};
EventEmitter.prototype.once = function(eventName, listener) {
  return add(this, eventName, listener, false, true);
};
EventEmitter.prototype.prependOnceListener = function(eventName, listener) {
  return add(this, eventName, listener, true, true);
};
EventEmitter.prototype.removeListener = function(eventName, listener) {
  return removeOne(this, eventName, listener, true);
};
EventEmitter.prototype.off = EventEmitter.prototype.removeListener;
EventEmitter.prototype.removeAllListeners = function(eventName) {
  const state = stateFor(this);
  if (arguments.length === 0) {
    const records = state.events.slice();
    for (const record of records) {
      if (!sameEvent(record.name, "removeListener")) this.removeAllListeners(record.name);
    }
    if (recordFor(state, "removeListener") !== undefined) this.removeAllListeners("removeListener");
    return this;
  }

  const record = recordFor(state, eventName);
  if (record === undefined) return this;
  const entries = record.listeners.slice();
  for (const entry of entries) removeOne(this, eventName, entry.wrapper, true);
  return this;
};
EventEmitter.prototype.emit = function(eventName, ...args) {
  const state = stateFor(this);
  if (sameEvent(eventName, "error")) {
    const monitor = recordFor(state, errorMonitor);
    if (monitor !== undefined) {
      for (const entry of monitor.listeners.slice()) invoke(this, errorMonitor, entry, args);
    }
  }

  const record = recordFor(state, eventName);
  if (record === undefined || record.listeners.length === 0) {
    if (sameEvent(eventName, "error")) {
      let error = args[0];
      if (args.length === 0 || error === undefined) {
        error = new Error("Unhandled error.");
        error.code = "ERR_UNHANDLED_ERROR";
        error.context = args[0];
      }
      const domain = this.domain;
      if (domain !== null && domain !== undefined && typeof domain._errorHandler === "function") {
        domain._errorHandler(error, this);
        return false;
      }
      throw error;
    }
    return false;
  }

  for (const entry of record.listeners.slice()) invoke(this, eventName, entry, args);
  return true;
};
EventEmitter.prototype.listeners = function(eventName) {
  const state = stateFor(this);
  const record = recordFor(state, eventName);
  return record === undefined ? [] : record.listeners.map(entry => entry.listener);
};
EventEmitter.prototype.rawListeners = function(eventName) {
  const state = stateFor(this);
  const record = recordFor(state, eventName);
  return record === undefined ? [] : record.listeners.map(entry => entry.wrapper);
};
EventEmitter.prototype.listenerCount = function(eventName, listener) {
  const listeners = this.listeners(eventName);
  if (listener === undefined) return listeners.length;
  let count = 0;
  for (const candidate of listeners) if (candidate === listener) count += 1;
  return count;
};
EventEmitter.prototype.eventNames = function() {
  const state = stateFor(this);
  return state.events.filter(record => record.listeners.length !== 0).map(record => record.name);
};
EventEmitter.prototype.getMaxListeners = function() {
  const state = stateFor(this);
  return state.maxListeners === undefined ? EventEmitter.defaultMaxListeners : state.maxListeners;
};
EventEmitter.prototype.setMaxListeners = function(value) {
  const state = stateFor(this);
  state.maxListeners = maxListeners(value);
  return this;
};

const listenerCount = (emitter, eventName) => {
  stateFor(emitter);
  return emitter.listenerCount(eventName);
};
const getEventListeners = (emitter, eventName) => {
  if (isEmitter(emitter)) return emitter.listeners(eventName);
  if (isObjectLike(emitter) && typeof emitter.addEventListener === "function") return [];
  throw new TypeError("The \"emitter\" argument must be an EventEmitter or EventTarget");
};
const getMaxListeners = emitter => {
  if (isEmitter(emitter)) return emitter.getMaxListeners();
  if (isObjectLike(emitter) && typeof emitter.addEventListener === "function") {
    const value = eventTargetMaxListeners.get(emitter);
    return value === undefined ? EventEmitter.defaultMaxListeners : value;
  }
  throw new TypeError("The \"emitter\" argument must be an EventEmitter or EventTarget");
};
const setMaxListeners = (value, ...emitters) => {
  const validated = maxListeners(value);
  if (emitters.length === 0) {
    EventEmitter.defaultMaxListeners = validated;
    return undefined;
  }
  for (const emitter of emitters) {
    if (isEmitter(emitter)) emitter.setMaxListeners(validated);
    else if (isObjectLike(emitter) && typeof emitter.addEventListener === "function") eventTargetMaxListeners.set(emitter, validated);
    else throw new TypeError("The \"emitter\" argument must be an EventEmitter or EventTarget");
  }
  return undefined;
};

const once = (emitter, eventName, options) => {
  stateFor(emitter);
  const signal = options && options.signal;
  return new Promise((resolve, reject) => {
    let settled = false;
    const cleanup = () => {
      emitter.removeListener(eventName, resolver);
      if (!sameEvent(eventName, "error")) emitter.removeListener("error", errorListener);
      if (signal !== undefined && signal.removeEventListener) signal.removeEventListener("abort", abort);
    };
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      cleanup();
      callback(value);
    };
    const resolver = (...args) => finish(resolve, args);
    const errorListener = error => finish(reject, error);
    const abort = () => finish(reject, abortReason(signal));

    if (signal !== undefined) {
      if (typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function") {
        reject(new TypeError("The \"signal\" option must be an AbortSignal"));
        return;
      }
      if (signal.aborted) {
        reject(abortReason(signal));
        return;
      }
      signal.addEventListener("abort", abort, { once: true });
    }
    if (!sameEvent(eventName, "error")) emitter.once("error", errorListener);
    emitter.once(eventName, resolver);
  });
};

const on = (emitter, eventName, options) => {
  stateFor(emitter);
  const signal = options && options.signal;
  if (signal !== undefined && (typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function")) {
    throw new TypeError("The \"signal\" option must be an AbortSignal");
  }
  const queue = [];
  const waiters = [];
  const closeEvents = options === undefined ? undefined : options.close;
  const closeNames = closeEvents === undefined ? [] : Array.isArray(closeEvents) ? closeEvents : [closeEvents];
  let finished = false;
  let failure;

  let cleanup = () => undefined;
  const finish = error => {
    if (finished) return;
    finished = true;
    failure = error;
    cleanup();
    while (waiters.length !== 0) {
      const waiter = waiters.shift();
      if (error === undefined) waiter.resolve({ value: undefined, done: true });
      else waiter.reject(error);
    }
  };
  const push = (...args) => {
    if (finished) return;
    if (waiters.length !== 0) waiters.shift().resolve({ value: args, done: false });
    else queue.push(args);
  };
  const errorListener = error => finish(error);
  const closeListener = () => finish(undefined);
  const abort = () => finish(abortReason(signal));

  const iterator = {
    next: () => {
      if (queue.length !== 0) return Promise.resolve({ value: queue.shift(), done: false });
      if (finished) return failure === undefined
        ? Promise.resolve({ value: undefined, done: true })
        : Promise.reject(failure);
      return new Promise((resolve, reject) => waiters.push({ resolve, reject }));
    },
    return: () => {
      finish(undefined);
      return Promise.resolve({ value: undefined, done: true });
    },
    throw: error => {
      finish(error);
      return Promise.reject(error);
    },
  };
  const asyncIterator = Symbol.asyncIterator;
  if (asyncIterator !== undefined) iterator[asyncIterator] = () => iterator;

  cleanup = () => {
    emitter.removeListener(eventName, push);
    if (!sameEvent(eventName, "error")) emitter.removeListener("error", errorListener);
    for (const closeName of closeNames) emitter.removeListener(closeName, closeListener);
    if (signal !== undefined && signal.removeEventListener) signal.removeEventListener("abort", abort);
  };
  emitter.on(eventName, push);
  if (!sameEvent(eventName, "error")) emitter.on("error", errorListener);
  for (const closeName of closeNames) {
    if (!sameEvent(closeName, eventName)) emitter.once(closeName, closeListener);
  }
  if (signal !== undefined) {
    if (typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function") {
      finish(new TypeError("The \"signal\" option must be an AbortSignal"));
    } else if (signal.aborted) {
      finish(abortReason(signal));
    } else {
      signal.addEventListener("abort", abort, { once: true });
    }
  }
  return iterator;
};

const addAbortListener = (signal, listener) => {
  if (!isObjectLike(signal) || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function") {
    throw new TypeError("The \"signal\" argument must be an AbortSignal");
  }
  if (typeof listener !== "function") throw new TypeError("The \"listener\" argument must be of type function");
  let disposed = false;
  const abort = event => {
    if (disposed) return;
    disposed = true;
    signal.removeEventListener("abort", abort);
    listener(event);
  };
  if (signal.aborted) abort({ type: "abort", target: signal, currentTarget: signal });
  else signal.addEventListener("abort", abort, { once: true });
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    signal.removeEventListener("abort", abort);
  };
  const result = { dispose };
  const disposeSymbol = Symbol.dispose;
  if (disposeSymbol !== undefined) result[disposeSymbol] = dispose;
  return result;
};

Object.defineProperty(EventEmitter, "defaultMaxListeners", {
  configurable: true,
  enumerable: true,
  get: () => defaultMaxListenersValue,
  set: value => { defaultMaxListenersValue = maxListeners(value); },
});
EventEmitter.captureRejections = false;
EventEmitter.errorMonitor = errorMonitor;
EventEmitter.captureRejectionSymbol = captureRejectionSymbol;
EventEmitter.EventEmitter = EventEmitter;
EventEmitter.once = once;
EventEmitter.on = on;
EventEmitter.getEventListeners = getEventListeners;
EventEmitter.getMaxListeners = getMaxListeners;
EventEmitter.listenerCount = listenerCount;
EventEmitter.setMaxListeners = setMaxListeners;
EventEmitter.addAbortListener = addAbortListener;
EventEmitter.usingDomains = false;
EventEmitter.init = function(options) {
  if (states.has(this)) return;
  const capture = options && typeof options.captureRejections === "boolean"
    ? options.captureRejections
    : EventEmitter.captureRejections;
  states.set(this, {
    events: [],
    maxListeners: undefined,
    captureRejections: capture === true,
  });
};

export {
  EventEmitter,
  addAbortListener,
  captureRejectionSymbol,
  errorMonitor,
  getEventListeners,
  getMaxListeners,
  listenerCount,
  on,
  once,
  setMaxListeners,
};
export const usingDomains = false;
export const captureRejections = EventEmitter.captureRejections;
export const defaultMaxListeners = EventEmitter.defaultMaxListeners;
export default EventEmitter;
