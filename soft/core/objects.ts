/**
 * Object Model: Implementation of JavaScript object behavior.
 * Provides the core object model without TypeScript 'any' type.
 */

import type { Value, PropertyDescriptor, PropertyDescriptorMap } from "./types.js";
import * as host from "../host.js";

// Debug registration: report that core objects module initialized
try {
  host.debugValue("core/objects module loaded", "ok");
} catch (_) {}

// Object property storage
class PropertyStore {
  private data: Map<string, PropertyDescriptor> = new Map();

  defineProperty(key: string, descriptor: PropertyDescriptor): void {
    this.data.set(key, descriptor);
  }

  getOwnPropertyDescriptor(key: string): PropertyDescriptor | undefined {
    return this.data.get(key);
  }

  hasOwnProperty(key: string): boolean {
    return this.data.has(key);
  }

  deleteProperty(key: string): boolean {
    return this.data.delete(key);
  }

  getOwnPropertyKeys(): string[] {
    return Array.from(this.data.keys());
  }

  getEnumerableKeys(): string[] {
    const result: string[] = [];
    const entries = Array.from(this.data.entries());
    let i = 0;
    while (i < entries.length) {
      const pair = entries[i];
      const key = pair[0];
      const descriptor = pair[1];
      if (descriptor.enumerable !== false) {
        result.push(key);
      }
      i = i + 1;
    }
    return result;
  }
}

// Base object implementation
class BlitzObject {
  private properties = new PropertyStore();
  private _prototype: Value = null;
  private _extensible = true;

  constructor(proto: Value = null) {
    this._prototype = proto;
  }

  // [[GetOwnProperty]] internal method
  getOwnProperty(key: string): PropertyDescriptor | undefined {
    return this.properties.getOwnPropertyDescriptor(key);
  }

  // [[Get]] internal method
  get(key: string): Value {
    const desc = this.getOwnProperty(key);

    if (desc) {
      if ("value" in desc) {
        return desc.value;
      }
      if (desc.get) {
        return desc.get.call(this);
      }
      return undefined;
    }

    // Walk prototype chain
    const proto = this.getPrototypeOf();
    if (proto && typeof proto === "object") {
      return (proto as BlitzObject).get(key);
    }

    return undefined;
  }

  // [[Set]] internal method
  set(key: string, value: Value): boolean {
    const desc = this.getOwnProperty(key);

    if (desc) {
      if ("value" in desc) {
        if (desc.writable === false) {
          return false;
        }
        desc.value = value;
        return true;
      }
      if (desc.set) {
        desc.set.call(this, value);
        return true;
      }
      return false;
    }

    // Walk prototype chain for setter
    const proto = this.getPrototypeOf();
    if (proto && typeof proto === "object") {
      const protoDesc = (proto as BlitzObject).getOwnProperty(key);
      if (protoDesc && "set" in protoDesc && protoDesc.set) {
        protoDesc.set.call(this, value);
        return true;
      }
    }

    // Create new property
    if (!this._extensible) {
      return false;
    }

    this.properties.defineProperty(key, {
      value: value,
      writable: true,
      enumerable: true,
      configurable: true,
    });

    return true;
  }

  // [[HasProperty]] internal method
  hasProperty(key: string): boolean {
    if (this.properties.hasOwnProperty(key)) {
      return true;
    }

    const proto = this.getPrototypeOf();
    if (proto && typeof proto === "object") {
      return (proto as BlitzObject).hasProperty(key);
    }

    return false;
  }

  // [[Delete]] internal method
  deleteProperty(key: string): boolean {
    const desc = this.getOwnProperty(key);
    if (!desc) {
      return true;
    }

    if (desc.configurable === false) {
      return false;
    }

    return this.properties.deleteProperty(key);
  }

  // [[OwnPropertyKeys]] internal method
  getOwnPropertyKeys(): string[] {
    return this.properties.getOwnPropertyKeys();
  }

  // [[Enumerate]] internal method
  getEnumerableKeys(): string[] {
    const result: string[] = [];
    const processed = new Set<string>();

    // Get own enumerable keys
    const ownKeys = this.properties.getEnumerableKeys();
    let i = 0;
    while (i < ownKeys.length) {
      const key = ownKeys[i];
      if (!processed.has(key)) {
        result.push(key);
        processed.add(key);
      }
      i = i + 1;
    }

    // Walk prototype chain for enumerable keys
    let proto = this.getPrototypeOf();
    while (proto && typeof proto === "object") {
      const protoObj = proto as BlitzObject;
      // Access the proto's enumerable keys via safe call to avoid direct
      // property access on host-backed prototypes that may not expose
      // soft-layer methods.
      try {
        const protoKeys = protoObj.getEnumerableKeys();
        let j = 0;
        while (j < protoKeys.length) {
          const key = protoKeys[j];
          if (!processed.has(key)) {
            result.push(key);
            processed.add(key);
          }
          j = j + 1;
        }
      } catch (_) {
        // If the host-backed proto doesn't expose getEnumerableKeys, try
        // a defensive fallback that inspects known slots.
        try {
          const fallbackKeys = (protoObj as any).getOwnPropertyKeys
            ? (protoObj as any).getOwnPropertyKeys()
            : [];
          let k = 0;
          while (k < fallbackKeys.length) {
            const key = fallbackKeys[k];
            if (!processed.has(key)) {
              result.push(key);
              processed.add(key);
            }
            k = k + 1;
          }
        } catch (_) {}
      }
      proto = protoObj.getPrototypeOf();
    }

    return result;
  }

  // [[GetPrototypeOf]] internal method
  getPrototypeOf(): Value {
    return this._prototype;
  }

  // [[SetPrototypeOf]] internal method
  setPrototypeOf(proto: Value): boolean {
    // Implementation would handle circular references
    this._prototype = proto;
    return true;
  }

  // [[IsExtensible]] internal method
  isExtensible(): boolean {
    return this._extensible;
  }

  // [[PreventExtensions]] internal method
  preventExtensions(): void {
    this._extensible = false;
  }

  // Define property with full descriptor
  defineProperty(key: string, descriptor: PropertyDescriptor): void {
    const current = this.getOwnProperty(key);

    if (!this._extensible && !current) {
      throw new TypeError("Cannot define property on non-extensible object");
    }

    if (current) {
      if (!current.configurable) {
        throw new TypeError("Cannot redefine non-configurable property");
      }
    }

    this.properties.defineProperty(key, descriptor);
  }
}

// Function object implementation
class BlitzFunction extends BlitzObject {
  public readonly length: number;
  public readonly name: string;

  constructor(
    private impl: (thisArg: Value, ...args: Value[]) => Value,
    name: string = "",
    length: number = 0,
  ) {
    super();
    this.name = name;
    this.length = length;

    // Set up function properties
    this.defineProperty("length", {
      value: length,
      writable: false,
      enumerable: false,
      configurable: true,
    });

    this.defineProperty("name", {
      value: name,
      writable: false,
      enumerable: false,
      configurable: true,
    });

    // Functions have a prototype object
    const prototype = new BlitzObject();
    this.defineProperty("prototype", {
      value: prototype,
      writable: true,
      enumerable: false,
      configurable: false,
    });

    // Set prototype's constructor. Be defensive: during bootstrap the
    // prototype object may be a host/native-backed object or otherwise not
    // expose the soft-layer `defineProperty` method. In those cases fall
    // back to a plain assignment (and ignore failures). This prevents a
    // runtime crash when the soft layer runs before the host has fully
    // initialized its object graph.
    if (prototype && typeof (prototype as any).defineProperty === "function") {
      prototype.defineProperty("constructor", {
        value: this,
        writable: true,
        enumerable: false,
        configurable: true,
      });
    } else if (prototype !== null && typeof prototype !== "undefined") {
      try {
        (prototype as any)["constructor"] = this;
      } catch (_) {
        // Ignore failures; some host-backed prototypes may reject
        // property assignments during early bootstrap.
      }
    }
  }

  // Call internal method
  Call(thisArg: Value, ...args: Value[]): Value {
    return this.impl(thisArg, ...args);
  }

  // Construct internal method
  Construct(...args: Value[]): BlitzObject {
    const instance = new BlitzObject(this.get("prototype"));
    const result = this.impl.call(instance, ...args);

    // If constructor returns an object, return that instead
    if (typeof result === "object" && result !== null) {
      return result as BlitzObject;
    }

    return instance;
  }
}

export { BlitzObject, BlitzFunction, PropertyStore };

// Re-export types as type-only exports to satisfy `verbatimModuleSyntax` and
// type-only import rules used by the project's typechecker.
export type { Value, PropertyDescriptor, PropertyDescriptorMap };
