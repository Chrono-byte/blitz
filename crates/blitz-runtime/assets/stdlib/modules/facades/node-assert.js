const objectToString = Object.prototype.toString;
const propertyIsEnumerable = Object.prototype.propertyIsEnumerable;

const isObjectLike = value => value !== null && (typeof value === "object" || typeof value === "function");
const isNaNValue = value => typeof value === "number" && value !== value;
const sameValue = (left, right) => left === right
  ? left !== 0 || 1 / left === 1 / right
  : isNaNValue(left) && isNaNValue(right);

const formatValue = (value, seen = []) => {
  if (typeof value === "string") return JSON.stringify(value);
  if (value === undefined) return "undefined";
  if (value === null) return "null";
  if (typeof value === "number" || typeof value === "boolean" || typeof value === "bigint") return String(value);
  if (typeof value === "symbol") return String(value);
  if (typeof value === "function") return `[Function${value.name === undefined ? "" : `: ${value.name}`}]`;
  if (seen.indexOf(value) !== -1) return "[Circular]";
  const nextSeen = seen.concat([value]);
  if (Array.isArray(value)) return `[${value.map(item => formatValue(item, nextSeen)).join(", ")}]`;
  const keys = Object.keys(value);
  if (keys.length === 0) return objectToString.call(value);
  return `{ ${keys.map(key => `${key}: ${formatValue(value[key], nextSeen)}`).join(", ")} }`;
};

const assertion = (message, actual, expected, operator) => {
  if (message instanceof Error) throw message;
  throw new AssertionError({
    actual,
    expected,
    operator,
    message: message === undefined ? undefined : String(message),
  });
};

class AssertionError extends Error {
  constructor(options) {
    if (options === null || typeof options !== "object") {
      const error = new TypeError('The "options" argument must be of type object');
      error.code = "ERR_INVALID_ARG_TYPE";
      throw error;
    }
    const generatedMessage = options.message === undefined;
    const message = generatedMessage
      ? `Assertion failed${options.operator === undefined ? "" : ` (${options.operator})`}`
      : String(options.message);
    super(message);
    this.name = "AssertionError";
    this.code = "ERR_ASSERTION";
    this.actual = options.actual;
    this.expected = options.expected;
    this.operator = options.operator === undefined ? "" : options.operator;
    this.generatedMessage = generatedMessage;
  }
}

const enumerableKeys = value => {
  const keys = Object.keys(value);
  if (typeof Object.getOwnPropertySymbols === "function") {
    const symbols = Object.getOwnPropertySymbols(value);
    for (const symbol of symbols) {
      if (propertyIsEnumerable.call(value, symbol)) keys.push(symbol);
    }
  }
  return keys;
};

const compareKey = (left, right) => left === right;
const hasKey = (keys, key) => keys.some(candidate => compareKey(candidate, key));

const compareArrayBufferBytes = (left, right) => {
  if (left.byteLength !== right.byteLength || typeof Uint8Array !== "function") return left.byteLength === right.byteLength;
  const leftBytes = new Uint8Array(left);
  const rightBytes = new Uint8Array(right);
  for (let index = 0; index < leftBytes.length; index += 1) {
    if (leftBytes[index] !== rightBytes[index]) return false;
  }
  return true;
};

const compareProperties = (left, right, state, strict) => {
  const leftKeys = enumerableKeys(left);
  const rightKeys = enumerableKeys(right);
  if (leftKeys.length !== rightKeys.length) return false;
  for (const key of leftKeys) {
    if (!hasKey(rightKeys, key) || !compareValues(left[key], right[key], state, strict)) return false;
  }
  return true;
};

const snapshotState = state => ({
  left: state.left.slice(),
  right: state.right.slice(),
});

const restoreState = (state, snapshot) => {
  state.left.length = 0;
  state.right.length = 0;
  state.left.push(...snapshot.left);
  state.right.push(...snapshot.right);
};

const compareMap = (left, right, state, strict) => {
  if (left.size !== right.size) return false;
  const rightEntries = [];
  right.forEach((value, key) => rightEntries.push([key, value]));
  const matched = [];
  let result = true;
  left.forEach((leftValue, leftKey) => {
    if (!result) return;
    let found = false;
    for (let index = 0; index < rightEntries.length; index += 1) {
      if (matched[index]) continue;
      const snapshot = snapshotState(state);
      const entry = rightEntries[index];
      if (compareValues(leftKey, entry[0], state, strict) && compareValues(leftValue, entry[1], state, strict)) {
        matched[index] = true;
        found = true;
        break;
      }
      restoreState(state, snapshot);
    }
    if (!found) result = false;
  });
  return result;
};

const compareSet = (left, right, state, strict) => {
  if (left.size !== right.size) return false;
  const rightValues = [];
  right.forEach(value => rightValues.push(value));
  const matched = [];
  let result = true;
  left.forEach(leftValue => {
    if (!result) return;
    let found = false;
    for (let index = 0; index < rightValues.length; index += 1) {
      if (matched[index]) continue;
      const snapshot = snapshotState(state);
      if (compareValues(leftValue, rightValues[index], state, strict)) {
        matched[index] = true;
        found = true;
        break;
      }
      restoreState(state, snapshot);
    }
    if (!found) result = false;
  });
  return result;
};

const compareValues = (left, right, state, strict) => {
  if (strict && sameValue(left, right)) return true;
  if (!strict && !isObjectLike(left) && !isObjectLike(right)
    && (left == right || (isNaNValue(left) && isNaNValue(right)))) return true;
  if (!isObjectLike(left) || !isObjectLike(right)) return false;
  if (typeof left !== typeof right) return false;
  if (typeof left === "function") return false;

  const leftIndex = state.left.indexOf(left);
  if (leftIndex !== -1) return state.right[leftIndex] === right;
  if (state.right.indexOf(right) !== -1) return false;
  state.left.push(left);
  state.right.push(right);

  const leftTag = objectToString.call(left);
  const rightTag = objectToString.call(right);
  if (leftTag !== rightTag) return false;
  if (strict && Object.getPrototypeOf(left) !== Object.getPrototypeOf(right)) return false;

  if (leftTag === "[object Date]") {
    return sameValue(left.getTime(), right.getTime());
  }
  if (leftTag === "[object RegExp]") {
    return left.source === right.source && left.flags === right.flags && left.lastIndex === right.lastIndex;
  }
  if (leftTag === "[object Map]") return compareMap(left, right, state, strict);
  if (leftTag === "[object Set]") return compareSet(left, right, state, strict);
  if (leftTag === "[object Promise]" || leftTag === "[object WeakMap]" || leftTag === "[object WeakSet]") return false;
  if (leftTag === "[object ArrayBuffer]") return compareArrayBufferBytes(left, right);
  if (leftTag === "[object Error]") {
    if (left.name !== right.name || left.message !== right.message) return false;
  }
  if (leftTag === "[object Number]" || leftTag === "[object String]" || leftTag === "[object Boolean]") {
    const leftPrimitive = left.valueOf();
    const rightPrimitive = right.valueOf();
    if (!(strict ? sameValue(leftPrimitive, rightPrimitive) : leftPrimitive == rightPrimitive)) return false;
  }
  return compareProperties(left, right, state, strict);
};

const deepEqualValues = (left, right, strict) => compareValues(left, right, { left: [], right: [] }, strict);

export function fail(actual, expected, message, operator) {
  if (arguments.length <= 1) {
    if (actual instanceof Error) throw actual;
    assertion(actual === undefined ? "Failed" : actual, undefined, undefined, "fail");
  }
  assertion(message, actual, expected, operator === undefined ? "fail" : operator);
}

export function ok(value, message) {
  if (!value) {
    const defaultMessage = arguments.length === 0 ? "No value argument passed to `assert.ok()`" : undefined;
    assertion(message === undefined ? defaultMessage : message, value, true, "ok");
  }
}

export function equal(actual, expected, message) {
  if (!(actual == expected || (isNaNValue(actual) && isNaNValue(expected)))) assertion(message, actual, expected, "==");
}

export function notEqual(actual, expected, message) {
  if (actual == expected || (isNaNValue(actual) && isNaNValue(expected))) assertion(message, actual, expected, "!=");
}

export function strictEqual(actual, expected, message) {
  if (!sameValue(actual, expected)) assertion(message, actual, expected, "strictEqual");
}

export function notStrictEqual(actual, expected, message) {
  if (sameValue(actual, expected)) assertion(message, actual, expected, "notStrictEqual");
}

export function deepEqual(actual, expected, message) {
  if (!deepEqualValues(actual, expected, false)) assertion(message, actual, expected, "deepEqual");
}

export function notDeepEqual(actual, expected, message) {
  if (deepEqualValues(actual, expected, false)) assertion(message, actual, expected, "notDeepEqual");
}

export function deepStrictEqual(actual, expected, message) {
  if (!deepEqualValues(actual, expected, true)) assertion(message, actual, expected, "deepStrictEqual");
}

export function notDeepStrictEqual(actual, expected, message) {
  if (deepEqualValues(actual, expected, true)) assertion(message, actual, expected, "notDeepStrictEqual");
}

const isRegExp = value => objectToString.call(value) === "[object RegExp]";
const isClassConstructor = value => typeof value === "function"
  && Function.prototype.toString.call(value).startsWith("class ");
const isErrorConstructor = value => typeof value === "function"
  && (value === Error || value.prototype instanceof Error || isClassConstructor(value));

const matchesError = (thrown, expected) => {
  if (expected === undefined) return true;
  if (isRegExp(expected)) {
    expected.lastIndex = 0;
    const result = expected.test(String(thrown));
    expected.lastIndex = 0;
    return result;
  }
  if (typeof expected === "function") {
    if (isErrorConstructor(expected)) return thrown instanceof expected;
    return expected(thrown) === true;
  }
  if (isObjectLike(expected)) {
    const keys = enumerableKeys(expected);
    if (objectToString.call(expected) === "[object Error]") keys.push("name", "message");
    for (const key of keys) {
      const expectedValue = expected[key];
      const actualValue = thrown === null || thrown === undefined ? undefined : thrown[key];
      if (isRegExp(expectedValue) && typeof actualValue === "string") {
        expectedValue.lastIndex = 0;
        const result = expectedValue.test(actualValue);
        expectedValue.lastIndex = 0;
        if (!result) return false;
      } else if (!deepEqualValues(actualValue, expectedValue, true)) {
        return false;
      }
    }
    return true;
  }
  return false;
};

const assertionMessage = (message, fallback) => message === undefined ? fallback : message;

export function throws(block, expected, message) {
  if (typeof expected === "string" && message === undefined) {
    message = expected;
    expected = undefined;
  }
  let thrown = false;
  let value;
  try {
    block();
  } catch (error) {
    thrown = true;
    value = error;
  }
  if (!thrown) assertion(assertionMessage(message, "Missing expected exception"), undefined, expected, "throws");
  if (!matchesError(value, expected)) assertion(assertionMessage(message, "The thrown error did not match the expected error"), value, expected, "throws");
}

export function doesNotThrow(block, expected, message) {
  if (typeof expected === "string" && message === undefined) {
    message = expected;
    expected = undefined;
  }
  if (expected instanceof Error && message === undefined) {
    message = expected;
    expected = undefined;
  }
  try {
    block();
  } catch (error) {
    if (expected === undefined || !matchesError(error, expected)) throw error;
    assertion(assertionMessage(message, "Got unwanted exception"), error, undefined, "doesNotThrow");
  }
}

const invalidPromise = () => Promise.reject(new TypeError("The promise or function must return a Promise"));

const toPromise = block => {
  let value;
  if (typeof block === "function") {
    try {
      value = block();
    } catch (error) {
      return Promise.reject(error);
    }
  } else {
    value = block;
  }
  if (value === null || (typeof value !== "object" && typeof value !== "function")) return invalidPromise();
  try {
    if (typeof value.then !== "function") return invalidPromise();
    return Promise.resolve(value);
  } catch (error) {
    return Promise.reject(error);
  }
};

export function rejects(block, expected, message) {
  if (typeof expected === "string" && message === undefined) {
    message = expected;
    expected = undefined;
  }
  return toPromise(block).then(
    () => {
      assertion(assertionMessage(message, "Missing expected rejection"), undefined, expected, "rejects");
    },
    error => {
      if (!matchesError(error, expected)) assertion(assertionMessage(message, "The rejection error did not match the expected error"), error, expected, "rejects");
    },
  );
}

export function doesNotReject(block, expected, message) {
  if (typeof expected === "string" && message === undefined) {
    message = expected;
    expected = undefined;
  }
  if (expected instanceof Error && message === undefined) {
    message = expected;
    expected = undefined;
  }
  return toPromise(block).then(
    () => undefined,
    error => {
      if (expected !== undefined && matchesError(error, expected)) {
        assertion(assertionMessage(message, "Got unwanted rejection"), error, undefined, "doesNotReject");
      }
      throw error;
    },
  );
}

export function ifError(value) {
  if (value !== null && value !== undefined) {
    assertion(`ifError got unwanted exception: ${formatValue(value)}`, value, null, "ifError");
  }
}

const assertStringAndRegExp = (value, regExp, message, negative) => {
  if (typeof value !== "string") throw new TypeError('The "string" argument must be of type string');
  if (!isRegExp(regExp)) throw new TypeError('The "regexp" argument must be an instance of RegExp');
  regExp.lastIndex = 0;
  const matched = regExp.test(value);
  regExp.lastIndex = 0;
  if (matched === negative) {
    assertion(message, value, regExp, negative ? "doesNotMatch" : "match");
  }
};

export function match(value, regExp, message) {
  assertStringAndRegExp(value, regExp, message, false);
}

export function doesNotMatch(value, regExp, message) {
  assertStringAndRegExp(value, regExp, message, true);
}

const assert = function assert(value, message) {
  return ok(value, message);
};

const strict = function strict(value, message) {
  return ok(value, message);
};

assert.AssertionError = AssertionError;
assert.fail = fail;
assert.ok = ok;
assert.equal = equal;
assert.notEqual = notEqual;
assert.strictEqual = strictEqual;
assert.notStrictEqual = notStrictEqual;
assert.deepEqual = deepEqual;
assert.notDeepEqual = notDeepEqual;
assert.deepStrictEqual = deepStrictEqual;
assert.notDeepStrictEqual = notDeepStrictEqual;
assert.throws = throws;
assert.doesNotThrow = doesNotThrow;
assert.rejects = rejects;
assert.doesNotReject = doesNotReject;
assert.ifError = ifError;
assert.match = match;
assert.doesNotMatch = doesNotMatch;

strict.AssertionError = AssertionError;
strict.fail = fail;
strict.ok = ok;
strict.equal = strictEqual;
strict.notEqual = notStrictEqual;
strict.strictEqual = strictEqual;
strict.notStrictEqual = notStrictEqual;
strict.deepEqual = deepStrictEqual;
strict.notDeepEqual = notDeepStrictEqual;
strict.deepStrictEqual = deepStrictEqual;
strict.notDeepStrictEqual = notDeepStrictEqual;
strict.throws = throws;
strict.doesNotThrow = doesNotThrow;
strict.rejects = rejects;
strict.doesNotReject = doesNotReject;
strict.ifError = ifError;
strict.match = match;
strict.doesNotMatch = doesNotMatch;
strict.strict = strict;
assert.strict = strict;

export { AssertionError, assert as default, strict };
