import assert from "node:assert/strict";
// @ts-expect-error The JS facade is paired with the curated declaration below.
const asyncHooks = await import("../src/facades/node-async-hooks.js") as typeof import("../declarations/node-async-hooks.d.ts");
const {
  AsyncLocalStorage,
  AsyncResource,
  createHook,
  executionAsyncId,
  executionAsyncResource,
  triggerAsyncId,
} = asyncHooks;

const store = new AsyncLocalStorage<{ id: string }>({ name: "request" });
assert.equal(store.name, "request");
assert.equal(store.getStore(), undefined);

const seen: unknown[] = [];
const result = store.run({ id: "outer" }, (suffix: string) => {
  seen.push(store.getStore());
  assert.equal(store.getStore()?.id + suffix, "outer!");
  store.run({ id: "inner" }, () => assert.equal(store.getStore()?.id, "inner"));
  assert.equal(store.getStore()?.id, "outer");
  store.exit(() => assert.equal(store.getStore(), undefined));
  assert.equal(store.getStore()?.id, "outer");
  store.enterWith({ id: "entered" });
  return store.getStore();
}, "!");
assert.deepEqual(result, { id: "entered" });
assert.deepEqual(seen, [{ id: "outer" }]);
assert.equal(store.getStore(), undefined);

const disabled = new AsyncLocalStorage<string>({ defaultValue: "default" });
disabled.run("temporary", () => {
  assert.equal(disabled.getStore(), "temporary");
  disabled.disable();
  assert.equal(disabled.getStore(), "default");
});
assert.equal(disabled.getStore(), "default");
disabled.enterWith("again");
assert.equal(disabled.getStore(), "again");

const captured = store.run({ id: "captured" }, () => ({
  bind: AsyncLocalStorage.bind(() => store.getStore()?.id),
  snapshot: AsyncLocalStorage.snapshot(),
}));
store.run({ id: "other" }, () => {
  assert.equal(captured.bind(), "captured");
  assert.equal(captured.snapshot(() => store.getStore()?.id), "captured");
});

const resource = store.run({ id: "resource" }, () => new AsyncResource("test-resource"));
const resourceId = resource.asyncId();
assert.equal(resourceId, resource.asyncId());
assert.equal(resource.triggerAsyncId(), 1);
assert.equal(executionAsyncId(), 1);
assert.equal(triggerAsyncId(), 0);
assert.equal(typeof executionAsyncResource(), "object");
assert.equal(executionAsyncId(), 1);
store.run({ id: "other" }, () => {
  assert.equal(resource.runInAsyncScope(() => store.getStore()?.id), "resource");
  assert.deepEqual(resource.runInAsyncScope(function(this: { receiver: boolean }) { return this; }, { receiver: true }), { receiver: true });
  const bound = resource.bind(() => store.getStore()?.id);
  assert.equal(bound(), "resource");
});
assert.equal(resource.emitDestroy(), resource);
assert.equal(resource.emitDestroy(), resource);

const receiver = { receiver: true };
const staticBound = store.run({ id: "static" }, () => AsyncResource.bind(function(this: { receiver: boolean }) {
  return [this, store.getStore()?.id, executionAsyncId()];
}, undefined, receiver));
const boundResult = staticBound.call(receiver);
assert.equal(boundResult[0], receiver);
assert.equal(boundResult[1], "static");
assert.notEqual(boundResult[2], 1);

const promiseContinuation = store.run({ id: "promise" }, () => Promise.resolve().then(() => store.getStore()));
assert.equal(store.getStore(), undefined);
assert.equal(await promiseContinuation, undefined);

assert.deepEqual(Object.keys(asyncHooks.default).sort(), [
  "AsyncLocalStorage",
  "AsyncResource",
  "createHook",
  "executionAsyncId",
  "executionAsyncResource",
  "triggerAsyncId",
]);
assert.throws(() => createHook({}), error => (
  error instanceof Error
  && "code" in error
  && error.code === "ERR_NOT_IMPLEMENTED"
  && error.message === "node:async_hooks.createHook is not implemented by Blitz"
));

console.log("node:async_hooks facade tests passed: sync scopes, explicit bindings, stable resource IDs, and lifecycle limitations.");
