/**
 * Prelude: loaded first by the VM to set up globals (std + more).
 * All of this runs on the Rust VM; the actual runtime is in Rust.
 * This file prepares the environment for user code that expects a full ECMAScript
 * environment, including all built-in objects and functions.
 */

import type {
  Value,
  Console,
  ObjectConstructor,
  ArrayConstructor,
  StringConstructor,
  BlitzObject,
  BlitzFunction,
} from "./core/types.js";
import { BlitzObject as ObjectImpl, BlitzFunction as FunctionImpl } from "./core/objects.js";
import { toString as runtimeToString } from "./core/runtime.js";
// Host helpers are provided by the inlined `soft/host.ts` module and expose
// top-level functions such as `host_print`, `native_new_object`, `native_print`
// and `debugValue`. Import them explicitly from the host module so the soft
// prelude doesn't accidentally shadow or redefine these helpers.
import {
  host_print,
  native_new_object,
  native_print,
  debugValue as host_debugValue,
  host as host_ns,
} from "./host.js";
import "./runtime_builtins.js";
try { host_print("PRELUDE: runtime_builtins imported"); } catch (_) {}
// Diagnostic: report whether runtime_builtins exported $262.agent.getReport
try {
    if (typeof globalThis !== "undefined") {
        const g: any = globalThis as any;
        try { host_print("PRELUDE DIAG: typeof $262 = " + typeof g.$262); } catch (_) {}
        try { host_print("PRELUDE DIAG: typeof $262.agent = " + (g.$262 ? typeof g.$262.agent : "<missing>")); } catch (_) {}
        try { host_print("PRELUDE DIAG: typeof $262.agent.getReport = " + (g.$262 && g.$262.agent ? typeof g.$262.agent.getReport : "<missing>")); } catch (_) {}
    }
} catch (_) {}

// Enable tracing for host.debugValue during bootstrap to help capture
// type/shape information that may be lost otherwise.
try {
    if (typeof globalThis !== "undefined") (globalThis as any).__TSBLITZ_TRACE = true;
    try { host_print("PRELUDE: trace enabled"); } catch (_) {}
} catch (_) {}

// NOTE: Do not shim or polyfill harness APIs from the prelude. The harness
// must load itself and the runtime should provide compliant builtins. Any
// missing harness helpers should be implemented in `runtime_builtins.ts` or
// the host environment, not here.

// Marker and top-level try/catch to surface a full stack trace when the
// prelude fails during VM bootstrap. Tests (harness) can then report the
// exact file/line from the thrown error stack.
let __prelude_marker = "start";
try {

    // Note: std modules are imported after we establish canonical runtime
    // constructors (Object, Array, String, console). The std code mutates
    // `.prototype` on those constructors, so we must ensure the host/runtime
    // provides a usable prototype object before evaluating std modules.

    // Note: we imported the runtime helpers above. For clarity expose local
    // aliases used by the rest of the prelude.
    const host = host_ns;
    const debugValue = host_debugValue;
    // Provide a short alias expected by some bootstrap helpers.
    const toString = runtimeToString;

    // Lazy resolver that caches the host/native print function to avoid
    // performing host-bridge lookups on every console call. It first tries
    // to use globally published helpers (getHostGetter/getBuiltinCached) which
    // `soft/host.ts` exposes during bootstrap. If those are not available it
    // falls back to the imported `native_print` wrapper and finally to
    // `host_print` as a last resort.
    const getNativePrinter = (() => {
        let cached: any = undefined;
        let initialized = false;
        return function (): any {
            if (initialized) return cached;
            initialized = true;
            // Prefer the imported `native_print` wrapper which handles host lookup
            // safely. Avoid calling global helper functions here because some
            // bundling/initialization variants reference internal helpers as globals
            // which can be unavailable during certain bootstrap orders.
            try {
                if (typeof native_print === "function") {
                    cached = native_print;
                    return cached;
                }
            } catch (_) { }
            if (!cached) cached = host_print;
            return cached;
        };
    })();

    // Declare global variables
    // Capture runtime globals (console, Object, Array, String) when available so
    // the bundler-generated TypeScript files can reference them without
    // relying on unsupported `declare` statements.
    let console: any =
        typeof globalThis !== "undefined" && (globalThis as any).console
            ? (globalThis as any).console
            : { log: function (..._args: any[]) { } };
    let ObjectImplGlobal: any =
        typeof globalThis !== "undefined" && (globalThis as any).Object ? (globalThis as any).Object : {};
    let ArrayImplGlobal: any =
        typeof globalThis !== "undefined" && (globalThis as any).Array ? (globalThis as any).Array : {};
    let StringImplGlobal: any =
        typeof globalThis !== "undefined" && (globalThis as any).String ? (globalThis as any).String : {};

    // Bootstrap global console with basic implementation
    const globalConsole: Console = {
        log: function (...args: Value[]): void {
            let out = "";
            let sep = "";
            let i = 0;
            while (i < (args as any).length) {
                const a = (args as any)[i];
                let s: string;
                if (a === null) s = "null";
                else if (typeof a === "undefined") s = "undefined";
                else if (typeof a === "string") s = a;
                else if (typeof a === "number") s = runtimeToString(a as any);
                else if (typeof a === "boolean") s = a ? "true" : "false";
                else s = "[object Object]";
                out = out + sep + s;
                sep = " ";
                i = i + 1;
            }
            native_print(out);
        },
        warn: function (...args: Value[]): void {
            let out = "";
            let sep = "";
            let i = 0;
            while (i < (args as any).length) {
                const a = (args as any)[i];
                let s: string;
                if (a === null) s = "null";
                else if (typeof a === "undefined") s = "undefined";
                else if (typeof a === "string") s = a;
                else if (typeof a === "number") s = runtimeToString(a as any);
                else if (typeof a === "boolean") s = a ? "true" : "false";
                else s = "[object Object]";
                out = out + sep + s;
                sep = " ";
                i = i + 1;
            }
            native_print(out);
        },
        error: function (...args: Value[]): void {
            let out = "";
            let sep = "";
            let i = 0;
            while (i < (args as any).length) {
                const a = (args as any)[i];
                let s: string;
                if (a === null) s = "null";
                else if (typeof a === "undefined") s = "undefined";
                else if (typeof a === "string") s = a;
                else if (typeof a === "number") s = runtimeToString(a as any);
                else if (typeof a === "boolean") s = a ? "true" : "false";
                else s = "[object Object]";
                out = out + sep + s;
                sep = " ";
                i = i + 1;
            }
            native_print(out);
        },
        info: function (...args: Value[]): void {
            let out = "";
            let sep = "";
            let i = 0;
            while (i < (args as any).length) {
                const a = (args as any)[i];
                let s: string;
                if (a === null) s = "null";
                else if (typeof a === "undefined") s = "undefined";
                else if (typeof a === "string") s = a;
                else if (typeof a === "number") s = runtimeToString(a as any);
                else if (typeof a === "boolean") s = a ? "true" : "false";
                else s = "[object Object]";
                out = out + sep + s;
                sep = " ";
                i = i + 1;
            }
            native_print(out);
        },
        debug: function (...args: Value[]): void {
            let out = "";
            let sep = "";
            let i = 0;
            while (i < (args as any).length) {
                const a = (args as any)[i];
                let s: string;
                if (a === null) s = "null";
                else if (typeof a === "undefined") s = "undefined";
                else if (typeof a === "string") s = a;
                else if (typeof a === "number") s = runtimeToString(a as any);
                else if (typeof a === "boolean") s = a ? "true" : "false";
                else s = "[object Object]";
                out = out + sep + s;
                sep = " ";
                i = i + 1;
            }
            native_print(out);
        },
    };

    // Build and assign `console` with a single stable assignment to avoid creating
    // multiple intermediate hidden classes during bootstrap. Construct `console`
    // privately, set its properties in a stable order, then publish it to
    // `globalThis` in one write. If a host-provided console already exists, prefer
    // it but still copy methods in a stable way.
    try {
        const builtConsole: any = {};
        // Set properties in a fixed order to stabilize hidden classes.
        // For `log` we provide a small wrapper that resolves the native printer
        // once and calls it. This avoids a property lookup + host-bridge lookup
        // at every `console.log` call site.
        // Use the original `globalConsole` implementation as the default printer.
        // This avoids invoking host accessor helpers during prelude initialization
        // which can fail in some bootstrap orders. `globalConsole.log` is a safe
        // JS function that formats arguments and calls into the host through the
        // runtime wrapper; it's wrapped in try/catch to avoid crashing the runtime
        // if the host environment behaves unexpectedly.
        builtConsole.log = function (...args: Value[]): void {
            // Minimal Node-like console.log formatting: support %s, %d/%i, %j and %%.
            const inspect = function (v: any): string {
                try {
                    if (v === null) return "null";
                    if (typeof v === "undefined") return "undefined";
                    if (typeof v === "string") return v;
                    if (typeof v === "number") return toString(v as any);
                    if (typeof v === "boolean") return v ? "true" : "false";
                    try {
                        return JSON.stringify(v);
                    } catch (_) {
                        return toString(v as any);
                    }
                } catch (_) {
                    return "[object]";
                }
            };

            try {
                let out = "";
                if ((args as any).length > 0 && typeof (args as any)[0] === "string") {
                    const fmt: string = (args as any)[0];
                    let i = 1;
                    let j = 0;
                    while (j < fmt.length) {
                        const ch = fmt[j];
                        if (ch === "%" && j + 1 < fmt.length) {
                            const spec = fmt[j + 1];
                            if (spec === "%") {
                                out += "%";
                                j += 2;
                                continue;
                            }
                            const arg = (args as any)[i];
                            let rep = "";
                            if (typeof arg === "undefined") {
                                rep = "undefined";
                            } else if (spec === "s") {
                                rep = toString(arg as any);
                                i += 1;
                            } else if (spec === "d" || spec === "i") {
                                rep = Number(arg).toString();
                                i += 1;
                            } else if (spec === "j") {
                                try {
                                    rep = JSON.stringify(arg);
                                } catch (_) {
                                    rep = "[Circular]";
                                }
                                i += 1;
                            } else {
                                // unknown spec -> emit literally
                                out += "%" + spec;
                                j += 2;
                                continue;
                            }
                            out += rep;
                            j += 2;
                        } else {
                            out += ch;
                            j += 1;
                        }
                    }
                    // append remaining args
                    while (i < (args as any).length) {
                        out += (out === "" ? "" : " ") + inspect((args as any)[i]);
                        i += 1;
                    }
                } else {
                    // join inspected args with spaces
                    let sep = "";
                    let kk = 0;
                    while (kk < (args as any).length) {
                        out += sep + inspect((args as any)[kk]);
                        sep = " ";
                        kk += 1;
                    }
                }

                const printer = getNativePrinter();
                try {
                    printer(out);
                } catch (_) {
                    try {
                        host_print(out);
                    } catch (_) { }
                }
            } catch (_) { }
        };
        builtConsole.info = globalConsole.info;
        builtConsole.warn = globalConsole.warn;
        builtConsole.error = globalConsole.error;
        builtConsole.debug = globalConsole.debug;

        if (typeof globalThis !== "undefined") {
            const g: any = globalThis as any;
            try {
                // If host did not provide a console, publish our built console.
                if (!g.console) {
                    try {
                        g.console = builtConsole;
                    } catch (_) { }
                } else {
                    // If host provided one, ensure we use a stable set of methods.
                    // Copy host methods into a fixed-shape wrapper to avoid polymorphic
                    // shapes at call sites that expect `console` to have certain keys.
                    const hostConsole = g.console;
                    const useConsole: any = {};
                    useConsole.log = hostConsole.log
                        ? function (...args: any[]) {
                            try {
                                return hostConsole.log(...args);
                            } catch (_) { }
                            // Fallback: attempt to call the low-level native printer
                            // directly (in case hostConsole.log is flaky), otherwise
                            // call the built JS implementation as a last resort.
                            try {
                                const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
                                const direct: any = g
                                    ? g.__native_print ||
                                    g.__native_print_raw ||
                                    g.native_print ||
                                    (g.host && g.host.native_print)
                                    : undefined;
                                if (typeof direct === "function") {
                                    try {
                                        let out = "";
                                        let sep = "";
                                        let i = 0;
                                        while (i < (args as any).length) {
                                            out = out + sep + toString((args as any)[i]);
                                            sep = " ";
                                            i = i + 1;
                                        }
                                        direct(out);
                                        return;
                                    } catch (_) { }
                                }
                            } catch (_) { }
                            try {
                                return builtConsole.log(...args);
                            } catch (_) { }
                        }
                        : builtConsole.log;
                    useConsole.info = hostConsole.info
                        ? function (...args: any[]) {
                            try {
                                return hostConsole.info(...args);
                            } catch (_) { }
                            // Fallback: attempt to call the low-level native printer
                            try {
                                const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
                                const direct: any = g
                                    ? g.__native_print ||
                                    g.__native_print_raw ||
                                    g.native_print ||
                                    (g.host && g.host.native_print)
                                    : undefined;
                                if (typeof direct === "function") {
                                    try {
                                        let out = "";
                                        let sep = "";
                                        let i = 0;
                                        while (i < (args as any).length) {
                                            out = out + sep + toString((args as any)[i]);
                                            sep = " ";
                                            i = i + 1;
                                        }
                                        direct(out);
                                        return;
                                    } catch (_) { }
                                }
                            } catch (_) { }
                            try {
                                return builtConsole.info(...args);
                            } catch (_) { }
                        }
                        : builtConsole.info;
                    useConsole.warn = hostConsole.warn
                        ? function (...args: any[]) {
                            try {
                                return hostConsole.warn(...args);
                            } catch (_) { }
                            // Fallback: attempt to call the low-level native printer
                            try {
                                const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
                                const direct: any = g
                                    ? g.__native_print ||
                                    g.__native_print_raw ||
                                    g.native_print ||
                                    (g.host && g.host.native_print)
                                    : undefined;
                                if (typeof direct === "function") {
                                    try {
                                        let out = "";
                                        let sep = "";
                                        let i = 0;
                                        while (i < (args as any).length) {
                                            out = out + sep + toString((args as any)[i]);
                                            sep = " ";
                                            i = i + 1;
                                        }
                                        direct(out);
                                        return;
                                    } catch (_) { }
                                }
                            } catch (_) { }
                            try {
                                return builtConsole.warn(...args);
                            } catch (_) { }
                        }
                        : builtConsole.warn;
                    useConsole.error = hostConsole.error
                        ? function (...args: any[]) {
                            try {
                                return hostConsole.error(...args);
                            } catch (_) { }
                            // Fallback: attempt to call the low-level native printer
                            try {
                                const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
                                const direct: any = g
                                    ? g.__native_print ||
                                    g.__native_print_raw ||
                                    g.native_print ||
                                    (g.host && g.host.native_print)
                                    : undefined;
                                if (typeof direct === "function") {
                                    try {
                                        let out = "";
                                        let sep = "";
                                        let i = 0;
                                        while (i < (args as any).length) {
                                            out = out + sep + toString((args as any)[i]);
                                            sep = " ";
                                            i = i + 1;
                                        }
                                        direct(out);
                                        return;
                                    } catch (_) { }
                                }
                            } catch (_) { }
                            try {
                                return builtConsole.error(...args);
                            } catch (_) { }
                        }
                        : builtConsole.error;
                    useConsole.debug = hostConsole.debug
                        ? function (...args: any[]) {
                            try {
                                return hostConsole.debug(...args);
                            } catch (_) { }
                            // Fallback: attempt to call the low-level native printer
                            try {
                                const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
                                const direct: any = g
                                    ? g.__native_print ||
                                    g.__native_print_raw ||
                                    g.native_print ||
                                    (g.host && g.host.native_print)
                                    : undefined;
                                if (typeof direct === "function") {
                                    try {
                                        let out = "";
                                        let sep = "";
                                        let i = 0;
                                        while (i < (args as any).length) {
                                            out = out + sep + toString((args as any)[i]);
                                            sep = " ";
                                            i = i + 1;
                                        }
                                        direct(out);
                                        return;
                                    } catch (_) { }
                                }
                            } catch (_) { }
                            try {
                                return builtConsole.debug(...args);
                            } catch (_) { }
                        }
                        : builtConsole.debug;
                    try {
                        g.console = useConsole;
                    } catch (_) { }
                }
            } catch (_) {
                try {
                    g.console = builtConsole;
                } catch (_) { }
            }
            try {
                console = g.console ? g.console : builtConsole;
            } catch (_) {
                console = builtConsole;
            }
        } else {
            console = builtConsole;
        }
        try { host_print("PRELUDE: console assigned"); } catch (_) {}
    } catch (_) {
        try {
            console = globalConsole;
        } catch (_) { }
        // Debug: print the assigned console
        try {
            host_debugValue("global console assigned", console);
        } catch (_) { }

        // Basic Object implementation
        const ObjectImplInstance = {
            new(value?: Value): Value {
                if (value === null || value === undefined) {
                    return new ObjectImpl();
                }
                if (typeof value === "object") {
                    return value as Value;
                }
                // Would create wrapper objects for primitives
                throw new Error("Wrapper objects not implemented");
            },

            // Removed anonymous call-signature function (tech debt). Implement callable-object behavior
            // properly in the runtime if/when needed.

            keys(o: Value): string[] {
                return (o as BlitzObject).getOwnPropertyKeys();
            },

            values(o: Value): Value[] {
                const bo = o as BlitzObject;
                const keys = bo.getOwnPropertyKeys();
                const result: Value[] = [];
                let i = 0;
                while (i < keys.length) {
                    result.push(bo.get(keys[i]));
                    i = i + 1;
                }
                return result;
            },

            entries(o: Value): [string, Value][] {
                const bo = o as BlitzObject;
                const keys = bo.getOwnPropertyKeys();
                const result: [string, Value][] = [];
                let i = 0;
                while (i < keys.length) {
                    const key = keys[i];
                    result.push([key, bo.get(key)]);
                    i = i + 1;
                }
                return result;
            },

            assign(target: Value, ...sources: Value[]): Value {
                let si = 0;
                const t = target as BlitzObject;
                while (si < (sources as any).length) {
                    const source = (sources as any)[si] as BlitzObject;
                    const keys = source.getOwnPropertyKeys();
                    let ki = 0;
                    while (ki < keys.length) {
                        const key = keys[ki];
                        const value = source.get(key);
                        t.set(key, value);
                        ki = ki + 1;
                    }
                    si = si + 1;
                }
                return target;
            },

            create(o: Value | null): Value {
                return new ObjectImpl(o as any);
            },

            defineProperty(o: Value, p: string, attributes: Value): Value {
                let descriptor: any = {};
                if (attributes && typeof attributes === "object") {
                    if ("value" in (attributes as any)) descriptor.value = (attributes as any).value;
                    if ("writable" in (attributes as any)) descriptor.writable = (attributes as any).writable;
                    if ("enumerable" in (attributes as any))
                        descriptor.enumerable = (attributes as any).enumerable;
                    if ("configurable" in (attributes as any))
                        descriptor.configurable = (attributes as any).configurable;
                    if ("get" in (attributes as any)) descriptor.get = (attributes as any).get;
                    if ("set" in (attributes as any)) descriptor.set = (attributes as any).set;
                } else {
                    descriptor = {
                        value: attributes,
                        writable: true,
                        enumerable: true,
                        configurable: true,
                    };
                }
                try {
                    if (typeof (o as any).defineProperty === "function") {
                        (o as any).defineProperty(p, descriptor);
                        return o;
                    }
                } catch (_) { }
                try {
                    if (host_ns && typeof (host_ns as any).defineProperty === "function") {
                        (host_ns as any).defineProperty(o, p, descriptor);
                        return o;
                    }
                } catch (_) { }
                (o as BlitzObject).defineProperty(p, descriptor);
                return o;
            },

            defineProperties(o: Value, properties: Value): Value {
                if (properties && typeof properties === "object") {
                    const keys = (properties as any).getOwnPropertyKeys
                        ? (properties as any).getOwnPropertyKeys()
                        : ObjectImplGlobal.keys(properties as any);
                    let i = 0;
                    while (i < keys.length) {
                        const key = keys[i];
                        try {
                            const desc = (properties as any)[key];
                            (Object as any).defineProperty(o as any, key, desc);
                        } catch (_) { }
                        i = i + 1;
                    }
                }
                return o;
            },

            getOwnPropertyDescriptor(o: Value, p: string): Value {
                return (o as BlitzObject).getOwnProperty(p) || undefined;
            },

            getOwnPropertyDescriptors(o: Value): Value {
                const bo = o as BlitzObject;
                const result: Record<string, Value> = {};
                const keys = bo.getOwnPropertyKeys();
                let i = 0;
                while (i < keys.length) {
                    const key = keys[i];
                    const desc = bo.getOwnProperty(key);
                    if (desc) {
                        result[key] = desc;
                    }
                    i = i + 1;
                }
                return result;
            },

            getOwnPropertyNames(o: Value): string[] {
                return (o as BlitzObject).getOwnPropertyKeys();
            },

            getPrototypeOf(o: Value): Value | null {
                return (o as BlitzObject).getPrototypeOf();
            },

            setPrototypeOf(o: Value, proto: Value | null): Value {
                (o as BlitzObject).setPrototypeOf(proto);
                return o;
            },

            is(x: Value, y: Value): boolean {
                if (x === y) {
                    if (x !== 0) return true;
                    return 1 / (x as number) === 1 / (y as number);
                }
                return false;
            },

            freeze(o: Value): Value {
                (o as BlitzObject).preventExtensions();
                return o;
            },

            seal(o: Value): Value {
                (o as BlitzObject).preventExtensions();
                return o;
            },

            preventExtensions(o: Value): Value {
                (o as BlitzObject).preventExtensions();
                return o;
            },

            isFrozen(o: Value): boolean {
                return !(o as BlitzObject).isExtensible();
            },

            isSealed(o: Value): boolean {
                return !(o as BlitzObject).isExtensible();
            },

            isExtensible(o: Value): boolean {
                return (o as BlitzObject).isExtensible();
            },
        } as unknown as ObjectConstructor;

        // Construct `Object` constructor object privately and then assign it to
        // `globalThis` in a single stable write to avoid producing many transient
        // hidden classes during bootstrap.
        try {
            const builtObjectCtor: any = ObjectImplInstance;
            if (typeof globalThis !== "undefined") {
                const g: any = globalThis as any;
                try {
                    if (!g.Object) {
                        try {
                            g.Object = builtObjectCtor;
                        } catch (_) { }
                    }
                } catch (_) { }
                try {
                    ObjectImplGlobal = g.Object ? g.Object : builtObjectCtor;
                } catch (_) {
                    ObjectImplGlobal = builtObjectCtor;
                }
            } else {
                ObjectImplGlobal = builtObjectCtor;
            }
        } catch (_) {
            try {
                ObjectImplGlobal = ObjectImplInstance;
            } catch (_) { }
        }
        try { host_print("PRELUDE: Object published"); } catch (_) {}

        // Basic String constructor
        const StringImplInstance = {
            new(value?: Value): string {
                return runtimeToString(value);
            },

            fromCharCode(...codes: number[]): string {
                return String.fromCharCode(...codes);
            },

            fromCodePoint(...codePoints: number[]): string {
                return String.fromCodePoint(...codePoints);
            },

            raw(template: Value, ...substitutions: Value[]): string {
                // Basic implementation
                return String.raw(template as TemplateStringsArray, ...substitutions);
            },
        } as unknown as StringConstructor;

        // Construct `String` constructor and publish it with a single stable write.
        try {
            const builtStringCtor: any = StringImplInstance;
            if (typeof globalThis !== "undefined") {
                const g: any = globalThis as any;
                try {
                    if (!g.String) {
                        try {
                            g.String = builtStringCtor;
                        } catch (_) { }
                    }
                } catch (_) { }
                try {
                    StringImplGlobal = g.String ? g.String : builtStringCtor;
                } catch (_) {
                    StringImplGlobal = builtStringCtor;
                }
            } else {
                StringImplGlobal = builtStringCtor;
            }
        } catch (_) {
            try {
                StringImplGlobal = StringImplInstance;
            } catch (_) { }
        }
        try { host_print("PRELUDE: String published"); } catch (_) {}

        // Basic Array implementation
        const ArrayImplInstance = {
            new(...items: Value[]): Value[] {
                if (items.length === 1 && typeof items[0] === "number") {
                    const length = items[0];
                    const result: Value[] = new Array(length);
                    return result;
                } else {
                    return items.slice();
                }
            },

            isArray(arg: Value): boolean {
                // Basic implementation for tsblitz VM
                return Array.isArray(arg);
            },

            from<T>(arrayLike: Value): T[] {
                // Basic implementation
                return [];
            },

            of<T>(...items: T[]): T[] {
                return items.slice();
            },
        } as unknown as ArrayConstructor;

        // Construct `Array` constructor and publish it with a single stable write.
        try {
            const builtArrayCtor: any = ArrayImplInstance;
            if (typeof globalThis !== "undefined") {
                const g: any = globalThis as any;
                try {
                    if (!g.Array) {
                        try {
                            g.Array = builtArrayCtor;
                        } catch (_) { }
                    }
                } catch (_) { }
                try {
                    ArrayImplGlobal = g.Array ? g.Array : builtArrayCtor;
                } catch (_) {
                    ArrayImplGlobal = builtArrayCtor;
                }
            } else {
                ArrayImplGlobal = builtArrayCtor;
            }
        } catch (_) {
            try {
                ArrayImplGlobal = ArrayImplInstance;
            } catch (_) { }
        }
        try { host_print("PRELUDE: Array published"); } catch (_) {}

        // Ensure an Array.prototype object exists so std library can install methods
        // (the runtime's BlitzObject exposes `set`/`defineProperty` and prototype
        // behavior; we use `ObjectImpl` to create a fresh prototype object and attach
        // it to the constructor-like `Array` implementation).
        {
            // Create Array.prototype using the host-provided native object helper
            // when available. `native_new_object` returns a raw VM-backed object and
            // is safer here than constructing a soft-layer class instance because the
            // VM's internal object model expects a native object for prototype links.
            let protoObj: any;
            // Attempt to create a native-backed prototype object via host helper.
            // If the host helper is not available or returns a non-object, fall back
            // to a soft-layer object so bootstrap continues.
            try {
                debugValue("native_new_object before use", native_new_object);
            } catch (_) { }
            if (typeof native_new_object === "function") {
                // Call in try/catch to handle host helper errors.
                try {
                    protoObj = native_new_object();
                    // If the host helper returned null/undefined, fall back
                    if (protoObj === null || typeof protoObj === "undefined") {
                        protoObj = new ObjectImpl();
                    }
                } catch (_e) {
                    protoObj = new ObjectImpl();
                }
            } else {
                protoObj = new ObjectImpl();
            }
            // Set constructor on the prototype and attach the prototype to whichever
            // Array constructor we are using (runtime-provided or our soft one). Be
            // defensive: do not overwrite an existing prototype object if present.
            if (protoObj !== null && typeof protoObj !== "undefined") {
                try {
                    (protoObj as any)["constructor"] = (Array as any) || ArrayImplInstance;
                } catch (e) {
                    // Ignore failures here; host-provided objects may be immutable during
                    // certain bootstrap scenarios.
                }

                // Attach to the active Array constructor's prototype if it's missing.
                try {
                    // Attach the prototype to the constructor we published above. Prefer
                    // the constructor we assigned to `globalThis` to ensure a stable shape.
                    const hostCtor = typeof globalThis !== "undefined" ? (globalThis as any).Array : undefined;
                    const ctorToUse = hostCtor ? hostCtor : ArrayImplInstance;
                    if (
                        ctorToUse != null &&
                        (typeof ctorToUse === "object" || typeof ctorToUse === "function")
                    ) {
                        try {
                            if (ctorToUse.prototype == null) {
                                ctorToUse.prototype = protoObj;
                            }
                        } catch (_) {
                            // Ignore failures — host objects may be immutable during bootstrap.
                        }
                    }
                } catch (_) { }

                // Debug: verify that prototype was set on the active constructor
                try {
                    let readBack: any =
                        typeof globalThis !== "undefined" && (globalThis as any).Array
                            ? (globalThis as any).Array.prototype
                            : (ArrayImplInstance as any).prototype;
                    try {
                        debugValue("Array.prototype post-assign", readBack);
                        try { host_print("PRELUDE: Array.prototype attached"); } catch (_) {}
                    } catch (_) { }
                } catch (_) { }
            }
        }

        // Run any postponed std installers that were queued while std modules
        // initialized before the prelude completed. These installers expect the
        // runtime-provided constructors (Array, String, etc.) to exist and will
        // attach prototype methods. Run them now, best-effort.
        try {
            if (typeof globalThis !== "undefined") {
                const g: any = globalThis as any;
                const arr = g.__tsblitz_post_bootstrap;
                // Use a defensive array detection because `Array` may be null/host-provided
                // and calling `Array.isArray` could itself throw. Treat a value as an array
                // if it looks like an array (has numeric `length` and `push` function).
                const looksLikeArray =
                    arr != null &&
                    typeof arr === "object" &&
                    typeof (arr as any).length === "number" &&
                    typeof (arr as any).push === "function";
                if (looksLikeArray) {
                    const remaining: any[] = [];
                    let i = 0;
                    while (i < arr.length) {
                        try {
                            const entry = arr[i];
                            // Support both legacy function installers and the new descriptor
                            // objects pushed by std modules.
                            const installer = typeof entry === "function" ? entry : entry && entry.fn;
                            const targetName = entry && entry.target ? entry.target : "Array";
                            if (typeof installer === "function") {
                                try {
                                    const hostCtor = g && g[targetName];
                                    const usableHostCtor =
                                        hostCtor != null &&
                                        (typeof hostCtor === "object" || typeof hostCtor === "function");
                                    // If host ctor is usable, pass it; otherwise pass undefined and
                                    // allow the installer or prelude fallback to handle it.
                                    const ok = installer(usableHostCtor ? hostCtor : undefined);
                                    // If installer explicitly returned false, keep it for later
                                    if (ok === false) remaining.push(entry);
                                } catch (_) {
                                    // If installer threw, keep it for a single retry later.
                                    remaining.push(entry);
                                }
                            }
                        } catch (_) {
                            // Swallow and continue
                        }
                        i = i + 1;
                    }
                    try {
                        g.__tsblitz_post_bootstrap = remaining;
                    } catch (_) { }
                }
            }
        } catch (_) { }

        // Make BlitzObject-backed adapters the canonical host wrappers.
        // Provide a lazy host->Blitz adapter so host/native objects can be used
        // transparently by soft code that expects the BlitzObject API.
        const __tsblitz_host_wrapper_map = typeof WeakMap !== "undefined" ? new WeakMap<any, any>() : undefined;

        function ensureBlitzObject(v: any): any {
            if (v === null || typeof v !== "object") return v;
            try {
                // already looks like a BlitzObject
                if (typeof (v as any).getOwnPropertyKeys === "function") return v;
            } catch (_) { }
            if (!__tsblitz_host_wrapper_map) return v;
            let cached = __tsblitz_host_wrapper_map.get(v);
            if (cached) return cached;

            // Create a thin adapter implementing the minimal BlitzObject surface
            const adapter: any = {
                getOwnProperty(key: string) {
                    try {
                        const d = Object.getOwnPropertyDescriptor(v, key);
                        if (!d) return undefined;
                        // normalize to runtime descriptor shape
                        const rd: any = {};
                        if ("value" in d) rd.value = d.value;
                        if (d.get) rd.get = d.get;
                        if (d.set) rd.set = d.set;
                        if ("writable" in d) rd.writable = !!d.writable;
                        if ("enumerable" in d) rd.enumerable = !!d.enumerable;
                        if ("configurable" in d) rd.configurable = !!d.configurable;
                        return rd;
                    } catch (_) {
                        return undefined;
                    }
                },

                get(key: string) {
                    try {
                        return (v as any)[key];
                    } catch (_) {
                        return undefined;
                    }
                },

                set(key: string, value: any) {
                    try {
                        try {
                            (v as any)[key] = value;
                            return true;
                        } catch (_) {
                            Object.defineProperty(v, key, { value: value, writable: true, enumerable: true, configurable: true });
                            return true;
                        }
                    } catch (_) {
                        return false;
                    }
                },

                hasProperty(key: string) {
                    try {
                        return key in v;
                    } catch (_) {
                        return false;
                    }
                },

                deleteProperty(key: string) {
                    try {
                        return delete (v as any)[key];
                    } catch (_) {
                        return false;
                    }
                },

                getOwnPropertyKeys() {
                    try {
                        return Object.getOwnPropertyNames(v).concat(Object.getOwnPropertySymbols ? Object.getOwnPropertySymbols(v) as any : []);
                    } catch (_) {
                        return [];
                    }
                },

                getEnumerableKeys() {
                    try {
                        const res: string[] = [];
                        for (const k in v) res.push(k);
                        return res;
                    } catch (_) {
                        return [];
                    }
                },

                getPrototypeOf() {
                    try {
                        return Object.getPrototypeOf(v);
                    } catch (_) {
                        return null;
                    }
                },

                setPrototypeOf(proto: any) {
                    try {
                        Object.setPrototypeOf(v, proto);
                        return true;
                    } catch (_) {
                        return false;
                    }
                },

                isExtensible() {
                    try {
                        return Object.isExtensible(v);
                    } catch (_) {
                        return false;
                    }
                },

                preventExtensions() {
                    try {
                        Object.preventExtensions(v);
                    } catch (_) { }
                },

                defineProperty(key: string, desc: any) {
                    try {
                        Object.defineProperty(v, key, desc as any);
                    } catch (_) { }
                },
            };

            __tsblitz_host_wrapper_map.set(v, adapter);
            return adapter;
        }

        // Expose the wrapper globally so other bootstrap code can reuse it.
        try {
            if (typeof globalThis !== "undefined") (globalThis as any).__tsblitz_ensure_blitz = ensureBlitzObject;
            try { host_print("PRELUDE: __tsblitz_ensure_blitz set"); } catch (_) {}
        } catch (e) {
            try {
                // Print error and stack to the host debug printer if available.
                const msg = typeof e === "object" && e !== null && (e as any).stack ? (e as any).stack : String(e);
                try { host_print("Prelude failed: " + msg); } catch (_) { }
                try { host_debugValue("Prelude error", e); } catch (_) { }
            } catch (_) { }
            // Re-throw to allow the VM to handle the failure as before.
            throw e;
        }
    }
} catch (e) {
        try {
            // Print error and stack to the host debug printer if available.
            const msg = typeof e === "object" && e !== null && (e as any).stack ? (e as any).stack : String(e);
            try { host_print("Prelude failed: " + msg); } catch (_) { }
            try { host_debugValue("Prelude error", e); } catch (_) { }
        } catch (_) { }
    // Re-throw to allow the VM to handle the failure as before.
    throw e;
}
