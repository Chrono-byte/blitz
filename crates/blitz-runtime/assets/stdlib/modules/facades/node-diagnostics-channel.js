const channels = new Map();

const invalidArgument = (name, expected, value) => {
  let received;
  if (value === undefined) received = "undefined";
  else if (value === null) received = "null";
  else if (typeof value === "function") received = "function ";
  else if (typeof value === "object") received = `an instance of ${value.constructor && value.constructor.name || "Object"}`;
  else received = `type ${typeof value} (${String(value)})`;
  const error = new TypeError(`The "${name}"${name.indexOf(".") !== -1 ? " property" : " argument"} must be ${expected}. Received ${received}`);
  error.code = "ERR_INVALID_ARG_TYPE";
  throw error;
};

const validateFunction = (value, name) => {
  if (typeof value !== "function") invalidArgument(name, "of type function", value);
};

const reportUncaught = error => {
  const process = globalThis.process;
  if (process !== undefined && typeof process.nextTick === "function") {
    process.nextTick(() => { throw error; });
  } else {
    throw error;
  }
};

const defaultTransform = data => data;
const apply = (fn, thisArg, args) => Function.prototype.apply.call(fn, thisArg, args);

const activate = channel => {
  if (channel._subscribers !== undefined) return;
  Object.setPrototypeOf(channel, ActiveChannel.prototype);
  channel._subscribers = [];
  channel._stores = new Map();
};

const maybeDeactivate = channel => {
  if (channel._subscribers.length !== 0 || channel._stores.size !== 0) return;
  Object.setPrototypeOf(channel, Channel.prototype);
  channel._subscribers = undefined;
  channel._stores = undefined;
};

// AsyncLocalStorage is not part of the Blitz facade surface. Support compatible
// store.run objects when supplied, and otherwise run the callback normally.
const runStore = (store, data, next, transform) => () => {
  if (store === null || store === undefined || typeof store.run !== "function") return next();

  let context;
  try {
    context = (transform === undefined ? defaultTransform : transform)(data);
  } catch (error) {
    reportUncaught(error);
    return next();
  }
  return store.run(context, next);
};

class Channel {
  constructor(name) {
    this._subscribers = undefined;
    this._stores = undefined;
    this.name = name;
    channels.set(name, this);
  }

  subscribe(subscription) {
    activate(this);
    this.subscribe(subscription);
  }

  unsubscribe() {
    return false;
  }

  bindStore(store, transform) {
    activate(this);
    this.bindStore(store, transform);
  }

  unbindStore() {
    return false;
  }

  get hasSubscribers() {
    return false;
  }

  publish() {}

  runStores(data, fn, thisArg, ...args) {
    return apply(fn, thisArg, args);
  }
}

class ActiveChannel {
  subscribe(subscription) {
    validateFunction(subscription, "subscription");
    this._subscribers = this._subscribers.slice();
    this._subscribers.push(subscription);
  }

  unsubscribe(subscription) {
    const index = this._subscribers.indexOf(subscription);
    if (index === -1) return false;

    this._subscribers = this._subscribers.slice(0, index).concat(this._subscribers.slice(index + 1));
    maybeDeactivate(this);
    return true;
  }

  bindStore(store, transform) {
    if (!this._stores.has(store)) this._stores.set(store, transform);
    else this._stores.set(store, transform);
  }

  unbindStore(store) {
    if (!this._stores.has(store)) return false;
    this._stores.delete(store);
    maybeDeactivate(this);
    return true;
  }

  get hasSubscribers() {
    return true;
  }

  publish(data) {
    const subscribers = this._subscribers;
    for (let index = 0; index < subscribers.length; index += 1) {
      try {
        subscribers[index](data, this.name);
      } catch (error) {
        reportUncaught(error);
      }
    }
  }

  runStores(data, fn, thisArg, ...args) {
    let run = () => {
      this.publish(data);
      return apply(fn, thisArg, args);
    };

    for (const entry of this._stores.entries()) {
      run = runStore(entry[0], data, run, entry[1]);
    }
    return run();
  }
}

Object.setPrototypeOf(ActiveChannel.prototype, Channel.prototype);
Object.defineProperty(Channel, Symbol.hasInstance, {
  value: instance => {
    const prototype = Object.getPrototypeOf(instance);
    return prototype === Channel.prototype || prototype === ActiveChannel.prototype;
  },
});

const channel = name => {
  const existing = channels.get(name);
  if (existing !== undefined) return existing;
  if (typeof name !== "string" && typeof name !== "symbol") {
    invalidArgument("channel", "one of type string or symbol", name);
  }
  return new Channel(name);
};

const hasSubscribers = name => {
  const existing = channels.get(name);
  return existing === undefined ? false : existing.hasSubscribers;
};

const subscribe = (name, onMessage) => channel(name).subscribe(onMessage);
const unsubscribe = (name, onMessage) => channel(name).unsubscribe(onMessage);

const traceEvents = ["start", "end", "asyncStart", "asyncEnd", "error"];

const assertChannel = (value, name) => {
  if (!(value instanceof Channel)) invalidArgument(name, "an instance of Channel", value);
};

const tracingChannelFrom = (nameOrChannels, name) => {
  if (typeof nameOrChannels === "string") return channel(`tracing:${nameOrChannels}:${name}`);
  if (typeof nameOrChannels === "object" && nameOrChannels !== null) {
    const value = nameOrChannels[name];
    assertChannel(value, `nameOrChannels.${name}`);
    return value;
  }
  invalidArgument("nameOrChannels", "one of type string, object, or TracingChannel", nameOrChannels);
};

class TracingChannel {
  constructor(nameOrChannels) {
    for (const name of traceEvents) {
      Object.defineProperty(this, name, {
        value: tracingChannelFrom(nameOrChannels, name),
      });
    }
  }

  get hasSubscribers() {
    return this.start.hasSubscribers
      || this.end.hasSubscribers
      || this.asyncStart.hasSubscribers
      || this.asyncEnd.hasSubscribers
      || this.error.hasSubscribers;
  }

  subscribe(handlers) {
    for (const name of traceEvents) {
      if (!handlers[name]) continue;
      this[name].subscribe(handlers[name]);
    }
  }

  unsubscribe(handlers) {
    let done = true;
    for (const name of traceEvents) {
      if (!handlers[name]) continue;
      if (!this[name].unsubscribe(handlers[name])) done = false;
    }
    return done;
  }

  traceSync(fn, context = {}, thisArg, ...args) {
    if (!this.hasSubscribers) return apply(fn, thisArg, args);
    const start = this.start;
    const end = this.end;
    const error = this.error;

    return start.runStores(context, () => {
      try {
        const result = apply(fn, thisArg, args);
        context.result = result;
        return result;
      } catch (failure) {
        context.error = failure;
        error.publish(context);
        throw failure;
      } finally {
        end.publish(context);
      }
    });
  }

  tracePromise(fn, context = {}, thisArg, ...args) {
    if (!this.hasSubscribers) return apply(fn, thisArg, args);
    const start = this.start;
    const end = this.end;
    const asyncStart = this.asyncStart;
    const asyncEnd = this.asyncEnd;
    const error = this.error;

    const reject = failure => {
      context.error = failure;
      error.publish(context);
      asyncStart.publish(context);
      asyncEnd.publish(context);
      return Promise.reject(failure);
    };
    const resolve = result => {
      context.result = result;
      asyncStart.publish(context);
      asyncEnd.publish(context);
      return result;
    };

    return start.runStores(context, () => {
      try {
        let promise = apply(fn, thisArg, args);
        if (!(promise instanceof Promise)) promise = Promise.resolve(promise);
        return promise.then(resolve, reject);
      } catch (failure) {
        context.error = failure;
        error.publish(context);
        throw failure;
      } finally {
        end.publish(context);
      }
    });
  }

  traceCallback(fn, position = -1, context = {}, thisArg, ...args) {
    if (!this.hasSubscribers) return apply(fn, thisArg, args);
    const start = this.start;
    const end = this.end;
    const asyncStart = this.asyncStart;
    const asyncEnd = this.asyncEnd;
    const error = this.error;
    const callbackIndex = position < 0 ? args.length + position : position;
    const callback = args[callbackIndex];
    validateFunction(callback, "callback");

    const wrappedCallback = function(errorValue, result) {
      if (errorValue) {
        context.error = errorValue;
        error.publish(context);
      } else {
        context.result = result;
      }

      return asyncStart.runStores(context, () => {
        try {
          return apply(callback, this, arguments);
        } finally {
          asyncEnd.publish(context);
        }
      });
    };
    args.splice(position, 1, wrappedCallback);

    return start.runStores(context, () => {
      try {
        return apply(fn, thisArg, args);
      } catch (failure) {
        context.error = failure;
        error.publish(context);
        throw failure;
      } finally {
        end.publish(context);
      }
    });
  }
}

const tracingChannel = nameOrChannels => new TracingChannel(nameOrChannels);

const diagnosticsChannel = {
  channel,
  hasSubscribers,
  subscribe,
  tracingChannel,
  unsubscribe,
  Channel,
};

export {
  Channel,
  channel,
  hasSubscribers,
  subscribe,
  tracingChannel,
  unsubscribe,
};
export default diagnosticsChannel;
