const isHex = (code) =>
  (code >= 48 && code <= 57) ||
  (code >= 65 && code <= 70) ||
  (code >= 97 && code <= 102);

const hexValue = (code) =>
  code >= 48 && code <= 57
    ? code - 48
    : code >= 65 && code <= 70
      ? code - 55
      : code - 87;

const charCodes = (value) => {
  const codes = [];
  for (let index = 0; index < value.length; index++) codes.push(value.charCodeAt(index));
  return codes;
};

const decodeUtf8 = (bytes) => {
  let result = "";
  for (let index = 0; index < bytes.length;) {
    const first = bytes[index];
    if (first <= 0x7f) {
      result += String.fromCharCode(first);
      index++;
      continue;
    }

    let length;
    let codePoint;
    let secondMinimum = 0x80;
    let secondMaximum = 0xbf;
    if (first >= 0xc2 && first <= 0xdf) {
      length = 2;
      codePoint = first & 0x1f;
    } else if (first >= 0xe0 && first <= 0xef) {
      length = 3;
      codePoint = first & 0x0f;
      if (first === 0xe0) secondMinimum = 0xa0;
      if (first === 0xed) secondMaximum = 0x9f;
    } else if (first >= 0xf0 && first <= 0xf4) {
      length = 4;
      codePoint = first & 0x07;
      if (first === 0xf0) secondMinimum = 0x90;
      if (first === 0xf4) secondMaximum = 0x8f;
    } else {
      result += "\ufffd";
      index++;
      continue;
    }

    const second = bytes[index + 1];
    if (second === undefined || second < secondMinimum || second > secondMaximum) {
      result += "\ufffd";
      index++;
      continue;
    }
    codePoint = (codePoint << 6) | (second & 0x3f);

    let valid = true;
    for (let offset = 2; offset < length; offset++) {
      const next = bytes[index + offset];
      if (next === undefined || next < 0x80 || next > 0xbf) {
        valid = false;
        break;
      }
      codePoint = (codePoint << 6) | (next & 0x3f);
    }
    if (!valid) {
      result += "\ufffd";
      index += length - 1;
      continue;
    }

    if (codePoint <= 0xffff) {
      result += String.fromCharCode(codePoint);
    } else {
      const adjusted = codePoint - 0x10000;
      result += String.fromCharCode(
        0xd800 + (adjusted >> 10),
        0xdc00 + (adjusted & 0x3ff),
      );
    }
    index += length;
  }
  return result;
};

const unescapeBufferString = (value, decodeSpaces = false) => {
  if (typeof value.length !== "number" || typeof value.charCodeAt !== "function") {
    throw new TypeError("querystring.unescape requires a string-like value");
  }
  const bytes = [];
  for (let index = 0; index < value.length;) {
    const code = value.charCodeAt(index);
    if (code === 43 && decodeSpaces) {
      bytes.push(32);
      index++;
      continue;
    }
    if (code === 37 && index + 2 < value.length) {
      const high = value.charCodeAt(index + 1);
      const low = value.charCodeAt(index + 2);
      if (isHex(high) && isHex(low)) {
        bytes.push(hexValue(high) * 16 + hexValue(low));
        index += 3;
        continue;
      }
    }
    bytes.push(code & 0xff);
    index++;
  }
  return decodeUtf8(bytes);
};

const qsUnescape = (value) => {
  try {
    return decodeURIComponent(value);
  } catch {
    return unescapeBufferString(value);
  }
};

const coerceEscapeInput = (value) => {
  if (typeof value === "string") return value;
  if (typeof value === "object") return String(value);
  return value + "";
};

const qsEscape = (value) => {
  const string = coerceEscapeInput(value);
  const hex = "0123456789ABCDEF";
  let result = "";
  const appendByte = (byte) => {
    result += "%" + hex[(byte >> 4) & 15] + hex[byte & 15];
  };
  for (let index = 0; index < string.length; index++) {
    let codePoint = string.charCodeAt(index);
    const safe =
      (codePoint >= 48 && codePoint <= 57) ||
      (codePoint >= 65 && codePoint <= 90) ||
      (codePoint >= 97 && codePoint <= 122) ||
      codePoint === 45 || codePoint === 46 || codePoint === 95 || codePoint === 33 ||
      codePoint === 126 || codePoint === 42 || codePoint === 39 || codePoint === 40 || codePoint === 41;
    if (safe) {
      result += string[index];
    } else if (codePoint < 0x80) {
      appendByte(codePoint);
    } else if (codePoint < 0x800) {
      appendByte(0xc0 | (codePoint >> 6));
      appendByte(0x80 | (codePoint & 0x3f));
    } else {
      if (codePoint >= 0xd800 && codePoint <= 0xdbff && index + 1 < string.length) {
        const low = string.charCodeAt(index + 1);
        if (low >= 0xdc00 && low <= 0xdfff) {
          codePoint = 0x10000 + ((codePoint - 0xd800) << 10) + (low - 0xdc00);
          index++;
        }
      }
      if (codePoint > 0xffff) {
        appendByte(0xf0 | (codePoint >> 18));
        appendByte(0x80 | ((codePoint >> 12) & 0x3f));
        appendByte(0x80 | ((codePoint >> 6) & 0x3f));
        appendByte(0x80 | (codePoint & 0x3f));
      } else {
        appendByte(0xe0 | (codePoint >> 12));
        appendByte(0x80 | ((codePoint >> 6) & 0x3f));
        appendByte(0x80 | (codePoint & 0x3f));
      }
    }
  }
  return result;
};

const unescape = qsUnescape;
export { qsEscape as escape, unescape };

const stringifyValue = (value) => {
  if (typeof value === "string") return value;
  if (typeof value === "number") return Number.isFinite(value) ? value + "" : "";
  if (typeof value === "bigint" || typeof value === "boolean") return value + "";
  return "";
};

const convertStringified = (value, encode) => encode(stringifyValue(value));

export const stringify = (object, sep, eq, options) => {
  if (!sep) sep = "&";
  if (!eq) eq = "=";

  const encode = options && typeof options.encodeURIComponent === "function"
    ? options.encodeURIComponent
    : querystring.escape;

  if (object === null || typeof object !== "object") return "";

  const keys = Object.keys(object);
  let fields = "";
  for (let keyIndex = 0; keyIndex < keys.length; keyIndex++) {
    const key = keys[keyIndex];
    const keyPart = encode(key) + eq;
    const value = object[key];
    if (Array.isArray(value)) {
      if (value.length === 0) continue;
      for (let index = 0; index < value.length; index++) {
        if (fields) fields += sep;
        fields += keyPart + convertStringified(value[index], encode);
      }
    } else {
      if (fields) fields += sep;
      fields += keyPart + convertStringified(value, encode);
    }
  }
  return fields;
};

const addKeyValue = (object, key, value, keyEncoded, valueEncoded, decode) => {
  if (keyEncoded) key = decode(key);
  if (valueEncoded) value = decode(value);

  const previous = object[key];
  if (previous === undefined) {
    object[key] = value;
  } else if (Array.isArray(previous)) {
    previous.push(value);
  } else {
    object[key] = [previous, value];
  }
};

export const parse = (query, sep, eq, options) => {
  const object = Object.create(null);
  if (typeof query !== "string" || query.length === 0) return object;

  const sepCodes = !sep ? [38] : charCodes(String(sep));
  const eqCodes = !eq ? [61] : charCodes(String(eq));
  const sepLength = sepCodes.length;
  const eqLength = eqCodes.length;

  let pairs = 1000;
  if (options && typeof options.maxKeys === "number") {
    pairs = options.maxKeys > 0 ? options.maxKeys : -1;
  }

  let decoder = qsUnescape;
  if (options && typeof options.decodeURIComponent === "function") {
    decoder = options.decodeURIComponent;
  }
  const customDecode = decoder !== qsUnescape;

  let lastPosition = 0;
  let separatorIndex = 0;
  let equalsIndex = 0;
  let key = "";
  let value = "";
  let keyEncoded = customDecode;
  let valueEncoded = customDecode;
  const plusCharacter = customDecode ? "%20" : " ";
  let encodeCheck = 0;

  for (let index = 0; index < query.length; index++) {
    const code = query.charCodeAt(index);

    if (code === sepCodes[separatorIndex]) {
      separatorIndex++;
      if (separatorIndex === sepLength) {
        const end = index - separatorIndex + 1;
        if (equalsIndex < eqLength) {
          if (lastPosition < end) {
            key += query.slice(lastPosition, end);
          } else if (key.length === 0) {
            pairs -= 1;
            if (pairs === 0) return object;
            lastPosition = index + 1;
            separatorIndex = 0;
            equalsIndex = 0;
            continue;
          }
        } else if (lastPosition < end) {
          value += query.slice(lastPosition, end);
        }

        addKeyValue(object, key, value, keyEncoded, valueEncoded, decoder);
        pairs -= 1;
        if (pairs === 0) return object;
        keyEncoded = customDecode;
        valueEncoded = customDecode;
        key = "";
        value = "";
        encodeCheck = 0;
        lastPosition = index + 1;
        separatorIndex = 0;
        equalsIndex = 0;
      }
    } else {
      separatorIndex = 0;
      if (equalsIndex < eqLength) {
        if (code === eqCodes[equalsIndex]) {
          equalsIndex++;
          if (equalsIndex === eqLength) {
            const end = index - equalsIndex + 1;
            if (lastPosition < end) key += query.slice(lastPosition, end);
            encodeCheck = 0;
            lastPosition = index + 1;
          }
          continue;
        }
        equalsIndex = 0;
        if (!keyEncoded) {
          if (code === 37) {
            encodeCheck = 1;
            continue;
          } else if (encodeCheck > 0) {
            if (isHex(code)) {
              encodeCheck += 1;
              if (encodeCheck === 3) keyEncoded = true;
              continue;
            }
            encodeCheck = 0;
          }
        }
        if (code === 43) {
          if (lastPosition < index) key += query.slice(lastPosition, index);
          key += plusCharacter;
          lastPosition = index + 1;
          continue;
        }
      }
      if (code === 43) {
        if (lastPosition < index) value += query.slice(lastPosition, index);
        value += plusCharacter;
        lastPosition = index + 1;
      } else if (!valueEncoded) {
        if (code === 37) {
          encodeCheck = 1;
        } else if (encodeCheck > 0) {
          if (isHex(code)) {
            encodeCheck += 1;
            if (encodeCheck === 3) valueEncoded = true;
          } else {
            encodeCheck = 0;
          }
        }
      }
    }
  }

  if (lastPosition < query.length) {
    if (equalsIndex < eqLength) key += query.slice(lastPosition);
    else if (separatorIndex < sepLength) value += query.slice(lastPosition);
  } else if (equalsIndex === 0 && key.length === 0) {
    return object;
  }

  addKeyValue(object, key, value, keyEncoded, valueEncoded, decoder);
  return object;
};

export const encode = stringify;
export const decode = parse;

const querystring = {
  unescape,
  escape: qsEscape,
  stringify,
  encode,
  parse,
  decode,
};

export default querystring;
