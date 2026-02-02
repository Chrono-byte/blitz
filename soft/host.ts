// Host accessor helpers for prelude and std modules.
// Resolve the VM-provided `__host_get` builtin via a safe typeof check
// and export wrapper functions so modules can `import * as host`.

// Resolve the VM-provided host accessor once and cache it. Historically we
// performed a defensive inline lookup at every call site to avoid ordering
// bugs during bundling. That is robust but expensive (many lookups on hot
// paths). Instead, resolve once on first use and fall back to a defensive
// lookup if needed. This keeps boot-time safety while removing repetitive
// host-bridge overhead.

export function host_print(msg: string): void {
  try {
    const g = getHostGetter();
    if (typeof g === "function") {
      const p = getBuiltinCached(g, "__native_print");
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
    const g = getHostGetter();
    if (typeof g === "function") {
      const f = getBuiltinCached(g, "__native_new_object");
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
    const g = getHostGetter();
    if (typeof g === "function") {
      const f = getBuiltinCached(g, "__native_print");
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
    // Avoid heavy formatting when tracing is disabled. If the host wants
    // tracing it should set a global flag `__TSBLITZ_TRACE` to a truthy value.
    try {
      const traceEnabled = typeof globalThis !== "undefined" && (globalThis as any).__TSBLITZ_TRACE;
      if (!traceEnabled) return;
    } catch (_) {}
    host_print(name + " -> typeof=" + typ + " repr=" + repr);
  } catch (_) {}
}

// --- internal cached helpers ---
let cachedHostGetter: any | null | undefined = undefined;
const builtinCache: Record<string, any> = {};

function resolveHostGetter(): any | null {
  try {
    if (typeof globalThis !== "undefined") {
      const gg: any = (globalThis as any);
      if (typeof gg.__host_get === "function") return gg.__host_get;
      if (typeof gg.__host_get_fn === "function") return gg.__host_get_fn;
      // Fall back to top-level `__host_get` if present
      const top = (globalThis as any).__host_get;
      if (typeof top === "function") return top;
    }
  } catch (_) {}
  return null;
}

function getHostGetter(): any | null {
  if (cachedHostGetter !== undefined) return cachedHostGetter;
  const g = resolveHostGetter();
  cachedHostGetter = g;
  return g;
}

function getBuiltinCached(getter: any, name: string): any {
  if (Object.prototype.hasOwnProperty.call(builtinCache, name)) return builtinCache[name];
  try {
    const v = getter(name);
    builtinCache[name] = v;
    return v;
  } catch (_) {
    builtinCache[name] = undefined;
    return undefined;
  }
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
        // Expose cached resolver helpers on the global to avoid bundler/init
        // ordering problems: some bundle variants reference these helpers as
        // globals. Publishing them here keeps them available to inlined code.
        try {
          (g as any).getHostGetter = getHostGetter;
          (g as any).getBuiltinCached = getBuiltinCached;
        } catch (_) {}
      }
    } catch (_) {}
  }
} catch (_) {}
