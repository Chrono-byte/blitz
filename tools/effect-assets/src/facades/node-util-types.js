const tag = value => {
  if (value === null || (typeof value !== "object" && typeof value !== "function")) return "";
  try {
    return Object.prototype.toString.call(value);
  } catch {
    return "";
  }
};

const hasBrand = (value, method) => {
  if (typeof method !== "function") return false;
  try {
    method.call(value);
    return true;
  } catch {
    return false;
  }
};

const getterHasBrand = (value, getter) => {
  if (typeof getter !== "function") return false;
  try {
    getter.call(value);
    return true;
  } catch {
    return false;
  }
};

const descriptorGetter = (prototype, name) => {
  try {
    const descriptor = Object.getOwnPropertyDescriptor(prototype, name);
    return descriptor === undefined ? undefined : descriptor.get;
  } catch {
    return undefined;
  }
};

const arrayBufferByteLength = typeof ArrayBuffer === "function"
  ? descriptorGetter(ArrayBuffer.prototype, "byteLength")
  : undefined;
const sharedArrayBufferByteLength = typeof SharedArrayBuffer === "function"
  ? descriptorGetter(SharedArrayBuffer.prototype, "byteLength")
  : undefined;
const dataViewByteLength = typeof DataView === "function"
  ? descriptorGetter(DataView.prototype, "byteLength")
  : undefined;
const typedArrayPrototype = typeof Uint8Array === "function"
  ? Object.getPrototypeOf(Uint8Array.prototype)
  : undefined;
const typedArrayValues = typedArrayPrototype === undefined ? undefined : typedArrayPrototype.values;

const isObjectLike = value => value !== null && (typeof value === "object" || typeof value === "function");
const isArrayBuffer = value => typeof ArrayBuffer === "function" && value instanceof ArrayBuffer;
const isSharedArrayBuffer = value => typeof SharedArrayBuffer === "function" && value instanceof SharedArrayBuffer;
const isAnyArrayBuffer = value => isArrayBuffer(value) || isSharedArrayBuffer(value);
const isDataView = value => typeof DataView === "function" && value instanceof DataView;
const isTypedArray = value => {
  if (typeof ArrayBuffer !== "function" || !isObjectLike(value) || isDataView(value)) return false;
  return hasBrand(value, typedArrayValues);
};
const isArrayBufferView = value => isDataView(value) || isTypedArray(value);

const typedArrayName = value => {
  if (!isTypedArray(value)) return "";
  try {
    return value.constructor.name;
  } catch {
    return "";
  }
};
const isTypedArrayOf = (value, name) => typedArrayName(value) === name;

const isDate = value => typeof Date === "function" && hasBrand(value, Date.prototype.getTime);
const isRegExp = value => typeof RegExp === "function" && hasBrand(value, RegExp.prototype.exec);
const isMap = value => typeof Map === "function" && hasBrand(value, Map.prototype.has);
const isSet = value => typeof Set === "function" && hasBrand(value, Set.prototype.has);
const isPromise = value => typeof Promise === "function" && hasBrand(value, Promise.prototype.then);
const isBoxed = (value, prototype) => prototype !== undefined && hasBrand(value, prototype.valueOf);

const isBooleanObject = value => isBoxed(value, typeof Boolean === "function" ? Boolean.prototype : undefined);
const isNumberObject = value => isBoxed(value, typeof Number === "function" ? Number.prototype : undefined);
const isStringObject = value => isBoxed(value, typeof String === "function" ? String.prototype : undefined);
const isBigIntObject = value => isBoxed(value, typeof BigInt === "function" ? BigInt.prototype : undefined);
const isSymbolObject = value => isBoxed(value, typeof Symbol === "function" ? Symbol.prototype : undefined);
const isBoxedPrimitive = value => isBooleanObject(value)
  || isNumberObject(value)
  || isStringObject(value)
  || isBigIntObject(value)
  || isSymbolObject(value);

const generatorFunctionPrototype = (() => {
  try {
    return Object.getPrototypeOf(function* () {});
  } catch {
    return undefined;
  }
})();
const generatorPrototype = (() => {
  try {
    return Object.getPrototypeOf((function* () {})());
  } catch {
    return undefined;
  }
})();
const generatorNext = generatorPrototype === undefined ? undefined : generatorPrototype.next;
const isGeneratorFunction = value => {
  if (generatorFunctionPrototype === undefined) return false;
  try {
    return Object.getPrototypeOf(value) === generatorFunctionPrototype;
  } catch {
    return false;
  }
};
const isGeneratorObject = value => hasBrand(value, generatorNext);

const isNativeError = value => {
  if (value === null || typeof value !== "object") return false;
  try {
    return value instanceof Error;
  } catch {
    return false;
  }
};

const unsupported = () => false;
const isArgumentsObject = unsupported;
const isAsyncFunction = unsupported;
const isCryptoKey = unsupported;
const isExternal = unsupported;
const isFloat16Array = unsupported;
const isKeyObject = unsupported;
const isModuleNamespaceObject = unsupported;
const isProxy = unsupported;
const isWeakMap = unsupported;
const isWeakSet = unsupported;

const isBigInt64Array = value => isTypedArrayOf(value, "BigInt64Array");
const isBigUint64Array = value => isTypedArrayOf(value, "BigUint64Array");
const isFloat32Array = value => isTypedArrayOf(value, "Float32Array");
const isFloat64Array = value => isTypedArrayOf(value, "Float64Array");
const isInt8Array = value => isTypedArrayOf(value, "Int8Array");
const isInt16Array = value => isTypedArrayOf(value, "Int16Array");
const isInt32Array = value => isTypedArrayOf(value, "Int32Array");
const isMapIterator = value => tag(value) === "[object Map Iterator]";
const isSetIterator = value => tag(value) === "[object Set Iterator]";
const isUint8Array = value => isTypedArrayOf(value, "Uint8Array");
const isUint8ClampedArray = value => isTypedArrayOf(value, "Uint8ClampedArray");
const isUint16Array = value => isTypedArrayOf(value, "Uint16Array");
const isUint32Array = value => isTypedArrayOf(value, "Uint32Array");

const types = {
  isAnyArrayBuffer,
  isArgumentsObject,
  isArrayBuffer,
  isArrayBufferView,
  isAsyncFunction,
  isBigInt64Array,
  isBigIntObject,
  isBigUint64Array,
  isBooleanObject,
  isBoxedPrimitive,
  isCryptoKey,
  isDataView,
  isDate,
  isExternal,
  isFloat16Array,
  isFloat32Array,
  isFloat64Array,
  isGeneratorFunction,
  isGeneratorObject,
  isInt8Array,
  isInt16Array,
  isInt32Array,
  isKeyObject,
  isMap,
  isMapIterator,
  isModuleNamespaceObject,
  isNativeError,
  isNumberObject,
  isPromise,
  isProxy,
  isRegExp,
  isSet,
  isSetIterator,
  isSharedArrayBuffer,
  isStringObject,
  isSymbolObject,
  isTypedArray,
  isUint8Array,
  isUint8ClampedArray,
  isUint16Array,
  isUint32Array,
  isWeakMap,
  isWeakSet,
};

export {
  isAnyArrayBuffer,
  isArgumentsObject,
  isArrayBuffer,
  isArrayBufferView,
  isAsyncFunction,
  isBigInt64Array,
  isBigIntObject,
  isBigUint64Array,
  isBooleanObject,
  isBoxedPrimitive,
  isCryptoKey,
  isDataView,
  isDate,
  isExternal,
  isFloat16Array,
  isFloat32Array,
  isFloat64Array,
  isGeneratorFunction,
  isGeneratorObject,
  isInt8Array,
  isInt16Array,
  isInt32Array,
  isKeyObject,
  isMap,
  isMapIterator,
  isModuleNamespaceObject,
  isNativeError,
  isNumberObject,
  isPromise,
  isProxy,
  isRegExp,
  isSet,
  isSetIterator,
  isSharedArrayBuffer,
  isStringObject,
  isSymbolObject,
  isTypedArray,
  isUint8Array,
  isUint8ClampedArray,
  isUint16Array,
  isUint32Array,
  isWeakMap,
  isWeakSet,
  types,
};
export { types as default };
