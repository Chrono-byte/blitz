/**
 * Test262 harness shim.
 * Provides minimal builtins so harness helpers can load.
 */

(() => {
  try {
    const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : (this as any);

    // Early diagnostics: print the presence/types of critical symbols before
    // runtime_builtins applies polyfills. This helps determine ordering issues
    // when harness files (like atomicsHelper.js) run very early.
    try {
      const _p: any = g
        ? g.__native_print ||
          g.__native_print_raw ||
          g.native_print ||
          (g.host && g.host.native_print)
        : undefined;
      try {
        if (typeof _p === "function") {
          _p(
            "RB DIAG EARLY: Function.prototype.bind=" +
              typeof (Function.prototype && Function.prototype.bind),
          );
          _p("RB DIAG EARLY: typeof $262=" + typeof g.$262);
          _p("RB DIAG EARLY: typeof $262.agent=" + (g.$262 ? typeof g.$262.agent : "<missing>"));
          _p(
            "RB DIAG EARLY: typeof getReport=" +
              (g.$262 && g.$262.agent ? typeof g.$262.agent.getReport : "<missing>"),
          );
          _p("RB DIAG EARLY: typeof Atomics=" + typeof g.Atomics);
          _p(
            "RB DIAG EARLY: typeof Atomics.waitAsync=" +
              (g.Atomics ? typeof g.Atomics.waitAsync : "<missing>"),
          );
        }
      } catch (_) {}
    } catch (_) {}

    // Minimal polyfills required by harness helpers: Function.prototype.bind is
    // used by `harness/atomicsHelper.js` (it calls `$262.agent.getReport.bind(...)`).
    // If bind is missing, provide a small compliant polyfill.
    try {
      if (typeof Function !== "undefined" && typeof Function.prototype !== "undefined") {
        if (typeof Function.prototype.bind !== "function") {
          Function.prototype.bind = function (thisArg: any) {
            const fn = this;
            const boundArgs = Array.prototype.slice.call(arguments, 1);
            return function () {
              const args = Array.prototype.slice.call(arguments);
              return fn.apply(thisArg, boundArgs.concat(args));
            };
          } as any;
        }
      }
    } catch (_) {}

    const defineGlobal = (name: string, value: any) => {
      try {
        if (typeof g[name] === "undefined") {
          g[name] = value;
        }
      } catch (_) {}
    };

    // Basic Promise shim (synchronous microtasks).
    if (typeof g.Promise === "undefined") {
      const PromiseImpl: any = function (executor: any) {
        if (!(this instanceof PromiseImpl)) return new PromiseImpl(executor);
        this._fulfilled = false;
        this._rejected = false;
        this._value = undefined;
        this._reason = undefined;
        this._handlers = [] as any[];
        const resolve = (v: any) => {
          if (this._fulfilled || this._rejected) return;
          this._fulfilled = true;
          this._value = v;
          for (let i = 0; i < this._handlers.length; i++) {
            const h = this._handlers[i];
            if (h.onFulfilled) h.onFulfilled(this._value);
          }
        };
        const reject = (r: any) => {
          if (this._fulfilled || this._rejected) return;
          this._rejected = true;
          this._reason = r;
          for (let i = 0; i < this._handlers.length; i++) {
            const h = this._handlers[i];
            if (h.onRejected) h.onRejected(this._reason);
          }
        };
        try {
          executor(resolve, reject);
        } catch (e) {
          reject(e);
        }
      };
      const promiseThen = function (onFulfilled: any, onRejected: any) {
        const self = this;
        return new PromiseImpl((res: any, rej: any) => {
          const handler = {
            onFulfilled: (v: any) => {
              try {
                res(typeof onFulfilled === "function" ? onFulfilled(v) : v);
              } catch (e) {
                rej(e);
              }
            },
            onRejected: (r: any) => {
              try {
                if (typeof onRejected === "function") res(onRejected(r));
                else rej(r);
              } catch (e) {
                rej(e);
              }
            },
          };
          if (self._fulfilled) handler.onFulfilled(self._value);
          else if (self._rejected) handler.onRejected(self._reason);
          else self._handlers.push(handler);
        });
      };
      const thenProp = "then" as any;
      (PromiseImpl.prototype as any)[thenProp] = promiseThen;
      const promiseCatch = function (onRejected: any) {
        return (this as any)[thenProp](undefined, onRejected);
      };
      (PromiseImpl.prototype as any).catch = promiseCatch;
      const promiseFinally = function (onFinally: any) {
        return (this as any)[thenProp](
          (v: any) => {
            if (typeof onFinally === "function") onFinally();
            return v;
          },
          (r: any) => {
            if (typeof onFinally === "function") onFinally();
            throw r;
          },
        );
      };
      (PromiseImpl.prototype as any).finally = promiseFinally;
      PromiseImpl.resolve = function (v: any) {
        return new PromiseImpl((res: any) => res(v));
      };
      PromiseImpl.reject = function (r: any) {
        return new PromiseImpl((_res: any, rej: any) => rej(r));
      };
      PromiseImpl.all = function (values: any[]) {
        return new PromiseImpl((res: any, rej: any) => {
          const out: any[] = [];
          let pending = values.length;
          if (pending === 0) return res(out);
          for (let i = 0; i < values.length; i++) {
            const v = values[i];
            PromiseImpl.resolve(v).then(
              (val: any) => {
                out[i] = val;
                pending -= 1;
                if (pending === 0) res(out);
              },
              (e: any) => rej(e),
            );
          }
        });
      };
      PromiseImpl.race = function (values: any[]) {
        return new PromiseImpl((res: any, rej: any) => {
          for (let i = 0; i < values.length; i++) {
            PromiseImpl.resolve(values[i]).then(res, rej);
          }
        });
      };
      defineGlobal("Promise", PromiseImpl);
    }

    // Basic Date shim if missing.
    if (typeof g.Date === "undefined") {
      const DateImpl: any = function (...args: any[]) {
        if (!(this instanceof DateImpl)) return new DateImpl(...args);
        this._time = args.length === 0 ? DateImpl.now() : Number(args[0]);
      };
      DateImpl.now = function () {
        return 0;
      };
      DateImpl.parse = function (_s: string) {
        return 0;
      };
      DateImpl.UTC = function () {
        return 0;
      };
      DateImpl.prototype.toString = function () {
        return "Thu Jan 01 1970 00:00:00 GMT";
      };
      DateImpl.prototype.valueOf = function () {
        return this._time || 0;
      };
      defineGlobal("Date", DateImpl);
    }

    // Symbol shim (very small).
    if (typeof g.Symbol === "undefined") {
      let symId = 0;
      const SymbolImpl: any = function (desc?: any) {
        const id = ++symId;
        return {
          __isSymbol: true,
          description: desc === undefined ? undefined : String(desc),
          toString: function () {
            return desc !== undefined ? "Symbol(" + desc + ")" : "Symbol()";
          },
          valueOf: function () {
            return this;
          },
          _id: id,
        };
      };
      SymbolImpl.toPrimitive = SymbolImpl("Symbol.toPrimitive");
      try {
        (SymbolImpl as any)["toPrimitive"] = SymbolImpl.toPrimitive;
      } catch (_) {}
      SymbolImpl.iterator = SymbolImpl("Symbol.iterator");
      SymbolImpl.asyncIterator = SymbolImpl("Symbol.asyncIterator");
      SymbolImpl.match = SymbolImpl("Symbol.match");
      SymbolImpl.matchAll = SymbolImpl("Symbol.matchAll");
      SymbolImpl.replace = SymbolImpl("Symbol.replace");
      SymbolImpl.search = SymbolImpl("Symbol.search");
      SymbolImpl.split = SymbolImpl("Symbol.split");
      SymbolImpl.hasInstance = SymbolImpl("Symbol.hasInstance");
      SymbolImpl.isConcatSpreadable = SymbolImpl("Symbol.isConcatSpreadable");
      SymbolImpl.species = SymbolImpl("Symbol.species");
      SymbolImpl.toStringTag = SymbolImpl("Symbol.toStringTag");
      SymbolImpl.unscopables = SymbolImpl("Symbol.unscopables");
      defineGlobal("Symbol", SymbolImpl);
    }

    // BigInt shim
    if (typeof g.BigInt === "undefined") {
      defineGlobal("BigInt", function (v: any) {
        return Number(v);
      });
    }

    // ArrayBuffer / SharedArrayBuffer shims
    if (typeof g.ArrayBuffer === "undefined") {
      const ArrayBufferImpl: any = function (byteLength: number, opts?: any) {
        this.byteLength = Number(byteLength) || 0;
        this.maxByteLength =
          opts && typeof opts.maxByteLength === "number" ? opts.maxByteLength : this.byteLength;
        this._data = new Array(this.byteLength).fill(0);
      };
      ArrayBufferImpl.isView = function (v: any) {
        return v && v._isTypedArray;
      };
      ArrayBufferImpl.prototype.slice = function (begin?: number, end?: number) {
        const len = this.byteLength;
        const b = begin == null ? 0 : Math.max(0, begin);
        const e = end == null ? len : Math.min(len, end);
        const nb = new ArrayBufferImpl(e - b);
        for (let i = 0; i < e - b; i++) nb._data[i] = this._data[b + i];
        return nb;
      };
      ArrayBufferImpl.prototype.resize = function (newSize: number) {
        const n = Number(newSize) || 0;
        if (n > this.maxByteLength) return;
        const old = this._data;
        this._data = new Array(n).fill(0);
        for (let i = 0; i < Math.min(old.length, n); i++) this._data[i] = old[i];
        this.byteLength = n;
      };
      defineGlobal("ArrayBuffer", ArrayBufferImpl);
    }

    if (typeof g.SharedArrayBuffer === "undefined") {
      const SharedArrayBufferImpl: any = function (byteLength: number, opts?: any) {
        const buf: any = new (g.ArrayBuffer as any)(byteLength, opts);
        buf._isShared = true;
        return buf;
      };
      defineGlobal("SharedArrayBuffer", SharedArrayBufferImpl);
    }

    const makeTypedArray = (name: string, bytesPerElement: number) => {
      const TA: any = function (arg?: any) {
        if (!(this instanceof TA)) return new TA(arg);
        if (typeof arg === "number") {
          this.length = arg;
          this.buffer = new (g.ArrayBuffer as any)(arg * bytesPerElement);
        } else if (arg && arg.byteLength !== undefined) {
          this.buffer = arg;
          this.length = Math.floor((arg.byteLength || 0) / bytesPerElement);
        } else {
          this.length = 0;
          this.buffer = new (g.ArrayBuffer as any)(0);
        }
        this.byteLength = this.length * bytesPerElement;
        this.byteOffset = 0;
        this._isTypedArray = true;
        this.__data = this.buffer._data || new Array(this.length).fill(0);
      };
      TA.BYTES_PER_ELEMENT = bytesPerElement;
      TA.prototype = [];
      TA.prototype.constructor = TA;
      return TA;
    };

    const taDefs: Array<[string, number]> = [
      ["Int8Array", 1],
      ["Uint8Array", 1],
      ["Uint8ClampedArray", 1],
      ["Int16Array", 2],
      ["Uint16Array", 2],
      ["Int32Array", 4],
      ["Uint32Array", 4],
      ["Float32Array", 4],
      ["Float64Array", 8],
      ["BigInt64Array", 8],
      ["BigUint64Array", 8],
    ];
    taDefs.forEach(([n, b]) => {
      if (typeof g[n] === "undefined") defineGlobal(n, makeTypedArray(n, b));
    });

    if (typeof g.DataView === "undefined") {
      defineGlobal("DataView", function (buffer: any) {
        this.buffer = buffer;
        this.byteLength = buffer && buffer.byteLength ? buffer.byteLength : 0;
        this.byteOffset = 0;
      });
    }

    if (typeof g.Atomics === "undefined") {
      const _p: any = g
        ? g.__native_print ||
          g.__native_print_raw ||
          g.native_print ||
          (g.host && g.host.native_print)
        : undefined;
      try {
        if (typeof _p === "function") _p("runtime_builtins: creating AtomicsImpl");
      } catch (_) {}
      const AtomicsImpl: any = {
        add: function (ta: any, index: number, value: any) {
          const i = Number(index) || 0;
          const old = ta.__data[i] || 0;
          const next = old + Number(value);
          ta.__data[i] = next;
          return old;
        },
        // Additional Atomics operations used by the Test262 harness.
        sub: function (ta: any, index: number, value: any) {
          const i = Number(index) || 0;
          const old = ta.__data[i] || 0;
          const next = old - Number(value);
          ta.__data[i] = next;
          return old;
        },
        and: function (ta: any, index: number, value: any) {
          const i = Number(index) || 0;
          const old = ta.__data[i] || 0;
          const next = old & Number(value);
          ta.__data[i] = next;
          return old;
        },
        or: function (ta: any, index: number, value: any) {
          const i = Number(index) || 0;
          const old = ta.__data[i] || 0;
          const next = old | Number(value);
          ta.__data[i] = next;
          return old;
        },
        xor: function (ta: any, index: number, value: any) {
          const i = Number(index) || 0;
          const old = ta.__data[i] || 0;
          const next = old ^ Number(value);
          ta.__data[i] = next;
          return old;
        },
        exchange: function (ta: any, index: number, value: any) {
          const i = Number(index) || 0;
          const old = ta.__data[i] || 0;
          ta.__data[i] = value;
          return old;
        },
        compareExchange: function (ta: any, index: number, expected: any, replacement: any) {
          const i = Number(index) || 0;
          const old = ta.__data[i] || 0;
          if (old === expected) {
            ta.__data[i] = replacement;
          }
          return old;
        },
        load: function (ta: any, index: number) {
          const i = Number(index) || 0;
          return ta.__data[i] || 0;
        },
        store: function (ta: any, index: number, value: any) {
          const i = Number(index) || 0;
          ta.__data[i] = value;
          return value;
        },
        wait: function (_ta: any, _index: number, _value: any, _timeout?: any) {
          return "ok";
        },
        // Minimal waitAsync shim: return an object with a Promise `value` and log invocation
        waitAsync: function (_ta: any, _index: number, _value: any, _timeout?: any) {
          try {
            if (typeof _p === "function") _p("runtime_builtins: Atomics.waitAsync called");
          } catch (_) {}
          try {
            const P = (g && g.Promise) || Promise;
            return { value: P.resolve("ok") };
          } catch (_) {
            return { value: Promise.resolve("ok") };
          }
        },
        wake: function (_ta: any, _index: number, _count?: any) {
          return 0;
        },
      };
      try {
        if (typeof _p === "function") _p("runtime_builtins: defining Atomics global");
      } catch (_) {}
      defineGlobal("Atomics", AtomicsImpl);
    }

    if (typeof g.Reflect === "undefined") {
      defineGlobal("Reflect", {
        apply: function (fn: any, thisArg: any, args: any[]) {
          return fn.apply(thisArg, args || []);
        },
        get: function (obj: any, key: any) {
          return obj[key];
        },
        set: function (obj: any, key: any, value: any) {
          obj[key] = value;
          return true;
        },
        defineProperty: function (obj: any, key: any, desc: any) {
          try {
            if (typeof g.__define_property === "function") {
              return !!g.__define_property(obj, key, desc);
            }
          } catch (_) {}
          try {
            Object.defineProperty(obj, key, desc);
            return true;
          } catch (_) {
            return false;
          }
        },
      });
    }

    if (typeof g.Proxy === "undefined") {
      defineGlobal("Proxy", function (target: any, _handler: any) {
        return target;
      });
    }

    if (typeof g.Map === "undefined") {
      const MapImpl: any = function () {
        this._data = [] as any[];
      };
      MapImpl.prototype.set = function (k: any, v: any) {
        this._data.push([k, v]);
        return this;
      };
      MapImpl.prototype.get = function (k: any) {
        for (let i = 0; i < this._data.length; i++)
          if (this._data[i][0] === k) return this._data[i][1];
        return undefined;
      };
      MapImpl.prototype.has = function (k: any) {
        for (let i = 0; i < this._data.length; i++) if (this._data[i][0] === k) return true;
        return false;
      };
      defineGlobal("Map", MapImpl);
    }

    if (typeof g.Set === "undefined") {
      const SetImpl: any = function () {
        this._data = [] as any[];
      };
      SetImpl.prototype.add = function (v: any) {
        this._data.push(v);
        return this;
      };
      SetImpl.prototype.has = function (v: any) {
        return this._data.indexOf(v) >= 0;
      };
      defineGlobal("Set", SetImpl);
    }

    if (typeof g.WeakMap === "undefined") defineGlobal("WeakMap", g.Map);
    if (typeof g.WeakSet === "undefined") defineGlobal("WeakSet", g.Set);

    if (!g.$262) g.$262 = {};
    if (!g.$262.agent) g.$262.agent = {};
    const agent = g.$262.agent;
    try {
      const _p: any = g
        ? g.__native_print ||
          g.__native_print_raw ||
          g.native_print ||
          (g.host && g.host.native_print)
        : undefined;
      try {
        if (typeof _p === "function")
          _p("runtime_builtins: agent initialized, getReport type=" + typeof agent.getReport);
      } catch (_) {}
    } catch (_) {}
    if (!agent._reports) agent._reports = [];
    if (!agent.getReport) {
      agent.getReport = function () {
        return agent._reports.length ? agent._reports.shift() : null;
      };
    }
    if (!agent.report) {
      agent.report = function (msg: any) {
        agent._reports.push(String(msg));
      };
    }
    if (!agent.sleep) {
      agent.sleep = function (_ms: any) {};
    }
    if (!agent.broadcast) {
      agent.broadcast = function (_buf: any) {
        if (typeof agent._broadcastCb === "function") agent._broadcastCb(_buf);
        return g.Promise.resolve();
      };
    }
    // safeBroadcast: ensure the typed array can be used for Atomics.wait
    if (!agent.safeBroadcast) {
      agent.safeBroadcast = function (typedArray: any) {
        try {
          const Constructor = Object.getPrototypeOf(typedArray).constructor;
          // create a temp shared buffer to test Atomics.wait support
          const temp = new Constructor(new SharedArrayBuffer(Constructor.BYTES_PER_ELEMENT));
          try {
            Atomics.wait(
              temp,
              0,
              Constructor === Int32Array ? 1 : typeof BigInt !== "undefined" ? BigInt(1) : 1,
            );
          } catch (error) {
            throw new Error(
              `${Constructor.name} cannot be used as a shared typed array. (${error})`,
            );
          }
          return agent.broadcast(typedArray.buffer || typedArray);
        } catch (e) {
          // If anything fails, rethrow to match harness behavior
          throw e;
        }
      };
    }
    if (!agent.receiveBroadcast) {
      agent.receiveBroadcast = function (cb: any) {
        agent._broadcastCb = cb;
      };
    }
    if (!agent.start) {
      agent.start = function (_src: any) {};
    }
    if (!agent.monotonicNow) {
      agent.monotonicNow = function () {
        try {
          return Date.now();
        } catch (_) {
          return 0;
        }
      };
    }
    if (!agent.getReportAsync) {
      agent.getReportAsync = function () {
        try {
          const P = (g && g.Promise) || Promise;
          return new P(function (resolve) {
            (function loop() {
              try {
                const r = agent.getReport();
                if (!r) {
                  try {
                    (g && g.setTimeout ? g.setTimeout : setTimeout)(loop, 1000);
                  } catch (_) {
                    P.resolve().then(loop);
                  }
                } else {
                  resolve(r);
                }
              } catch (_) {
                try {
                  (g && g.setTimeout ? g.setTimeout : setTimeout)(loop, 1000);
                } catch (_) {
                  P.resolve().then(loop);
                }
              }
            })();
          });
        } catch (_) {
          return Promise.resolve(agent.getReport());
        }
      };
    }
    if (!agent.safeBroadcastAsync) {
      // Conform to harness: safeBroadcastAsync(ta, index, expected)
      agent.safeBroadcastAsync = async function (ta: any, index: any, expected: any) {
        try {
          await agent.broadcast(ta.buffer || ta);
        } catch (_) {}
        try {
          await agent.waitUntil(ta, index, expected);
        } catch (_) {}
        try {
          await agent.tryYield();
        } catch (_) {}
        try {
          return Atomics.load(ta, index);
        } catch (_) {
          return ta && ta.__data && typeof ta.__data[index] !== "undefined" ? ta.__data[index] : 0;
        }
      };
    }
    // waitUntil: block (busy-wait) until Atomics.load returns expected
    if (!agent.waitUntil) {
      agent.waitUntil = function (typedArray: any, index: number, expected: number) {
        while (true) {
          try {
            const v = Atomics.load(typedArray, index);
            if (v === expected) break;
          } catch (_) {
            try {
              if (typedArray && typedArray.__data && typedArray.__data[index] === expected) break;
            } catch (_) {}
          }
          // spin
        }
      };
    }
    if (!agent.leaving) {
      agent.leaving = function () {};
    }
    if (!agent.tryYield) {
      agent.tryYield = function () {
        agent.sleep(1);
      };
    }
    if (!agent.trySleep) {
      agent.trySleep = function (ms: any) {
        agent.sleep(ms);
      };
    }

    if (!g.$262.detachArrayBuffer) {
      g.$262.detachArrayBuffer = function (buffer: any) {
        if (buffer && typeof buffer === "object") {
          buffer.byteLength = 0;
          buffer._data = [];
          buffer._detached = true;
        }
      };
    }
  } catch (e) {
    try {
      const msg = e && e.stack ? e.stack : String(e);
      try {
        const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
        const p = g
          ? g.__native_print ||
            g.__native_print_raw ||
            g.native_print ||
            (g.host && g.host.native_print)
          : undefined;
        if (typeof p === "function") {
          try {
            p("runtime_builtins failed: " + msg);
          } catch (_) {}
        }
      } catch (_) {}
    } catch (_) {}
    throw e;
  }
})();
