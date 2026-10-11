import * as promises from "node:timers/promises";

const hostSetTimeout = globalThis.setTimeout;
const hostSetInterval = globalThis.setInterval;
const hostClearTimeout = globalThis.clearTimeout;
const hostClearInterval = globalThis.clearInterval;

const setTimeout = (callback, delay, ...args) => hostSetTimeout(
  callback,
  delay === undefined ? 1 : delay,
  ...args,
);
const setInterval = (callback, delay, ...args) => hostSetInterval(
  callback,
  delay === undefined ? 1 : delay,
  ...args,
);

const setImmediate = (callback, ...args) => hostSetTimeout(callback, 0, ...args);
const numericHandle = handle => typeof handle === "string" ? Number(handle) : handle;
const clearTimeout = timeout => hostClearTimeout(numericHandle(timeout));
const clearInterval = timeout => hostClearInterval(numericHandle(timeout));
const clearImmediate = immediate => hostClearTimeout(numericHandle(immediate));

const timers = {
  clearImmediate,
  clearInterval,
  clearTimeout,
  promises,
  setImmediate,
  setInterval,
  setTimeout,
};

export {
  clearImmediate,
  clearInterval,
  clearTimeout,
  promises,
  setImmediate,
  setInterval,
  setTimeout,
};
export default timers;
