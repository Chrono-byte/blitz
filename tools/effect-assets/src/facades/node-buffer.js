const K_MAX_LENGTH = 0x7fffffff;
const INSPECT_MAX_BYTES = 50;
const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

const normalizeEncoding = encoding => {
  const value = encoding === undefined ? "utf8" : String(encoding).toLowerCase().replace(/[-_]/g, "");
  if (value === "utf8" || value === "utf") return "utf8";
  if (value === "utf16le" || value === "ucs2" || value === "ucs2le") return "utf16le";
  if (value === "latin1" || value === "binary") return "latin1";
  if (value === "ascii" || value === "hex" || value === "base64" || value === "base64url") return value;
  throw new TypeError(`Unknown encoding: ${encoding}`);
};

const stringCodeUnits = value => {
  const units = [];
  for (let index = 0;; index += 1) {
    const unit = value.charCodeAt(index);
    if (Number.isNaN(unit)) return units;
    units.push(unit);
  }
};

const utf8Bytes = value => {
  const units = stringCodeUnits(value);
  const out = [];
  for (let index = 0; index < units.length; index += 1) {
    let point = units[index];
    if (point >= 0xd800 && point <= 0xdbff && index + 1 < units.length) {
      const low = units[index + 1];
      if (low >= 0xdc00 && low <= 0xdfff) {
        point = 0x10000 + ((point - 0xd800) << 10) + low - 0xdc00;
        index += 1;
      } else point = 0xfffd;
    } else if (point >= 0xdc00 && point <= 0xdfff) point = 0xfffd;
    if (point <= 0x7f) out.push(point);
    else if (point <= 0x7ff) out.push(0xc0 | point >> 6, 0x80 | point & 0x3f);
    else if (point <= 0xffff) out.push(0xe0 | point >> 12, 0x80 | point >> 6 & 0x3f, 0x80 | point & 0x3f);
    else out.push(0xf0 | point >> 18, 0x80 | point >> 12 & 0x3f, 0x80 | point >> 6 & 0x3f, 0x80 | point & 0x3f);
  }
  return out;
};

const encodeString = (value, encoding) => {
  const text = String(value);
  switch (normalizeEncoding(encoding)) {
    case "utf8": return utf8Bytes(text);
    case "ascii":
    case "latin1": return stringCodeUnits(text).map(unit => unit & 0xff);
    case "utf16le": {
      const out = [];
      for (const unit of stringCodeUnits(text)) out.push(unit & 0xff, unit >>> 8);
      return out;
    }
    case "hex": {
      const out = [];
      for (let index = 0; index + 1 < text.length; index += 2) {
        const byte = parseInt(text.slice(index, index + 2), 16);
        if (!Number.isFinite(byte)) break;
        out.push(byte);
      }
      return out;
    }
    case "base64":
    case "base64url": {
      const cleaned = text.replace(/-/g, "+").replace(/_/g, "/").replace(/[^A-Za-z0-9+/]/g, "");
      const out = [];
      for (let index = 0; index < cleaned.length; index += 4) {
        const a = alphabet.indexOf(cleaned[index]);
        const b = alphabet.indexOf(cleaned[index + 1]);
        const c = alphabet.indexOf(cleaned[index + 2]);
        const d = alphabet.indexOf(cleaned[index + 3]);
        if (a < 0 || b < 0) break;
        out.push(a << 2 | b >> 4);
        if (c >= 0) out.push((b & 15) << 4 | c >> 2);
        if (d >= 0) out.push((c & 3) << 6 | d);
      }
      return out;
    }
  }
};

const decodeUtf8 = bytes => {
  let out = "";
  for (let index = 0; index < bytes.length;) {
    const first = bytes[index++];
    if (first < 0x80) { out += String.fromCharCode(first); continue; }
    let count = 0;
    let point = 0;
    if ((first & 0xe0) === 0xc0) { count = 1; point = first & 0x1f; }
    else if ((first & 0xf0) === 0xe0) { count = 2; point = first & 0x0f; }
    else if ((first & 0xf8) === 0xf0) { count = 3; point = first & 7; }
    else { out += "\ufffd"; continue; }
    if (index + count > bytes.length) { out += "\ufffd"; break; }
    let valid = true;
    for (let offset = 0; offset < count; offset += 1) {
      const next = bytes[index++];
      if ((next & 0xc0) !== 0x80) { valid = false; break; }
      point = point << 6 | next & 0x3f;
    }
    if (!valid || point > 0x10ffff || point >= 0xd800 && point <= 0xdfff) { out += "\ufffd"; continue; }
    if (point <= 0xffff) out += String.fromCharCode(point);
    else {
      point -= 0x10000;
      out += String.fromCharCode(0xd800 | point >> 10, 0xdc00 | point & 0x3ff);
    }
  }
  return out;
};

const decodeBytes = (bytes, encoding) => {
  switch (normalizeEncoding(encoding)) {
    case "utf8": return decodeUtf8(bytes);
    case "ascii": return Array.from(bytes, byte => String.fromCharCode(byte & 0x7f)).join("");
    case "latin1": return Array.from(bytes, byte => String.fromCharCode(byte)).join("");
    case "utf16le": {
      let out = "";
      for (let index = 0; index + 1 < bytes.length; index += 2) out += String.fromCharCode(bytes[index] | bytes[index + 1] << 8);
      return out;
    }
    case "hex": {
      const digits = "0123456789abcdef";
      return Array.from(bytes, byte => digits[byte >>> 4] + digits[byte & 15]).join("");
    }
    case "base64":
    case "base64url": {
      let out = "";
      for (let index = 0; index < bytes.length; index += 3) {
        const a = bytes[index];
        const b = index + 1 < bytes.length ? bytes[index + 1] : 0;
        const c = index + 2 < bytes.length ? bytes[index + 2] : 0;
        out += alphabet[a >> 2] + alphabet[(a & 3) << 4 | b >> 4];
        out += index + 1 < bytes.length ? alphabet[(b & 15) << 2 | c >> 6] : "=";
        out += index + 2 < bytes.length ? alphabet[c & 63] : "=";
      }
      return normalizeEncoding(encoding) === "base64url" ? out.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "") : out;
    }
  }
};

const brand = value => {
  Object.setPrototypeOf(value, Buffer.prototype);
  return value;
};
const checkedSize = value => {
  const size = Number(value);
  if (!Number.isInteger(size) || size < 0 || size > K_MAX_LENGTH) throw new RangeError("The value of size is out of range");
  return size;
};
const normalizeIndex = (value, length, fallback) => {
  if (value === undefined) return fallback;
  const number = Number(value);
  const integer = number < 0 ? -Math.floor(-number) : Math.floor(number);
  if (!Number.isFinite(integer)) return integer < 0 ? 0 : length;
  return integer < 0 ? Math.max(length + integer, 0) : Math.min(integer, length);
};

function Buffer(value, encodingOrOffset, length) {
  return typeof value === "number"
    ? Buffer.allocUnsafe(value)
    : Buffer.from(value, encodingOrOffset, length);
}
Buffer.prototype = Object.create(Uint8Array.prototype);
Object.defineProperty(Buffer.prototype, "constructor", { value: Buffer, writable: true, configurable: true });

Buffer.from = (value, encodingOrOffset, length) => {
  if (typeof value === "string") return brand(new Uint8Array(encodeString(value, encodingOrOffset)));
  if (value instanceof ArrayBuffer || typeof SharedArrayBuffer === "function" && value instanceof SharedArrayBuffer) {
    const offset = encodingOrOffset === undefined ? 0 : Number(encodingOrOffset);
    const available = value.byteLength - offset;
    return brand(new Uint8Array(value, offset, length === undefined ? available : Number(length)));
  }
  if (value === null || value === undefined) throw new TypeError("The first argument must be a string, Buffer, ArrayBuffer, Array, or array-like object");
  if (value instanceof Uint8Array || Array.isArray(value) || typeof value.length === "number") return brand(new Uint8Array(Array.from(value)));
  if (typeof value.valueOf === "function" && value.valueOf() !== value) return Buffer.from(value.valueOf(), encodingOrOffset, length);
  throw new TypeError("The first argument must be a string, Buffer, ArrayBuffer, Array, or array-like object");
};
Buffer.alloc = (size, fill = 0, encoding) => {
  const result = brand(new Uint8Array(checkedSize(size)));
  if (typeof fill === "string") {
    const pattern = Buffer.from(fill, encoding);
    if (pattern.length === 0) return result;
    for (let index = 0; index < result.length; index += 1) result[index] = pattern[index % pattern.length];
  } else if (fill instanceof Uint8Array) {
    if (fill.length === 0) return result;
    for (let index = 0; index < result.length; index += 1) result[index] = fill[index % fill.length];
  } else if (fill !== 0) result.fill(fill);
  return result;
};
Buffer.allocUnsafe = size => brand(new Uint8Array(checkedSize(size)));
Buffer.allocUnsafeSlow = Buffer.allocUnsafe;
Buffer.byteLength = (value, encoding) => typeof value === "string" ? encodeString(value, encoding).length : value.byteLength;
Buffer.compare = (left, right) => left.compare(right);
Buffer.concat = (list, totalLength) => {
  if (!Array.isArray(list)) throw new TypeError("list must be an Array of Buffer or Uint8Array instances");
  const length = totalLength === undefined ? list.reduce((sum, item) => sum + item.length, 0) : checkedSize(totalLength);
  const result = Buffer.allocUnsafe(length);
  let offset = 0;
  for (const item of list) for (let index = 0; index < item.length && offset < length; index += 1) result[offset++] = item[index];
  while (offset < length) result[offset++] = 0;
  return result;
};
Buffer.isBuffer = value => value instanceof Buffer;
Buffer.isEncoding = value => { try { normalizeEncoding(value); return true; } catch { return false; } };
Buffer.poolSize = 8192;

Buffer.prototype.toString = function (encoding = "utf8", start = 0, end = this.length) {
  const from = normalizeIndex(start, this.length, 0);
  const to = normalizeIndex(end, this.length, this.length);
  return decodeBytes(Array.from(this.subarray(from, Math.max(from, to))), encoding);
};
Buffer.prototype.toJSON = function () { return { type: "Buffer", data: Array.from(this) }; };
Buffer.prototype.equals = function (other) { return this.compare(other) === 0; };
Buffer.prototype.compare = function (other, targetStart = 0, targetEnd = other.length, sourceStart = 0, sourceEnd = this.length) {
  const left = this.subarray(sourceStart, sourceEnd);
  const right = other.subarray(targetStart, targetEnd);
  const length = Math.min(left.length, right.length);
  for (let index = 0; index < length; index += 1) if (left[index] !== right[index]) return left[index] < right[index] ? -1 : 1;
  return left.length === right.length ? 0 : left.length < right.length ? -1 : 1;
};
Buffer.prototype.subarray = function (start = 0, end = this.length) {
  const from = normalizeIndex(start, this.length, 0);
  const to = normalizeIndex(end, this.length, this.length);
  return brand(new Uint8Array(this.buffer, this.byteOffset + from, Math.max(0, to - from)));
};
Buffer.prototype.slice = Buffer.prototype.subarray;
Buffer.prototype.copy = function (target, targetStart = 0, sourceStart = 0, sourceEnd = this.length) {
  const source = Array.from(this.subarray(sourceStart, sourceEnd));
  let copied = 0;
  for (; copied < source.length && targetStart + copied < target.length; copied += 1) target[targetStart + copied] = source[copied];
  return copied;
};
Buffer.prototype.write = function (value, offset = 0, length, encoding = "utf8") {
  if (typeof length === "string") { encoding = length; length = undefined; }
  const bytes = encodeString(value, encoding);
  const maximum = Math.min(length === undefined ? bytes.length : Number(length), this.length - offset);
  for (let index = 0; index < maximum; index += 1) this[offset + index] = bytes[index];
  return maximum;
};
Buffer.prototype.readUInt8 = function (offset = 0) { return this[offset]; };
Buffer.prototype.readInt8 = function (offset = 0) { const value = this[offset]; return value & 0x80 ? value - 0x100 : value; };
Buffer.prototype.writeUInt8 = function (value, offset = 0) { this[offset] = value; return offset + 1; };
Buffer.prototype.writeInt8 = Buffer.prototype.writeUInt8;

const SlowBuffer = size => Buffer.alloc(size);
const constants = Object.freeze({ MAX_LENGTH: K_MAX_LENGTH, MAX_STRING_LENGTH: 0x1fffffe8 });
const kMaxLength = K_MAX_LENGTH;
const kStringMaxLength = constants.MAX_STRING_LENGTH;
const atob = value => Buffer.from(value, "base64").toString("latin1");
const btoa = value => Buffer.from(value, "latin1").toString("base64");
const inputBytes = input => input instanceof ArrayBuffer || typeof SharedArrayBuffer === "function" && input instanceof SharedArrayBuffer
  ? new Uint8Array(input)
  : input;
const isAscii = input => Array.from(inputBytes(input)).every(byte => byte <= 0x7f);
const isUtf8 = input => decodeUtf8(Array.from(inputBytes(input))).indexOf("\ufffd") === -1;

if (typeof globalThis.Buffer === "undefined") globalThis.Buffer = Buffer;

const buffer = { Buffer, SlowBuffer, INSPECT_MAX_BYTES, constants, kMaxLength, kStringMaxLength, atob, btoa, isAscii, isUtf8 };
export { Buffer, SlowBuffer, INSPECT_MAX_BYTES, constants, kMaxLength, kStringMaxLength, atob, btoa, isAscii, isUtf8 };
export default buffer;
