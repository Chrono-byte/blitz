import assert from "node:assert";

export {
  AssertionError,
  fail,
  ok,
  strictEqual,
  notStrictEqual,
  deepStrictEqual,
  notDeepStrictEqual,
  throws,
  doesNotThrow,
  rejects,
  doesNotReject,
  ifError,
  match,
  doesNotMatch,
} from "node:assert";

export const equal = assert.strictEqual;
export const notEqual = assert.notStrictEqual;
export const deepEqual = assert.deepStrictEqual;
export const notDeepEqual = assert.notDeepStrictEqual;
export const strict = assert.strict;

export default assert.strict;
