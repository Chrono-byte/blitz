import EventEmitter from "node:events";

// readline works with streams supplied by the caller. It does not create or
// own a terminal: terminal mode, raw mode, and stream lifetime remain the
// responsibility of the input/output objects.
const interfaceStates = new WeakMap();
const keypressStates = new WeakMap();
const asyncIterator = typeof Symbol === "function" ? Symbol.asyncIterator : undefined;

const isObjectLike = value => value !== null && (typeof value === "object" || typeof value === "function");
const isCallable = value => typeof value === "function";

const invalidArgument = (name, expected, value) => {
  const error = new TypeError(`The "${name}" argument must be ${expected}. Received ${String(value)}`);
  error.code = "ERR_INVALID_ARG_TYPE";
  return error;
};

const optionsObject = options => {
  if (options === undefined) return {};
  if (!isObjectLike(options)) throw invalidArgument("options", "an object", options);
  return options;
};

const outputWrite = (output, value, callback) => {
  if (!isObjectLike(output) || !isCallable(output.write)) {
    throw invalidArgument("output", "a writable stream", output);
  }
  return output.write(value, callback);
};

const controlSequence = (output, value, callback) => outputWrite(output, value, callback);

export const clearLine = (output, direction, callback) => {
  let actualDirection = direction;
  let actualCallback = callback;
  if (isCallable(actualDirection)) {
    actualCallback = actualDirection;
    actualDirection = 0;
  }
  if (actualDirection !== -1 && actualDirection !== 0 && actualDirection !== 1) {
    throw invalidArgument("dir", "-1, 0, or 1", actualDirection);
  }
  return controlSequence(output, actualDirection === -1 ? "\u001b[1K" : actualDirection === 1 ? "\u001b[0K" : "\u001b[2K", actualCallback);
};

export const clearScreenDown = (output, callback) => controlSequence(output, "\u001b[0J", callback);

export const cursorTo = (output, x, y, callback) => {
  let actualY = y;
  let actualCallback = callback;
  if (isCallable(actualY)) {
    actualCallback = actualY;
    actualY = undefined;
  }
  if (typeof x !== "number" || !Number.isInteger(x) || x < 0) throw invalidArgument("x", "a non-negative integer", x);
  if (actualY !== undefined && (typeof actualY !== "number" || !Number.isInteger(actualY) || actualY < 0)) {
    throw invalidArgument("y", "a non-negative integer", actualY);
  }
  const sequence = actualY === undefined
    ? `\u001b[${x + 1}G`
    : `\u001b[${actualY + 1};${x + 1}H`;
  return controlSequence(output, sequence, actualCallback);
};

export const moveCursor = (output, dx, dy, callback) => {
  if (typeof dx !== "number" || !Number.isInteger(dx)) throw invalidArgument("dx", "an integer", dx);
  if (typeof dy !== "number" || !Number.isInteger(dy)) throw invalidArgument("dy", "an integer", dy);
  let sequence = "";
  if (dx > 0) sequence += `\u001b[${dx}C`;
  else if (dx < 0) sequence += `\u001b[${-dx}D`;
  if (dy > 0) sequence += `\u001b[${dy}B`;
  else if (dy < 0) sequence += `\u001b[${-dy}A`;
  return controlSequence(output, sequence, callback);
};

const decodeChunk = chunk => {
  if (typeof chunk === "string") return chunk;
  if (typeof Uint8Array === "function" && chunk instanceof Uint8Array) {
    if (typeof TextDecoder === "function") return new TextDecoder().decode(chunk);
  }
  return String(chunk);
};

const lineError = () => {
  const error = new Error("readline was closed");
  error.code = "ERR_READLINE_CLOSED";
  return error;
};

const flushPendingLines = state => {
  if (state.paused || state.closed) return;
  const pending = state.pendingLines.splice(0);
  for (const line of pending) dispatchLine(state, line);
};

const dispatchLine = (state, line) => {
  if (state.closed) return;
  if (state.questions.length !== 0) {
    const question = state.questions.shift();
    if (question.signal !== undefined) question.signal.removeEventListener("abort", question.abort);
    question.callback(line);
    return;
  }
  if (state.iteratorStarted) {
    const waiter = state.lineWaiters.shift();
    if (waiter !== undefined) waiter.resolve({ value: line, done: false });
    else state.lineQueue.push(line);
  }
  state.interface.emit("line", line);
};

const emitLine = (state, line) => {
  if (state.paused) state.pendingLines.push(line);
  else dispatchLine(state, line);
};

const consumeChunk = (state, chunk) => {
  if (state.closed) return;
  const text = decodeChunk(chunk);
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (state.afterCarriageReturn) {
      state.afterCarriageReturn = false;
      if (character === "\n") continue;
    }
    if (character === "\r" || character === "\n") {
      emitLine(state, state.lineBuffer);
      state.lineBuffer = "";
      state.afterCarriageReturn = character === "\r";
    } else {
      state.lineBuffer += character;
    }
  }
};

const finishInput = state => {
  if (state.closed) return;
  if (state.lineBuffer.length !== 0) {
    emitLine(state, state.lineBuffer);
    state.lineBuffer = "";
  }
  closeInterface(state);
};

const removeInputListeners = state => {
  if (!state.input.removeListener && !state.input.off) return;
  const remove = state.input.removeListener || state.input.off;
  remove.call(state.input, "data", state.onData);
  remove.call(state.input, "end", state.onEnd);
  remove.call(state.input, "close", state.onClose);
  remove.call(state.input, "error", state.onError);
};

const closeInterface = state => {
  if (state.closed) return;
  state.closed = true;
  removeInputListeners(state);
  for (const question of state.questions.splice(0)) {
    if (question.signal !== undefined) question.signal.removeEventListener("abort", question.abort);
  }
  const waiters = state.lineWaiters.splice(0);
  for (const waiter of waiters) waiter.resolve({ value: undefined, done: true });
  state.interface.emit("close");
};

const validateInput = input => {
  if (!isObjectLike(input)) throw invalidArgument("input", "a readable stream", input);
  const hasEvents = isCallable(input.on) && (isCallable(input.removeListener) || isCallable(input.off));
  const hasAsyncIterator = asyncIterator !== undefined && isCallable(input[asyncIterator]);
  if (!hasEvents && !hasAsyncIterator) throw invalidArgument("input", "a readable stream", input);
};

const attachInput = state => {
  const input = state.input;
  if (isCallable(input.on) && (isCallable(input.removeListener) || isCallable(input.off))) {
    input.on("data", state.onData);
    input.on("end", state.onEnd);
    input.on("close", state.onClose);
    input.on("error", state.onError);
    return;
  }
  // An async iterable input has no event surface to subscribe to. Its
  // lifetime is still observed, but it is never closed or destroyed by us.
  const iterator = input[asyncIterator]();
  const read = () => {
    if (state.closed) return;
    let next;
    try { next = iterator.next(); } catch (error) { state.onError(error); return; }
    Promise.resolve(next).then(result => {
      if (state.closed) return;
      if (result.done) state.onEnd();
      else { state.onData(result.value); read(); }
    }, state.onError);
  };
  read();
};

const questionAbort = (state, question) => {
  const index = state.questions.indexOf(question);
  if (index !== -1) state.questions.splice(index, 1);
};

const abortError = signal => {
  const error = new Error("The operation was aborted");
  error.name = "AbortError";
  error.code = "ABORT_ERR";
  if (signal !== undefined && signal.reason !== undefined) error.cause = signal.reason;
  return error;
};

function Interface(options, output, completer, terminal) {
  if (!(this instanceof Interface)) throw new TypeError("Class constructor Interface cannot be invoked without 'new'");
  const normalized = arguments.length > 1
    ? { input: options, output, completer, terminal }
    : optionsObject(options);
  validateInput(normalized.input);
  EventEmitter.call(this, normalized);
  this.input = normalized.input;
  this.output = normalized.output;
  this.completer = normalized.completer;
  this.terminal = normalized.terminal === true;
  this.setPrompt(normalized.prompt === undefined ? "> " : normalized.prompt);
  const state = {
    interface: this,
    input: normalized.input,
    paused: false,
    closed: false,
    lineBuffer: "",
    afterCarriageReturn: false,
    pendingLines: [],
    lineQueue: [],
    lineWaiters: [],
    iteratorStarted: false,
    iteratorDone: false,
    questions: [],
    onData: chunk => consumeChunk(state, chunk),
    onEnd: () => finishInput(state),
    onClose: () => finishInput(state),
    onError: error => {
      if (state.closed) return;
      this.emit("error", error);
      closeInterface(state);
    },
  };
  interfaceStates.set(this, state);
  attachInput(state);
}

Interface.prototype = Object.create(EventEmitter.prototype);
Interface.prototype.constructor = Interface;

Object.defineProperties(Interface.prototype, {
  closed: { configurable: true, enumerable: true, get() {
    const state = interfaceStates.get(this);
    return state === undefined ? true : state.closed;
  } },
});

Interface.prototype.setPrompt = function(prompt) {
  if (typeof prompt !== "string") throw invalidArgument("prompt", "a string", prompt);
  this._prompt = prompt;
  return this;
};
Interface.prototype.getPrompt = function() { return this._prompt; };
Interface.prototype.prompt = function() {
  const state = interfaceStates.get(this);
  if (state === undefined || state.closed || this.output === undefined) return this;
  outputWrite(this.output, this._prompt);
  return this;
};
Interface.prototype.pause = function() {
  const state = interfaceStates.get(this);
  if (state === undefined || state.closed || state.paused) return this;
  state.paused = true;
  if (isCallable(state.input.pause)) state.input.pause();
  this.emit("pause");
  return this;
};
Interface.prototype.resume = function() {
  const state = interfaceStates.get(this);
  if (state === undefined || state.closed) return this;
  if (isCallable(state.input.resume)) state.input.resume();
  state.paused = false;
  flushPendingLines(state);
  this.emit("resume");
  return this;
};
Interface.prototype.close = function() {
  const state = interfaceStates.get(this);
  if (state !== undefined) closeInterface(state);
  return this;
};
Interface.prototype.question = function(query, options, callback) {
  const state = interfaceStates.get(this);
  let actualOptions = options;
  let actualCallback = callback;
  if (isCallable(actualOptions)) {
    actualCallback = actualOptions;
    actualOptions = undefined;
  }
  if (!isCallable(actualCallback)) throw invalidArgument("callback", "a function", actualCallback);
  if (state === undefined || state.closed) return this;
  const normalized = optionsObject(actualOptions);
  const question = { callback: actualCallback, signal: normalized.signal, abort: undefined };
  question.abort = () => questionAbort(state, question);
  if (question.signal !== undefined) {
    if (!isObjectLike(question.signal) || !isCallable(question.signal.addEventListener) || !isCallable(question.signal.removeEventListener)) {
      throw invalidArgument("signal", "an AbortSignal", question.signal);
    }
    if (question.signal.aborted) return this;
    question.signal.addEventListener("abort", question.abort, { once: true });
  }
  state.questions.push(question);
  if (typeof query !== "string") throw invalidArgument("query", "a string", query);
  if (this.output !== undefined) outputWrite(this.output, query);
  return this;
};
Interface.prototype.write = function(data, key) {
  // This is intentionally a small, non-terminal editing hook. It is useful
  // to callers that already own a keypress decoder, without claiming raw TTY
  // ownership or changing the input stream's mode.
  if (key !== undefined && key.name === "backspace") return this;
  if (typeof data === "string") consumeChunk(interfaceStates.get(this), data);
  return this;
};

if (asyncIterator !== undefined) {
  Interface.prototype[asyncIterator] = function() {
    const state = interfaceStates.get(this);
    if (state === undefined) return { next: () => Promise.resolve({ value: undefined, done: true }) };
    state.iteratorStarted = true;
    const iterator = {
      next: () => {
        if (state.lineQueue.length !== 0) return Promise.resolve({ value: state.lineQueue.shift(), done: false });
        if (state.closed || state.iteratorDone) return Promise.resolve({ value: undefined, done: true });
        return new Promise((resolve, reject) => state.lineWaiters.push({ resolve, reject }));
      },
      return: () => {
        state.iteratorDone = true;
        for (const waiter of state.lineWaiters.splice(0)) waiter.resolve({ value: undefined, done: true });
        return Promise.resolve({ value: undefined, done: true });
      },
      throw: error => {
        state.iteratorDone = true;
        for (const waiter of state.lineWaiters.splice(0)) waiter.reject(error);
        return Promise.reject(error);
      },
    };
    iterator[asyncIterator] = () => iterator;
    return iterator;
  };
}

const key = (sequence, name, extra) => Object.assign({
  sequence,
  name,
  ctrl: false,
  meta: false,
  shift: false,
}, extra);

const keyFor = sequence => {
  if (sequence === "\r" || sequence === "\n") return ["return", key(sequence, "return")];
  if (sequence === "\u007f") return ["backspace", key(sequence, "backspace")];
  if (sequence === "\u0003") return ["c", key(sequence, "c", { ctrl: true })];
  if (sequence === "\u0004") return ["d", key(sequence, "d", { ctrl: true })];
  if (sequence === "\u0001") return ["a", key(sequence, "a", { ctrl: true })];
  if (sequence === "\u0005") return ["e", key(sequence, "e", { ctrl: true })];
  const arrows = { "\u001b[A": "up", "\u001b[B": "down", "\u001b[C": "right", "\u001b[D": "left", "\u001b[H": "home", "\u001b[F": "end" };
  if (arrows[sequence] !== undefined) return [undefined, key(sequence, arrows[sequence])];
  if (sequence.startsWith("\u001b")) return [undefined, key(sequence, "escape")];
  return [sequence, key(sequence, sequence)];
};

export const emitKeypressEvents = (stream, _interface) => {
  if (!isObjectLike(stream) || !isCallable(stream.on) || !isCallable(stream.emit)) {
    throw invalidArgument("stream", "a readable stream", stream);
  }
  if (keypressStates.has(stream)) return;
  const state = { pendingEscape: "" };
  const onData = chunk => {
    const text = decodeChunk(chunk);
    let index = 0;
    while (index < text.length) {
      let sequence = text[index];
      if (sequence === "\u001b") {
        const candidate = text.slice(index, index + 3);
        if (candidate.length === 3 && candidate[1] === "[" && "ABCDHF".indexOf(candidate[2]) !== -1) {
          sequence = candidate;
          index += 3;
        } else {
          index += 1;
        }
      } else index += 1;
      const parsed = keyFor(sequence);
      stream.emit("keypress", parsed[0], parsed[1]);
    }
  };
  keypressStates.set(stream, onData);
  stream.on("data", onData);
};

const promiseWrite = (output, value) => new Promise((resolve, reject) => {
  try {
    const callback = () => resolve(null);
    const result = outputWrite(output, value, callback);
    // Plain host writers commonly expose only write(value). Do not leave a
    // promise pending merely because such a writer cannot accept a callback.
    if (output.write.length < 2 || result === undefined) resolve(null);
  } catch (error) {
    reject(error);
  }
});

function Readline(output) {
  if (!(this instanceof Readline)) throw new TypeError("Class constructor Readline cannot be invoked without 'new'");
  if (!isObjectLike(output) || !isCallable(output.write)) throw invalidArgument("stream", "a writable stream", output);
  this.output = output;
  this.actions = [];
}
Readline.prototype.cursorTo = function(x, y) { this.actions.push({ kind: "cursor", x, y }); return this; };
Readline.prototype.moveCursor = function(dx, dy) { this.actions.push({ kind: "move", dx, dy }); return this; };
Readline.prototype.clearLine = function(direction) { this.actions.push({ kind: "line", direction }); return this; };
Readline.prototype.clearScreenDown = function() { this.actions.push({ kind: "screen" }); return this; };
Readline.prototype.commit = function() {
  const actions = this.actions.splice(0);
  const output = this.output;
  let sequence = "";
  for (const action of actions) {
    if (action.kind === "cursor") sequence += action.y === undefined ? `\u001b[${action.x + 1}G` : `\u001b[${action.y + 1};${action.x + 1}H`;
    else if (action.kind === "move") {
      if (action.dx > 0) sequence += `\u001b[${action.dx}C`;
      else if (action.dx < 0) sequence += `\u001b[${-action.dx}D`;
      if (action.dy > 0) sequence += `\u001b[${action.dy}B`;
      else if (action.dy < 0) sequence += `\u001b[${-action.dy}A`;
    } else if (action.kind === "line") sequence += action.direction === -1 ? "\u001b[1K" : action.direction === 1 ? "\u001b[0K" : "\u001b[2K";
    else sequence += "\u001b[0J";
  }
  return sequence.length === 0 ? Promise.resolve(null) : promiseWrite(output, sequence);
};
Readline.prototype.rollback = function() { this.actions.length = 0; return this; };

function PromiseInterface(options, output, completer, terminal) {
  if (!(this instanceof PromiseInterface)) throw new TypeError("Class constructor Interface cannot be invoked without 'new'");
  if (arguments.length > 1) Interface.call(this, options, output, completer, terminal);
  else Interface.call(this, options);
}
PromiseInterface.prototype = Object.create(Interface.prototype);
PromiseInterface.prototype.constructor = PromiseInterface;
PromiseInterface.prototype.question = function(query, options) {
  const state = interfaceStates.get(this);
  if (state === undefined || state.closed) return Promise.reject(lineError());
  const signal = options !== null && typeof options === "object" ? options.signal : undefined;
  if (signal !== undefined && signal.aborted) return Promise.reject(abortError(signal));
  return new Promise((resolve, reject) => {
    const cleanup = () => {
      this.removeListener("close", onClose);
      if (signal !== undefined) signal.removeEventListener("abort", onAbort);
    };
    const onClose = () => { cleanup(); reject(lineError()); };
    const onAbort = () => { cleanup(); reject(abortError(signal)); };
    this.once("close", onClose);
    if (signal !== undefined) signal.addEventListener("abort", onAbort, { once: true });
    Interface.prototype.question.call(this, query, options, answer => {
      cleanup();
      resolve(answer);
    });
  });
};

function promiseCreateInterface(inputOrOptions, output, completer, terminal) {
  const options = isObjectLike(inputOrOptions) && "input" in inputOrOptions
    ? inputOrOptions
    : { input: inputOrOptions, output, completer, terminal };
  return new PromiseInterface(options);
}
const promises = Object.freeze({ Interface: PromiseInterface, Readline, createInterface: promiseCreateInterface });

export { Interface, promises };

function createInterface(inputOrOptions, output, completer, terminal) {
  const options = isObjectLike(inputOrOptions) && "input" in inputOrOptions
    ? inputOrOptions
    : { input: inputOrOptions, output, completer, terminal };
  return new Interface(options);
}

const readline = Object.freeze({
  Interface,
  clearLine,
  clearScreenDown,
  createInterface,
  cursorTo,
  emitKeypressEvents,
  moveCursor,
  promises,
});

export { createInterface };
export default readline;
