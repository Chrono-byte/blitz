// This facade implements the synchronous part of node:async_hooks that can be
// represented without VM lifecycle callbacks. Promise continuations are not
// wrapped: a run/enterWith context therefore ends when the current callback
// returns unless an AsyncResource or bind/snapshot wrapper is used explicitly.

const TOP_LEVEL_ID = 1;
let nextAsyncId = TOP_LEVEL_ID + 1;

const invalidArgument = (name, expected, value) => {
  const received = value === null ? "null" : value === undefined ? "undefined" : typeof value;
  const error = new TypeError(`The "${name}" argument must be ${expected}. Received ${received}`);
  error.code = "ERR_INVALID_ARG_TYPE";
  throw error;
};

const validateFunction = (value, name) => {
  if (typeof value !== "function") invalidArgument(name, "of type function", value);
};

const notImplemented = operation => {
  const error = new Error(`node:async_hooks.${operation} is not implemented by Blitz`);
  error.code = "ERR_NOT_IMPLEMENTED";
  throw error;
};

const resourceStates = new WeakMap();
const topLevelResource = {};
resourceStates.set(topLevelResource, {
  asyncId: TOP_LEVEL_ID,
  triggerAsyncId: 0,
  stores: new Map(),
  destroyed: false,
});

// A context is intentionally an ordinary VM value. Do not use a host-side
// async-local implementation here: this module must have the same behavior
// when it is evaluated by Blitz rather than by Node.
let currentContext = {
  resource: topLevelResource,
  stores: new Map(),
};

const storageState = new WeakMap();

const contextStores = context => new Map(context.stores);
const capturedStores = () => contextStores(currentContext);

const activeEntry = (storage, stores) => {
  const entry = stores.get(storage);
  const state = storageState.get(storage);
  return entry !== undefined && state !== undefined && entry.generation === state.generation
    ? entry
    : undefined;
};

const withStores = (stores, callback, thisArg, args) => {
  const previous = currentContext;
  currentContext = { resource: previous.resource, stores };
  try {
    return Function.prototype.apply.call(callback, thisArg, args);
  } finally {
    currentContext = previous;
  }
};

const withResource = (resource, callback, thisArg, args) => {
  const previous = currentContext;
  const state = resourceStates.get(resource);
  currentContext = { resource, stores: new Map(state.stores) };
  try {
    return Function.prototype.apply.call(callback, thisArg, args);
  } finally {
    currentContext = previous;
  }
};

const bindStores = (stores, callback) => {
  validateFunction(callback, "fn");
  return function(...args) {
    // A bound function keeps the receiver supplied at call time, like
    // AsyncResource.bind and AsyncLocalStorage.bind in Node.
    return withStores(new Map(stores), callback, this, args);
  };
};

class AsyncLocalStorage {
  constructor(options) {
    if (options !== undefined && (options === null || typeof options !== "object")) {
      invalidArgument("options", "of type object", options);
    }
    const settings = options === undefined ? {} : options;
    const state = {
      defaultValue: settings.defaultValue,
      hasDefaultValue: Object.prototype.hasOwnProperty.call(settings, "defaultValue"),
      generation: 0,
      name: settings.name === undefined ? "" : String(settings.name),
    };
    storageState.set(this, state);
  }

  static bind(callback) {
    return bindStores(capturedStores(), callback);
  }

  static snapshot() {
    const stores = capturedStores();
    return (callback, ...args) => {
      validateFunction(callback, "fn");
      return withStores(new Map(stores), callback, undefined, args);
    };
  }

  disable() {
    const state = storageState.get(this);
    if (state === undefined) return;
    state.generation += 1;
    const stores = contextStores(currentContext);
    stores.delete(this);
    currentContext = { resource: currentContext.resource, stores };
  }

  getStore() {
    const state = storageState.get(this);
    if (state === undefined) return undefined;
    const entry = activeEntry(this, currentContext.stores);
    if (entry !== undefined) return entry.value;
    return state.hasDefaultValue ? state.defaultValue : undefined;
  }

  enterWith(store) {
    const state = storageState.get(this);
    if (state === undefined) return;
    const stores = contextStores(currentContext);
    stores.set(this, { generation: state.generation, value: store });
    currentContext = { resource: currentContext.resource, stores };
  }

  run(store, callback, ...args) {
    validateFunction(callback, "callback");
    const state = storageState.get(this);
    if (state === undefined) return Function.prototype.apply.call(callback, undefined, args);
    const stores = contextStores(currentContext);
    stores.set(this, { generation: state.generation, value: store });
    return withStores(stores, callback, undefined, args);
  }

  exit(callback, ...args) {
    validateFunction(callback, "callback");
    const stores = contextStores(currentContext);
    stores.delete(this);
    return withStores(stores, callback, undefined, args);
  }

  get name() {
    const state = storageState.get(this);
    return state === undefined ? "" : state.name;
  }
}

class AsyncResource {
  constructor(type, triggerOrOptions) {
    if (typeof type !== "string") invalidArgument("type", "of type string", type);

    let triggerId = executionAsyncId();
    if (typeof triggerOrOptions === "number") {
      triggerId = triggerOrOptions;
    } else if (triggerOrOptions !== undefined) {
      if (triggerOrOptions === null || typeof triggerOrOptions !== "object") {
        invalidArgument("triggerAsyncId", "a number or an object", triggerOrOptions);
      }
      if (triggerOrOptions.triggerAsyncId !== undefined) triggerId = triggerOrOptions.triggerAsyncId;
    }
    if (typeof triggerId !== "number" || !Number.isFinite(triggerId)) {
      invalidArgument("triggerAsyncId", "of type number", triggerId);
    }

    resourceStates.set(this, {
      asyncId: nextAsyncId,
      triggerAsyncId: triggerId,
      stores: capturedStores(),
      destroyed: false,
      type,
    });
    nextAsyncId += 1;
  }

  static bind(callback, type, thisArg) {
    validateFunction(callback, "fn");
    const resource = new AsyncResource(type === undefined ? (callback.name || "bound-anonymous-fn") : type);
    return resource._bind(callback, arguments.length >= 3, thisArg);
  }

  _bind(callback, hasThisArg, thisArg) {
    const resource = this;
    const bound = function(...args) {
      return resource.runInAsyncScope(callback, hasThisArg ? thisArg : this, ...args);
    };
    Object.defineProperty(bound, "asyncResource", {
      configurable: true,
      value: resource,
    });
    return bound;
  }

  bind(callback) {
    validateFunction(callback, "fn");
    return this._bind(callback, false, undefined);
  }

  runInAsyncScope(callback, thisArg, ...args) {
    validateFunction(callback, "fn");
    return withResource(this, callback, thisArg, args);
  }

  emitDestroy() {
    // There is no truthful lifecycle hook to notify. Keep this idempotent,
    // matching the observable return behavior without claiming destroy events.
    const state = resourceStates.get(this);
    if (state !== undefined) state.destroyed = true;
    return this;
  }

  asyncId() {
    return resourceStates.get(this).asyncId;
  }

  triggerAsyncId() {
    return resourceStates.get(this).triggerAsyncId;
  }
}

const executionAsyncId = () => resourceStates.get(currentContext.resource).asyncId;
const triggerAsyncId = () => resourceStates.get(currentContext.resource).triggerAsyncId;
const executionAsyncResource = () => currentContext.resource;

const createHook = () => notImplemented("createHook");

const asyncHooks = Object.freeze({
  AsyncLocalStorage,
  AsyncResource,
  createHook,
  executionAsyncId,
  executionAsyncResource,
  triggerAsyncId,
});

export {
  AsyncLocalStorage,
  AsyncResource,
  createHook,
  executionAsyncId,
  executionAsyncResource,
  triggerAsyncId,
};
export default asyncHooks;
