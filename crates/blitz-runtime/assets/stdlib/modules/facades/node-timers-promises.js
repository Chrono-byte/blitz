const hostSetTimeout = globalThis.setTimeout;
const hostClearTimeout = globalThis.clearTimeout;

const abortReason = signal => {
  if (signal.reason !== undefined) return signal.reason;
  const error = new Error("The operation was aborted");
  error.name = "AbortError";
  return error;
};

const validateSignal = signal => {
  if (signal === undefined) return;
  if (signal === null
    || typeof signal !== "object"
    || typeof signal.addEventListener !== "function"
    || typeof signal.removeEventListener !== "function") {
    throw new TypeError("The \"signal\" option must be an AbortSignal");
  }
};

const validateOptions = options => {
  if (options === undefined || options === null) return undefined;
  if (typeof options !== "object") {
    throw new TypeError("The \"options\" argument must be an object");
  }
  const signal = options.signal;
  validateSignal(signal);
  if (options.ref !== undefined && typeof options.ref !== "boolean") {
    throw new TypeError("The \"ref\" option must be a boolean");
  }
  if (options.ref === false) {
    throw new Error("timers/promises ref:false is not supported by Blitz's scheduler");
  }
  return signal;
};

const completed = value => ({ value, done: false });
const finished = { value: undefined, done: true };

function promiseTimeout(delay, value, options) {
  const signal = validateOptions(options);
  const timeout = delay === undefined ? 1 : delay;
  if (signal !== undefined && signal.aborted) return Promise.reject(abortReason(signal));

  return new Promise((resolve, reject) => {
    let timer;
    let settled = false;
    const abort = () => {
      if (timer !== undefined) hostClearTimeout(timer);
      settled = true;
      if (signal !== undefined) signal.removeEventListener("abort", abort);
      reject(abortReason(signal));
    };

    if (signal !== undefined) signal.addEventListener("abort", abort, { once: true });
    if (settled) return;
    timer = hostSetTimeout(() => {
      settled = true;
      if (signal !== undefined) signal.removeEventListener("abort", abort);
      resolve(value);
    }, timeout);
  });
}

function promiseImmediate(value, options) {
  return promiseTimeout(0, value, options);
}

function promiseInterval(delay, value, options) {
  const signal = validateOptions(options);
  const interval = delay === undefined ? 1 : delay;
  let stopped = false;
  let failure;
  let timer;
  let pendingReject;

  const cleanup = () => {
    if (signal !== undefined) signal.removeEventListener("abort", abort);
  };
  const abort = () => {
    if (stopped) return;
    stopped = true;
    failure = abortReason(signal);
    if (timer !== undefined) hostClearTimeout(timer);
    timer = undefined;
    cleanup();
    if (pendingReject !== undefined) {
      const reject = pendingReject;
      pendingReject = undefined;
      reject(failure);
    }
  };
  const stop = () => {
    if (stopped) return;
    stopped = true;
    if (timer !== undefined) hostClearTimeout(timer);
    timer = undefined;
    cleanup();
  };

  if (signal !== undefined) {
    signal.addEventListener("abort", abort);
    if (signal.aborted) abort();
  }

  const iterator = {
    next: () => {
      if (failure !== undefined) return Promise.reject(failure);
      if (stopped) return Promise.resolve(finished);
      return new Promise((resolve, reject) => {
        pendingReject = reject;
        timer = hostSetTimeout(() => {
          timer = undefined;
          pendingReject = undefined;
          resolve(completed(value));
        }, interval);
      });
    },
    return: () => {
      stop();
      return Promise.resolve(finished);
    },
    throw: error => {
      stop();
      return Promise.reject(error);
    },
  };
  if (typeof Symbol === "function" && Symbol.asyncIterator !== undefined) {
    iterator[Symbol.asyncIterator] = () => iterator;
  }
  return iterator;
}

const scheduler = {
  wait: (delay, options) => {
    if (options === undefined || options === null) return promiseTimeout(delay);
    if (typeof options !== "object") {
      throw new TypeError("The \"options\" argument must be an object");
    }
    return promiseTimeout(delay, undefined, { signal: options.signal });
  },
  yield: () => promiseImmediate(),
};

const timersPromises = {
  scheduler,
  setImmediate: promiseImmediate,
  setInterval: promiseInterval,
  setTimeout: promiseTimeout,
};

const setImmediate = promiseImmediate;
const setInterval = promiseInterval;
const setTimeout = promiseTimeout;

export { scheduler, setImmediate, setInterval, setTimeout };
export default timersPromises;
