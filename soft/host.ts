import { toString as runtimeToString } from "./core/runtime.js";

// Host accessor helpers for prelude and std modules.
// Resolve the VM-provided `__host_get` builtin via a safe typeof check
// and export wrapper functions so modules can `import * as host`.

// Simple host print helper that calls the runtime-provided printer if present.
// The runtime publishes `__native_print` / `native_print` directly onto
// `globalThis`. During bootstrap these may be missing so this function
// defensively falls back to a no-op.
export function host_print(msg: string): void {
  try {
    const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
    let p: any = undefined;
    if (g) {
      if (typeof g.__native_print === "function") {
        p = g.__native_print;
      } else if (typeof g.__native_print_raw === "function") {
        p = g.__native_print_raw;
      } else if (typeof g.native_print === "function") {
        p = g.native_print;
      } else if (g.host) {
        if (typeof g.host.host_print === "function") {
          p = g.host.host_print;
        }
      }
    }
    if (typeof p === "function") {
      try {
        if (msg !== "") p(msg);
      } catch (_) {}
    }
  } catch (_) {}
}

// Export runtime helpers as functions that lookup the host builtin on each
// call. This avoids needing a module-level cached function value which can
// become `null`/`undefined` due to bundling/initialization ordering.
export function native_new_object(): any {
  try {
    const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
    let f: any = undefined;
    if (g) {
      if (typeof g.__native_new_object === "function") {
        f = g.__native_new_object;
      } else if (typeof g.__native_new_object_raw === "function") {
        f = g.__native_new_object_raw;
      } else if (typeof g.native_new_object === "function") {
        f = g.native_new_object;
      } else if (g.host) {
        if (typeof g.host.native_new_object === "function") {
          f = g.host.native_new_object;
        }
      }
    }
    if (typeof f === "function") {
      try {
        return f();
      } catch (_) {
        return undefined;
      }
    }
  } catch (_) {}
  return undefined;
}

export function native_print(...args: any[]): void {
  try {
    const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
    let f: any = undefined;
    if (g) {
      if (typeof g.__native_print === "function") {
        f = g.__native_print;
      } else if (typeof g.__native_print_raw === "function") {
        f = g.__native_print_raw;
      } else if (typeof g.native_print === "function") {
        f = g.native_print;
      } else if (g.host) {
        if (typeof g.host.native_print === "function") {
          f = g.host.native_print;
        }
      }
    }
    if (typeof f === "function") {
      try {
        const out = (args || [])
          .map((a: any) => {
            try {
              if (a === null) return "null";
              if (typeof a === "undefined") return "undefined";
              if (typeof a === "string") return a;
              if (typeof a === "number") return runtimeToString(a as any);
              if (typeof a === "boolean") return a ? "true" : "false";
              try {
                return JSON.stringify(a);
              } catch (_) {
                return "[object]";
              }
            } catch (_) {
              return "";
            }
          })
          .join(" ");
        f(out);
      } catch (_) {}
    }
  } catch (_) {}
}

// Small helper to print a debug line describing a value's type and a short
// stringified representation. Use host_print so it is safe during bootstrap.
export function debugValue(name: string, v: any): void {
  try {
    let typ: string = typeof v;
    if (v === null) typ = "null";
    let repr: string;
    // Avoid calling global constructors like `String` or `Object` here because
    // during bootstrap those globals may not be initialized or may be host
    // backed values that don't behave like normal JS objects. Use simple
    // primitive checks and fall back to a generic marker for complex values.
    if (v === null) {
      repr = "null";
    } else if (typeof v === "undefined") {
      repr = "undefined";
    } else if (typeof v === "string") {
      repr = v;
    } else if (typeof v === "number" || typeof v === "boolean") {
      try {
        repr = (v as any).toString();
      } catch (_) {
        repr = "[primitive]";
      }
    } else {
      repr = "[object]";
    }
    // Avoid heavy formatting when tracing is disabled. If the host wants
    // tracing it should set a global flag `__TSBLITZ_TRACE` to a truthy value.
    try {
      const traceEnabled = typeof globalThis !== "undefined" && (globalThis as any).__TSBLITZ_TRACE;
      if (!traceEnabled) return;
    } catch (_) {}
    host_print(name + " -> typeof=" + typ + " repr=" + repr);
  } catch (_) {}
}

// Note: we intentionally avoid a complex host-get indirection. The runtime
// publishes native helpers directly on `globalThis` and we read them when
// needed. This keeps bootstrap ordering simple and avoids creating wrappers
// which may confuse the VM during early evaluation.

// Provide a helper that returns a host getter function if available. This
// reads any runtime-published accessor (e.g. __host_get_fn or __host_get)
// from `globalThis` and returns it if callable.
export function getHostGetter(): any {
  try {
    const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
    if (!g) return undefined;
    if (typeof g.__host_get_fn === "function") return g.__host_get_fn;
    if (typeof g.__host_get === "function") return g.__host_get;
  } catch (_) {}
  return undefined;
}

// Provide a helper to access cached builtins if available. Some bootstrapped
// environments publish a `__get_builtin_cached` or similar accessor — fall
// back to undefined when missing.
export function getBuiltinCached(name: string): any {
  try {
    const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
    if (!g) return undefined;
    if (typeof g.__get_builtin_cached === "function") {
      try {
        return g.__get_builtin_cached(name);
      } catch (_) {}
    }
    if (typeof (g as any).getBuiltinCached === "function") {
      try {
        return (g as any).getBuiltinCached(name);
      } catch (_) {}
    }
  } catch (_) {}
  return undefined;
}

// Direct global access for host bindings. The host exposes helpers by
// publishing `__native_print` and `__native_new_object` directly onto
// `globalThis` (moved from a private host table). Avoid complex getter
// indirection; read the global directly when needed.

// Provide a global `host` object so modules that were written as
// `import * as host from "./host"` continue to work when modules are
// inlined by the bundler. This mirrors the exported helpers above.
// Keep this as a non-exported const so it's available as a plain
// top-level binding in the bundled program.
const host = {
  host_print,
  native_new_object,
  native_print,
  debugValue,
  getHostGetter,
  getBuiltinCached,
};

// Also export the symbol for module consumers that import named exports.
export { host };

// Do not publish internal host helpers onto the global object. Exported module
// bindings should be used by other modules instead of relying on global lookups.
// Preserve any existing host-provided runtime globals (e.g. __native_print)
// but do not add or copy internal helpers (such as `host`, `host_print`,
// `native_print`, `debugValue`, `getHostGetter`, or `getBuiltinCached`) onto
// `globalThis`. This avoids leaking low-level host internals into user code.
try {
  if (typeof globalThis !== "undefined") {
    try {
      const g: any = globalThis as any;
      // Intentionally do not assign any host internals to `globalThis`.
      // Consumers should import the helpers from this module when needed.
    } catch (_) {}
  }
} catch (_) {}
