/**
 * Standard library: Symbol primitive.
 * Written in TypeScript, runs on the Rust VM.
 */

declare var Symbol: SymbolConstructor;

interface SymbolConstructor {
  new (description?: string): symbol;
  (description?: string): symbol;
  for(key: string): symbol;
  keyFor(sym: symbol): string | undefined;
  readonly asyncIterator: symbol;
  readonly hasInstance: symbol;
  readonly isConcatSpreadable: symbol;
  readonly iterator: symbol;
  readonly match: symbol;
  readonly matchAll: symbol;
  readonly replace: symbol;
  readonly search: symbol;
  readonly species: symbol;
  readonly split: symbol;
  readonly toPrimitive: symbol;
  readonly toStringTag: symbol;
  readonly unscopables: symbol;
}

declare global {
  interface Symbol {
    readonly description: string | undefined;
    toString(): string;
    valueOf(): symbol;
    [Symbol.toStringTag]: string;
  }
}

const globalSymbolRegistry = new Map<string, symbol>();

function createSymbol(description?: string): symbol {
  if (typeof Symbol === "function" && Symbol.for) {
    return Symbol(description);
  }
  const id = Math.random().toString(36).substring(2) + Date.now().toString(36);
  const sym = Symbol(id);
  Object.defineProperty(sym, "description", {
    value: description,
    writable: true,
    enumerable: false,
    configurable: true
  });
  return sym;
}

const SymbolImpl: SymbolConstructor = function (description?: string): symbol {
  try {
    if ((this as any) && (this as any).__is_constructor_call) {
      return new Symbol(description);
    }
  } catch (_) {}
  return createSymbol(description);
} as any;

SymbolImpl.for = function (key: string): symbol {
  if (globalSymbolRegistry.has(key)) {
    return globalSymbolRegistry.get(key)!;
  }
  const sym = createSymbol(key);
  globalSymbolRegistry.set(key, sym);
  return sym;
};

SymbolImpl.keyFor = function (sym: symbol): string | undefined {
  for (const [key, value] of globalSymbolRegistry.entries()) {
    if (value === sym) return key;
  }
  return undefined;
};

SymbolImpl.asyncIterator = Symbol.asyncIterator || createSymbol("Symbol.asyncIterator");
SymbolImpl.hasInstance = Symbol.hasInstance || createSymbol("Symbol.hasInstance");
SymbolImpl.isConcatSpreadable = Symbol.isConcatSpreadable || createSymbol("Symbol.isConcatSpreadable");
SymbolImpl.iterator = Symbol.iterator || createSymbol("Symbol.iterator");
SymbolImpl.match = Symbol.match || createSymbol("Symbol.match");
SymbolImpl.matchAll = Symbol.matchAll || createSymbol("Symbol.matchAll");
SymbolImpl.replace = Symbol.replace || createSymbol("Symbol.replace");
SymbolImpl.search = Symbol.search || createSymbol("Symbol.search");
SymbolImpl.species = Symbol.species || createSymbol("Symbol.species");
SymbolImpl.split = Symbol.split || createSymbol("Symbol.split");
SymbolImpl.toPrimitive = Symbol.toPrimitive || createSymbol("Symbol.toPrimitive");
SymbolImpl.toStringTag = Symbol.toStringTag || createSymbol("Symbol.toStringTag");
SymbolImpl.unscopables = Symbol.unscopables || createSymbol("Symbol.unscopables");

if (typeof Symbol !== "undefined") {
  const prototype = Symbol.prototype;
  if (prototype) {
    if (!prototype.toString) {
      prototype.toString = function (): string {
        const desc = this.description;
        return desc !== undefined ? `Symbol(${desc})` : "Symbol()";
      };
    }
    if (!prototype.valueOf) {
      prototype.valueOf = function (): symbol {
        return this;
      };
    }
  }
}

(Symbol as any) = SymbolImpl;
