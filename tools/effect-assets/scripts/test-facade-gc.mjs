import assert from "node:assert/strict";
import { setImmediate } from "node:timers/promises";
import { EventEmitter, setMaxListeners } from "../src/facades/node-events.js";
import { StringDecoder } from "../src/facades/node-string-decoder.js";
import { AsyncResource, AsyncLocalStorage } from "../src/facades/node-async-hooks.js";
import { PassThrough } from "../src/facades/node-stream.js";
import { createInterface, emitKeypressEvents } from "../src/facades/node-readline.js";

assert.equal(typeof global.gc, "function", "run with node --expose-gc");
const factories = {
  emitter: () => new EventEmitter(),
  decoder: () => new StringDecoder(),
  resource: () => new AsyncResource("gc-test").emitDestroy(),
  storage: () => {
    const storage = new AsyncLocalStorage();
    storage.disable();
    return storage;
  },
  stream: () => new PassThrough(),
  readline: () => {
    const reader = createInterface({ input: new EventEmitter(), terminal: false });
    reader.close();
    return reader;
  },
  keypress: () => {
    const input = new EventEmitter();
    emitKeypressEvents(input);
    return input;
  },
  eventTarget: () => {
    const target = new EventTarget();
    setMaxListeners(5, target);
    return target;
  },
};
const references = Object.entries(factories).map(([name, create]) => ({
  name,
  refs: Array.from({ length: 100 }, () => new WeakRef(create())),
}));

// WeakRef keeps a dereferenced target alive until the next job. Yield before
// each collection; allow several collections for cycles and deferred cleanup.
let retained;
for (let attempt = 0; attempt < 20; attempt++) {
  await setImmediate();
  global.gc();
  retained = references.map(({ name, refs }) => ({
    name,
    count: refs.filter((ref) => ref.deref() !== undefined).length,
  }));
  if (retained.every(({ count }) => count === 0)) break;
}
assert.deepEqual(
  retained,
  references.map(({ name }) => ({ name, count: 0 })),
);
console.log("Facade GC checks passed for all eight instance registries.");
