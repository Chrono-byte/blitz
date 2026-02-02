// Host accessor helpers for prelude and std modules.
// Resolve the VM-provided `__host_get` builtin via a safe typeof check
// and export wrapper functions so modules can `import * as host`.

// Helper to resolve the VM-provided host accessor at call-time. We avoid
// capturing its value at module initialization because the bundling /
// execution ordering can vary and the global builtin `__host_get` may not be
// visible at module init time in some bootstrap scenarios.
function get_host_get(): any {
  try {
    // The runtime exposes a builtin accessor named `__host_get` in some
    // environments; in others the accessor may be inlined or renamed. Use a
    // defensive lookup and fall back to any available globals captured on
    // `globalThis` under common names. This keeps typechecking happy while
    // allowing the runtime to provide the real accessor at startup.
    // Use `typeof` to avoid a TS error when `__host_get` isn't declared.
    // The emitted JS will still reference the global if present.
    if (typeof (globalThis as any).__host_get !== "undefined") return ((globalThis as any).__host_get as any);
    const g: any = (typeof globalThis !== "undefined" ? (globalThis as any) : undefined);
    if (g && typeof g.__host_get === "function") return g.__host_get;
    return undefined;
  } catch (_) {
    return undefined;
  }
}

export function host_print(msg: string): void {
  try {
    const g = get_host_get();
    if (typeof g === "function") {
      const p = g("__native_print");
      if (typeof p === "function") {
        try {
          p(msg);
        } catch (_) {}
      }
    }
  } catch (_) {}
}

// Export runtime helpers as functions that lookup the host builtin on each
// call. This avoids needing a module-level cached function value which can
// become `null`/`undefined` due to bundling/initialization ordering.
export function native_new_object(): any {
  try {
    const g = get_host_get();
    if (typeof g === "function") {
      const f = g("__native_new_object");
      if (typeof f === "function") {
        try {
          return f();
        } catch (_) {
          return undefined;
        }
      }
    }
  } catch (_) {}
  return undefined;
}

export function native_print(...args: any[]): void {
  try {
    const g = get_host_get();
    if (typeof g === "function") {
      const f = g("__native_print");
      if (typeof f === "function") {
        try {
          f(...args);
        } catch (_) {}
      }
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
    host_print(name + " -> typeof=" + typ + " repr=" + repr);
  } catch (_) {}
}

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
};

// Also export the symbol for module consumers that import named exports.
export { host };

// Also publish these helpers onto the global object so bundled modules that
// access them via global lookups (GetGlobal) can find them. Be defensive and
// only assign when `globalThis` exists.
try {
  if (typeof globalThis !== "undefined") {
    try {
      const g: any = globalThis as any;
      if (g !== null && typeof g === "object") {
        g.host = host;
        g.host_print = host_print;
        g.native_new_object = native_new_object;
        g.native_print = native_print;
        g.debugValue = debugValue;
        // Also expose a callable accessor name used in some prelude variants.
        try {
          // Avoid referencing an undeclared global directly; read from
          // `globalThis` which is available and was assigned to `g` above.
          const getter = (g as any).__host_get;
          if (typeof getter === "function") {
            g.__host_get_fn = getter;
          }
        } catch (_) {}
      }
    } catch (_) {}
  }
} catch (_) {}
