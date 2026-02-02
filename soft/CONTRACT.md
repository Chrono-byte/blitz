# Kernel ↔ blitzlib contract

Purpose

- Define a small, stable host ABI (the "host table") that the kernel provides
  to blitzlib. blitzlib is the TypeScript host prelude and stdlib that defines
  the user-visible runtime surface. The kernel enforces low-level invariants
  and implements native primitives only accessible via the host table.

Access pattern

- The kernel initializes a private host table containing named Values (functions
  or objects). The only global provided to blitzlib is `__host_get(name)` — a
  builtin function that returns the named entry from the host table.
- blitzlib must call `__host_get('entry_name')` during prelude/bootstrap to
  capture host-provided values into local bindings. The prelude should not
  rely on raw `__native_*` globals being present in the global object.

Minimal host entries (examples)

- `__native_print` — builtin function `(vm, args) -> Value` that writes to host stdout.
- `__native_new_object` — builtin to create a raw VM object for use as a prototype.
- `__host_now` — returned as a number for Date / timers.
- `__host_read_file(path)` / `__host_write_file(path, data)` — optional safe file APIs.

Semantics & guarantees

- Host functions are created once and stored in the kernel host table. They are
  not inserted into the VM globals map and therefore are not directly reachable
  by user code.
- `__host_get` is the only accessor blitzlib may call; it returns a Value that
  blitzlib can stash in local variables/closures. `__host_get` itself is a safe
  builtin implemented by the kernel and should perform defensive checks.

Security

- Keep the number of host entries minimal. Document and review each new entry.
- If sandboxing is required, add capability tokens or restrict which host
  entries are available per-realm.

Example usage (prelude)

```ts
const native_print = __host_get("__native_print");
native_print("hello from prelude");
```
