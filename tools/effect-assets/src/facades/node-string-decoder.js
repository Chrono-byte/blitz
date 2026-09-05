// A pure-JavaScript Node 24 string_decoder slice. Blitz does not provide a
// faithful Buffer or TextDecoder, so this facade deliberately works with
// Uint8Array and number arrays instead of pretending to implement Buffer.
const states = new WeakMap();
const replacement = "\ufffd";

const unknownEncoding = encoding => {
  const error = new TypeError(`Unknown encoding: ${String(encoding)}`);
  error.code = "ERR_UNKNOWN_ENCODING";
  throw error;
};

const normalizeEncoding = encoding => {
  if (encoding === undefined || encoding === null || encoding === "") return "utf8";
  const value = String(encoding).toLowerCase();
  if (value === "utf8" || value === "utf-8") return "utf8";
  if (value === "latin1" || value === "binary") return "latin1";
  if (value === "ascii") return "ascii";
  if (value === "utf16le" || value === "utf-16le" || value === "ucs2" || value === "ucs-2") return "utf16le";
  return unknownEncoding(encoding);
};

const isUint8Array = value => typeof Uint8Array === "function" && value instanceof Uint8Array;

const invalidBuffer = value => {
  const received = value === null ? "null" : value === undefined ? "undefined" : `an instance of ${Object.prototype.toString.call(value).slice(8, -1)}`;
  const error = new TypeError(`The \"buf\" argument must be a Uint8Array or an array of bytes. Received ${received}`);
  error.code = "ERR_INVALID_ARG_TYPE";
  throw error;
};

const bytesOf = value => {
  if (isUint8Array(value)) return value;
  if (!Array.isArray(value)) return invalidBuffer(value);
  const bytes = new Uint8Array(value.length);
  for (let index = 0; index < value.length; index++) bytes[index] = value[index];
  return bytes;
};

const copyBytes = (bytes, start = 0, end = bytes.length) => {
  const result = [];
  for (let index = start; index < end && index < bytes.length; index++) result.push(bytes[index]);
  return result;
};

const zeroLastChar = lastChar => {
  for (let index = 0; index < lastChar.length; index++) lastChar[index] = 0;
};

const stateOf = decoder => {
  const state = states.get(decoder);
  if (state === undefined) throw new TypeError("ERR_INVALID_THIS");
  return state;
};

const clearPending = (state, resetChar) => {
  state.pending = [];
  state.pendingKind = "";
  state.lastNeed = 0;
  state.lastTotal = 0;
  if (resetChar) zeroLastChar(state.lastChar);
};

const setPending = (state, bytes, kind, total) => {
  state.pending = bytes.slice();
  state.pendingKind = kind;
  state.lastNeed = total - bytes.length;
  state.lastTotal = total;
  if (kind === "utf16-odd") {
    state.lastChar[0] = bytes[0];
    return;
  }
  zeroLastChar(state.lastChar);
  for (let index = 0; index < bytes.length && index < 4; index++) state.lastChar[index] = bytes[index];
};

const appendPending = (state, byte) => {
  const index = state.pending.length;
  state.pending.push(byte);
  state.lastChar[index] = byte;
  state.lastNeed -= 1;
};

const utf8Total = byte => {
  if (byte >= 0xc2 && byte <= 0xdf) return 2;
  if (byte >= 0xe0 && byte <= 0xef) return 3;
  if (byte >= 0xf0 && byte <= 0xf4) return 4;
  return 0;
};

const validUtf8Continuation = (pending, byte) => {
  if (byte < 0x80 || byte > 0xbf) return false;
  if (pending.length !== 1) return true;
  const first = pending[0];
  if (first === 0xe0) return byte >= 0xa0;
  if (first === 0xed) return byte <= 0x9f;
  if (first === 0xf0) return byte >= 0x90;
  if (first === 0xf4) return byte <= 0x8f;
  return true;
};

const codePointFromUtf8 = bytes => {
  if (bytes.length === 2) return ((bytes[0] & 0x1f) << 6) | (bytes[1] & 0x3f);
  if (bytes.length === 3) return ((bytes[0] & 0x0f) << 12) | ((bytes[1] & 0x3f) << 6) | (bytes[2] & 0x3f);
  return ((bytes[0] & 0x07) << 18) | ((bytes[1] & 0x3f) << 12) | ((bytes[2] & 0x3f) << 6) | (bytes[3] & 0x3f);
};

const utf8Write = (state, bytes) => {
  let result = "";
  let index = 0;

  if (state.pendingKind === "utf8") {
    while (state.pending.length < state.lastTotal && index < bytes.length) {
      const byte = bytes[index];
      if (!validUtf8Continuation(state.pending, byte)) {
        result += replacement;
        clearPending(state, false);
        break;
      }
      appendPending(state, byte);
      index += 1;
    }
    if (state.pendingKind === "utf8" && state.pending.length === state.lastTotal) {
      result += String.fromCodePoint(codePointFromUtf8(state.pending));
      clearPending(state, false);
    }
  }

  while (index < bytes.length) {
    const byte = bytes[index];
    if (byte <= 0x7f) {
      result += String.fromCharCode(byte);
      index += 1;
      continue;
    }

    const total = utf8Total(byte);
    if (total === 0) {
      result += replacement;
      index += 1;
      continue;
    }

    const available = bytes.length - index;
    if (available < total) {
      const partial = copyBytes(bytes, index);
      let validPartial = true;
      let invalidAt = 1;
      for (let continuation = 1; continuation < partial.length; continuation++) {
        if (!validUtf8Continuation(partial.slice(0, continuation), partial[continuation])) {
          validPartial = false;
          invalidAt = continuation;
          break;
        }
      }
      if (validPartial) {
        setPending(state, partial, "utf8", total);
        break;
      }
      result += replacement;
      index += invalidAt;
      continue;
    }

    const sequence = copyBytes(bytes, index, index + total);
    let valid = true;
    let invalidAt = 1;
    for (let continuation = 1; continuation < sequence.length; continuation++) {
      if (!validUtf8Continuation(sequence.slice(0, continuation), sequence[continuation])) {
        valid = false;
        invalidAt = continuation;
        break;
      }
    }
    if (valid) {
      result += String.fromCodePoint(codePointFromUtf8(sequence));
      index += total;
    } else {
      // The lead and any valid continuation prefix become one replacement.
      result += replacement;
      index += invalidAt;
    }
  }
  return result;
};

const utf16CodeUnit = (low, high) => low | (high << 8);

const utf16Write = (state, bytes) => {
  let result = "";
  let index = 0;

  if (state.pendingKind === "utf16-high") {
    const needed = 4 - state.pending.length;
    if (bytes.length - index < needed) {
      while (index < bytes.length) {
        appendPending(state, bytes[index]);
        index += 1;
      }
      return result;
    }
    while (state.pending.length < 4) {
      appendPending(state, bytes[index]);
      index += 1;
    }
    const next = utf16CodeUnit(state.pending[2], state.pending[3]);
    const high = utf16CodeUnit(state.pending[0], state.pending[1]);
    if (next >= 0xdc00 && next <= 0xdfff) {
      result += String.fromCodePoint(0x10000 + ((high - 0xd800) << 10) + next - 0xdc00);
      clearPending(state, false);
    } else {
      const nextBytes = [state.pending[2], state.pending[3]];
      result += String.fromCharCode(high);
      clearPending(state, false);
      if (next >= 0xd800 && next <= 0xdbff) {
        setPending(state, nextBytes, "utf16-high", 4);
        return result + utf16Write(state, copyBytes(bytes, index));
      }
      result += String.fromCharCode(next);
    }
  }

  if (state.pendingKind === "utf16-odd") {
    if (index === bytes.length) return result;
    const unit = utf16CodeUnit(state.pending[0], bytes[index]);
    state.lastChar[1] = bytes[index];
    index += 1;
    clearPending(state, false);
    result += String.fromCharCode(unit);
  }

  while (index + 1 < bytes.length) {
    const low = bytes[index];
    const high = bytes[index + 1];
    const unit = utf16CodeUnit(low, high);
    index += 2;
    if (unit >= 0xd800 && unit <= 0xdbff) {
      if (index + 1 >= bytes.length) {
        if (index < bytes.length) {
          result += String.fromCharCode(unit);
          setPending(state, [bytes[index]], "utf16-odd", 2);
        } else {
          setPending(state, [low, high], "utf16-high", 4);
        }
        return result;
      }
      const next = utf16CodeUnit(bytes[index], bytes[index + 1]);
      if (next >= 0xdc00 && next <= 0xdfff) {
        result += String.fromCodePoint(0x10000 + ((unit - 0xd800) << 10) + next - 0xdc00);
        index += 2;
      } else {
        result += String.fromCharCode(unit);
      }
    } else {
      result += String.fromCharCode(unit);
    }
  }

  if (index < bytes.length) setPending(state, [bytes[index]], "utf16-odd", 2);
  return result;
};

const directUtf8 = bytes => {
  let result = "";
  let index = 0;
  while (index < bytes.length) {
    const byte = bytes[index];
    if (byte <= 0x7f) {
      result += String.fromCharCode(byte);
      index += 1;
      continue;
    }
    const normalTotal = utf8Total(byte);
    const total = normalTotal !== 0 ? normalTotal : byte >= 0xc0 && byte <= 0xc1 ? 2 : byte >= 0xf5 && byte <= 0xf7 ? 4 : 0;
    if (total === 0) {
      result += replacement;
      index += 1;
      continue;
    }
    const available = bytes.length - index;
    const sequence = copyBytes(bytes, index, Math.min(bytes.length, index + total));
    let invalidAt = 0;
    for (let continuation = 1; continuation < sequence.length; continuation++) {
      if (!validUtf8Continuation(sequence.slice(0, continuation), sequence[continuation])) {
        invalidAt = continuation;
        break;
      }
    }
    if (normalTotal === 0) {
      if (available < total && invalidAt === 0) break;
      result += replacement;
      index += 1;
      continue;
    }
    const boundaryInvalid = (byte === 0xe0 && sequence[1] < 0xa0)
      || (byte === 0xed && sequence[1] > 0x9f)
      || (byte === 0xf0 && sequence[1] < 0x90)
      || (byte === 0xf4 && sequence[1] > 0x8f);
    if (available < total && (invalidAt === 0 || (invalidAt === 1 && boundaryInvalid))) break;
    if (invalidAt !== 0) {
      result += replacement;
      index += invalidAt;
      continue;
    }
    result += String.fromCodePoint(codePointFromUtf8(sequence));
    index += total;
  }
  return result;
};

const directUtf16 = bytes => {
  const state = { pending: [], pendingKind: "", lastNeed: 0, lastTotal: 0, lastChar: new Uint8Array(4) };
  return utf16Write(state, bytes);
};

const directText = (encoding, bytes, start, end) => {
  const from = start === undefined ? 0 : Math.max(0, Number(start));
  const to = end === undefined ? bytes.length : Math.min(bytes.length, Number(end));
  const selected = copyBytes(bytes, from, Math.max(from, to));
  if (encoding === "latin1") {
    let result = "";
    for (const byte of selected) result += String.fromCharCode(byte);
    return result;
  }
  if (encoding === "ascii") {
    let result = "";
    for (const byte of selected) result += String.fromCharCode(byte & 0x7f);
    return result;
  }
  if (encoding === "utf16le") return directUtf16(selected);
  return directUtf8(selected);
};

function StringDecoder(encoding) {
  const normalized = normalizeEncoding(encoding);
  const state = {
    pending: [],
    pendingKind: "",
    lastNeed: 0,
    lastTotal: 0,
    lastChar: new Uint8Array(4),
    encoding: normalized,
  };
  this.encoding = normalized;
  states.set(this, state);
}

StringDecoder.prototype.write = function(buffer) {
  const state = stateOf(this);
  const bytes = bytesOf(buffer);
  if (state.encoding === "latin1") {
    let result = "";
    for (const byte of bytes) result += String.fromCharCode(byte);
    return result;
  }
  if (state.encoding === "ascii") {
    let result = "";
    for (const byte of bytes) result += String.fromCharCode(byte & 0x7f);
    return result;
  }
  return state.encoding === "utf16le" ? utf16Write(state, bytes) : utf8Write(state, bytes);
};

StringDecoder.prototype.end = function(buffer) {
  const state = stateOf(this);
  let result = "";
  if (buffer !== undefined) result = this.write(buffer);
  if (state.pendingKind === "utf8") {
    result += replacement;
    clearPending(state, false);
  } else if (state.pendingKind === "utf16-high") {
    result += String.fromCharCode(utf16CodeUnit(state.pending[0], state.pending[1]));
    clearPending(state, false);
  } else if (state.pendingKind === "utf16-odd") {
    clearPending(state, false);
  }
  return result;
};

StringDecoder.prototype.text = function(buffer, start, end) {
  const state = stateOf(this);
  return directText(state.encoding, bytesOf(buffer), start, end);
};

Object.defineProperties(StringDecoder.prototype, {
  lastChar: {
    configurable: true,
    enumerable: true,
    get() { return stateOf(this).lastChar; },
  },
  lastNeed: {
    configurable: true,
    enumerable: true,
    get() { return stateOf(this).lastNeed; },
  },
  lastTotal: {
    configurable: true,
    enumerable: true,
    get() { return stateOf(this).lastTotal; },
  },
});

const stringDecoder = { StringDecoder };

export { StringDecoder };
export default stringDecoder;
