/**
 * Standard library: Array helpers.
 * Written in TypeScript, runs on the Rust VM.
 */

// Do not capture the `Array` constructor at module-init time. During
// bootstrap the runtime may not yet have provided a host-backed `Array`.
// Instead register installers that run once the runtime `Array` is ready.
import * as host from "../host.js";

type ArrayInstaller = (ctor: any) => boolean | void;

// Register an installer for Array prototype. If a usable runtime Array
// constructor is present the installer runs immediately. Otherwise the
// installer is queued on globalThis.__tsblitz_post_bootstrap and will be
// executed by the prelude with a safe constructor (host-provided when usable
// or the local fallback). Installers must be defensive and may return false
// to indicate they could not complete.
function ensureArrayInstaller(installer: ArrayInstaller): void {
  try {
    if (typeof globalThis !== "undefined") {
      const g: any = globalThis as any;
      const hostCtor = g && g.Array;
      const usableHostCtor =
        hostCtor != null && (typeof hostCtor === "object" || typeof hostCtor === "function");
      if (usableHostCtor) {
        // Ensure prototype exists before calling installer
        try {
          if (hostCtor.prototype == null) {
            const p = host.native_new_object ? host.native_new_object() : undefined;
            hostCtor.prototype = p == null ? {} : p;
          }
        } catch (_) {
          try {
            if (hostCtor.prototype == null) hostCtor.prototype = {};
          } catch (_) {}
        }
        try {
          // Call installer with the host constructor
          installer(hostCtor);
        } catch (_) {}
        return;
      }

      // Not usable now — queue installer; prelude will call installers with
      // a safe constructor (host when available or the soft fallback).
      try {
        const existing = g.__tsblitz_post_bootstrap;
        const looksLikeArray =
          existing != null &&
          typeof existing === "object" &&
          typeof (existing as any).length === "number" &&
          typeof (existing as any).push === "function";
        if (!looksLikeArray) g.__tsblitz_post_bootstrap = [];
        // Store a simple descriptor so prelude can distinguish legacy function
        // entries and richer installer descriptors in future.
        g.__tsblitz_post_bootstrap.push({ target: "Array", fn: installer });
      } catch (_) {}
    }
  } catch (_) {}
}

// Ensure Array.prototype exists and is an object before mutating it.
// This is defensive because during bootstrap the runtime may provide a
// host-backed `Array` constructor whose `.prototype` is null/undefined or
// not yet initialized. Attempt to create a native-backed prototype via the
// host helper; fall back to a plain JS object so the std library can attach
// methods without causing SetProperty on null errors.
try {
  try {
    if (typeof globalThis !== "undefined") {
      const g: any = globalThis as any;
      const ctor: any =
        g && g.Array ? g.Array : typeof Array !== "undefined" ? (Array as any) : undefined;
      if (ctor) {
        let proto = ctor.prototype;
        if (proto == null) {
          // Try to obtain a native-backed object from the host helper.
          try {
            proto = host.native_new_object();
          } catch (_) {
            proto = undefined;
          }
          if (proto == null) proto = {};
          try {
            ctor.prototype = proto;
          } catch (_) {
            // If assignment fails, best-effort only — std will skip if prototype
            // is still not usable.
          }
        }
      }
    }
  } catch (_) {}
} catch (_) {}

// Diagnostic trace to help VM-side debugging: print types of Array and its prototype.
// Avoid caching `host.native_print` at module-init time because initialization
// order may make the cached value undefined. Resolve it at call time instead.
// Diagnostic removed: avoid calling host helpers during early bootstrap
// to prevent invocation of uninitialized host bindings.

// Extend Array prototype with helper methods
declare global {
  interface Array<T> {
    map<U>(callbackfn: (value: T, index: number, array: T[]) => U, thisArg?: any): U[];
    filter<S extends T>(
      predicate: (value: T, index: number, array: T[]) => value is S,
      thisArg?: any,
    ): S[];
    filter(predicate: (value: T, index: number, array: T[]) => unknown, thisArg?: any): T[];
    reduce<U>(
      callbackfn: (previousValue: U, currentValue: T, currentIndex: number, array: T[]) => U,
      initialValue: U,
    ): U;
    forEach(callbackfn: (value: T, index: number, array: T[]) => void, thisArg?: any): void;
    find<S extends T>(
      predicate: (value: T, index: number, obj: T[]) => value is S,
      thisArg?: any,
    ): S | undefined;
    find(predicate: (value: T, index: number, obj: T[]) => unknown, thisArg?: any): T | undefined;
    includes(searchElement: T, fromIndex?: number): boolean;
    indexOf(searchElement: T, fromIndex?: number): number;
    push(...items: T[]): number;
    pop(): T | undefined;
    shift(): T | undefined;
    unshift(...items: T[]): number;
    slice(start?: number, end?: number): T[];
    splice(start: number, deleteCount?: number, ...items: T[]): T[];
    join(separator?: string): string;
    reverse(): T[];
    sort(compareFn?: (a: T, b: T) => number): T[];
    length: number;
  }
}

// Basic Array prototype extensions
ensureArrayInstaller(function installMap(ctor: any) {
  try {
    if (!ctor) return false;
    if (ctor.prototype == null) {
      try {
        const p = host.native_new_object ? host.native_new_object() : undefined;
        ctor.prototype = p == null ? {} : p;
      } catch (_) {
        try {
          if (ctor.prototype == null) ctor.prototype = {};
        } catch (_) {
          return false;
        }
      }
    }
    if (!ctor.prototype.map) {
      ctor.prototype.map = function <U, T>(
        this: T[],
        callbackfn: (value: T, index: number, array: T[]) => U,
        thisArg?: any,
      ): U[] {
        const result: U[] = [];
        let i = 0;
        while (i < (this as any).length) {
          if (Object.prototype.hasOwnProperty.call(this, (i as any).toString())) {
            if (
              callbackfn &&
              (callbackfn as any).call &&
              typeof (callbackfn as any).call === "function"
            ) {
              result[i] = (callbackfn as any).call(thisArg, this[i], i, this);
            } else {
              result[i] = (callbackfn as any)(this[i], i, this);
            }
          }
          i = i + 1;
        }
        return result;
      };
    }
    return true;
  } catch (_) {
    return false;
  }
});

ensureArrayInstaller(function installFilter(ctor: any) {
  try {
    if (!ctor) return false;
    if (ctor.prototype == null) {
      try {
        const p = host.native_new_object ? host.native_new_object() : undefined;
        ctor.prototype = p == null ? {} : p;
      } catch (_) {
        try {
          if (ctor.prototype == null) ctor.prototype = {};
        } catch (_) {
          return false;
        }
      }
    }
    if (!ctor.prototype.filter) {
      ctor.prototype.filter = function <T, S extends T>(
        this: T[],
        predicate: (value: T, index: number, array: T[]) => unknown,
        thisArg?: any,
      ): T[] {
        const result: T[] = [];
        let i = 0;
        while (i < (this as any).length) {
          if (Object.prototype.hasOwnProperty.call(this, (i as any).toString())) {
            const value = this[i];
            let ok;
            if (
              predicate &&
              (predicate as any).call &&
              typeof (predicate as any).call === "function"
            ) {
              ok = (predicate as any).call(thisArg, value, i, this);
            } else {
              ok = (predicate as any)(value, i, this);
            }
            if (ok) {
              result.push(value);
            }
          }
          i = i + 1;
        }
        return result;
      };
    }
    return true;
  } catch (_) {
    return false;
  }
});

ensureArrayInstaller(function installReduce(ctor: any) {
  try {
    if (!ctor) return false;
    if (ctor.prototype == null) {
      try {
        const p = host.native_new_object ? host.native_new_object() : undefined;
        ctor.prototype = p == null ? {} : p;
      } catch (_) {
        try {
          if (ctor.prototype == null) ctor.prototype = {};
        } catch (_) {
          return false;
        }
      }
    }
    if (!ctor.prototype.reduce) {
      ctor.prototype.reduce = function <U, T>(
        this: T[],
        callbackfn: (previousValue: U, currentValue: T, currentIndex: number, array: T[]) => U,
        initialValue: U,
      ): U {
        let accumulator = initialValue;
        let startIndex = 0;

        if (arguments.length < 2) {
          accumulator = this[0] as unknown as U;
          startIndex = 1;
        }

        let i = startIndex;
        while (i < (this as any).length) {
          if (Object.prototype.hasOwnProperty.call(this, (i as any).toString())) {
            accumulator = callbackfn(accumulator, this[i], i, this);
          }
          i = i + 1;
        }

        return accumulator;
      };
    }
    return true;
  } catch (_) {
    return false;
  }
});

ensureArrayInstaller(function installForEach(ctor: any) {
  try {
    if (!ctor) return false;
    if (ctor.prototype == null) {
      try {
        const p = host.native_new_object ? host.native_new_object() : undefined;
        ctor.prototype = p == null ? {} : p;
      } catch (_) {
        try {
          if (ctor.prototype == null) ctor.prototype = {};
        } catch (_) {
          return false;
        }
      }
    }
    if (!ctor.prototype.forEach) {
      ctor.prototype.forEach = function <T>(
        this: T[],
        callbackfn: (value: T, index: number, array: T[]) => void,
        thisArg?: any,
      ): void {
        let i = 0;
        while (i < (this as any).length) {
          if (Object.prototype.hasOwnProperty.call(this, String(i))) {
            if (
              callbackfn &&
              (callbackfn as any).call &&
              typeof (callbackfn as any).call === "function"
            ) {
              (callbackfn as any).call(thisArg, this[i], i, this);
            } else {
              (callbackfn as any)(this[i], i, this);
            }
          }
          i = i + 1;
        }
      };
    }
    return true;
  } catch (_) {
    return false;
  }
});

ensureArrayInstaller(function installIncludes(ctor: any) {
  try {
    if (!ctor) return false;
    if (ctor.prototype == null) {
      try {
        const p = host.native_new_object ? host.native_new_object() : undefined;
        ctor.prototype = p == null ? {} : p;
      } catch (_) {
        try {
          if (ctor.prototype == null) ctor.prototype = {};
        } catch (_) {
          return false;
        }
      }
    }
    if (!ctor.prototype.includes) {
      ctor.prototype.includes = function <T>(
        this: T[],
        searchElement: T,
        fromIndex: number = 0,
      ): boolean {
        const start = fromIndex < 0 ? Math.max(this.length + fromIndex, 0) : fromIndex;
        let i = start;
        while (i < (this as any).length) {
          if (Object.prototype.hasOwnProperty.call(this, String(i)) && this[i] === searchElement) {
            return true;
          }
          i = i + 1;
        }
        return false;
      };
    }
    return true;
  } catch (_) {
    return false;
  }
});

ensureArrayInstaller(function installJoin(ctor: any) {
  try {
    if (!ctor) return false;
    if (ctor.prototype == null) {
      try {
        const p = host.native_new_object ? host.native_new_object() : undefined;
        ctor.prototype = p == null ? {} : p;
      } catch (_) {
        try {
          if (ctor.prototype == null) ctor.prototype = {};
        } catch (_) {
          return false;
        }
      }
    }
    if (!ctor.prototype.join) {
      ctor.prototype.join = function <T>(this: T[], separator?: string): string {
        const sep = separator === undefined ? "," : toString(separator as any);
        let out = "";
        let i = 0;
        while (i < (this as any).length) {
          if (i > 0) {
            out = out + sep;
          }
          const v = this[i];
          if (v === null || v === undefined) {
            out = out + "";
          } else {
            out = out + toString(v as any);
          }
          i = i + 1;
        }
        return out;
      };
    }
    return true;
  } catch (_) {
    return false;
  }
});

export {};

// Note: postponed installers are intentionally NOT run by the std module
// itself because std modules are evaluated before the prelude has a chance
// to establish runtime-provided constructors. The prelude is responsible for
// running any installers after it has set up canonical constructors and
// prototypes.
