const isUint8Array = value => typeof Uint8Array === "function" && value instanceof Uint8Array;
const asyncIterator = typeof Symbol === "function" ? Symbol.asyncIterator : undefined;

const invalidStream = value => {
  const error = new TypeError(`The "stream" argument must be an async iterable or readable stream. Received ${value === null ? "null" : typeof value}`);
  error.code = "ERR_INVALID_ARG_TYPE";
  return error;
};

const encodeUtf8 = value => {
  const bytes = [];
  for (let index = 0; index < value.length; index += 1) {
    let codePoint = value.charCodeAt(index);
    if (codePoint >= 0xd800 && codePoint <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        codePoint = 0x10000 + ((codePoint - 0xd800) << 10) + (next - 0xdc00);
        index += 1;
      } else {
        codePoint = 0xfffd;
      }
    } else if (codePoint >= 0xdc00 && codePoint <= 0xdfff) {
      codePoint = 0xfffd;
    }
    if (codePoint <= 0x7f) bytes.push(codePoint);
    else if (codePoint <= 0x7ff) {
      bytes.push(0xc0 | (codePoint >> 6), 0x80 | (codePoint & 0x3f));
    } else if (codePoint <= 0xffff) {
      bytes.push(0xe0 | (codePoint >> 12), 0x80 | ((codePoint >> 6) & 0x3f), 0x80 | (codePoint & 0x3f));
    } else {
      bytes.push(0xf0 | (codePoint >> 18), 0x80 | ((codePoint >> 12) & 0x3f), 0x80 | ((codePoint >> 6) & 0x3f), 0x80 | (codePoint & 0x3f));
    }
  }
  return new Uint8Array(bytes);
};

const copyBytes = value => {
  if (typeof value === "string") return encodeUtf8(value);
  if (!isUint8Array(value)) throw new TypeError("Stream chunks must be strings or Uint8Array values");
  const output = new Uint8Array(value.length);
  for (let index = 0; index < output.length; index += 1) output[index] = value[index];
  return output;
};

const joinBytes = chunks => {
  const byteChunks = chunks.map(copyBytes);
  let length = 0;
  for (const chunk of byteChunks) length += chunk.length;
  const output = new Uint8Array(length);
  let offset = 0;
  for (const chunk of byteChunks) {
    for (let index = 0; index < chunk.length; index += 1) output[offset + index] = chunk[index];
    offset += chunk.length;
  }
  return output;
};

const decodeUtf8 = bytes => {
  let output = "";
  let index = 0;
  while (index < bytes.length) {
    const first = bytes[index];
    if (first <= 0x7f) {
      output += String.fromCharCode(first);
      index += 1;
      continue;
    }
    let needed;
    let codePoint;
    if (first >= 0xc2 && first <= 0xdf) {
      needed = 1;
      codePoint = first & 0x1f;
    } else if (first >= 0xe0 && first <= 0xef) {
      needed = 2;
      codePoint = first & 0x0f;
    } else if (first >= 0xf0 && first <= 0xf4) {
      needed = 3;
      codePoint = first & 0x07;
    } else {
      output += "\ufffd";
      index += 1;
      continue;
    }
    if (index + needed >= bytes.length) {
      output += "\ufffd";
      index += 1;
      continue;
    }
    let valid = true;
    for (let offset = 1; offset <= needed; offset += 1) {
      const continuation = bytes[index + offset];
      if ((continuation & 0xc0) !== 0x80) {
        valid = false;
        break;
      }
      codePoint = (codePoint << 6) | (continuation & 0x3f);
    }
    if (!valid || (needed === 2 && codePoint < 0x800) || (needed === 3 && codePoint < 0x10000) || (codePoint >= 0xd800 && codePoint <= 0xdfff) || codePoint > 0x10ffff) {
      output += "\ufffd";
      index += 1;
      continue;
    }
    if (codePoint <= 0xffff) output += String.fromCharCode(codePoint);
    else {
      const adjusted = codePoint - 0x10000;
      output += String.fromCharCode(0xd800 + (adjusted >> 10), 0xdc00 + (adjusted & 0x3ff));
    }
    index += needed + 1;
  }
  return output;
};

const iteratorFor = stream => {
  if (stream !== null && stream !== undefined && asyncIterator !== undefined && typeof stream[asyncIterator] === "function") {
    return stream[asyncIterator]();
  }
  throw invalidStream(stream);
};

const collect = stream => new Promise((resolve, reject) => {
  let iterator;
  try {
    iterator = iteratorFor(stream);
  } catch (error) {
    reject(error);
    return;
  }
  const chunks = [];
  const step = () => {
    let next;
    try {
      next = iterator.next();
    } catch (error) {
      reject(error);
      return;
    }
    Promise.resolve(next).then(result => {
      if (result.done) {
        resolve(chunks);
        return;
      }
      chunks.push(result.value);
      step();
    }, reject);
  };
  step();
});

export const text = stream => collect(stream).then(chunks => {
  let hasBytes = false;
  for (const chunk of chunks) if (typeof chunk !== "string") hasBytes = true;
  if (!hasBytes) {
    let output = "";
    for (const chunk of chunks) output += chunk;
    return output;
  }
  return decodeUtf8(joinBytes(chunks));
});

const parseJson = source => {
  let index = 0;
  const syntaxError = message => {
    return new SyntaxError(`${message} at position ${index}`);
  };
  const skipWhitespace = () => {
    while (index < source.length) {
      const code = source.charCodeAt(index);
      if (code !== 0x20 && code !== 0x09 && code !== 0x0a && code !== 0x0d) return;
      index += 1;
    }
  };
  const parseString = () => {
    if (source.charCodeAt(index) !== 0x22) throw syntaxError("Expected a JSON string");
    index += 1;
    let output = "";
    while (index < source.length) {
      const code = source.charCodeAt(index);
      if (code === 0x22) {
        index += 1;
        return output;
      }
      if (code === 0x5c) {
        index += 1;
        if (index >= source.length) throw syntaxError("Unterminated JSON escape");
        const escape = source[index];
        if (escape === '"' || escape === "\\" || escape === "/") output += escape;
        else if (escape === "b") output += "\b";
        else if (escape === "f") output += "\f";
        else if (escape === "n") output += "\n";
        else if (escape === "r") output += "\r";
        else if (escape === "t") output += "\t";
        else if (escape === "u") {
          const digits = source.slice(index + 1, index + 5);
          if (digits.length !== 4 || !/^[0-9a-fA-F]{4}$/.test(digits)) throw syntaxError("Invalid Unicode escape");
          output += String.fromCharCode(parseInt(digits, 16));
          index += 4;
        } else throw syntaxError("Invalid JSON escape");
        index += 1;
        continue;
      }
      if (code < 0x20) throw syntaxError("Invalid control character in JSON string");
      output += source[index];
      index += 1;
    }
    throw syntaxError("Unterminated JSON string");
  };
  const parseValue = () => {
    skipWhitespace();
    const character = source[index];
    if (character === '"') return parseString();
    if (character === "{") {
      index += 1;
      const object = {};
      skipWhitespace();
      if (source[index] === "}") { index += 1; return object; }
      while (true) {
        skipWhitespace();
        const key = parseString();
        skipWhitespace();
        if (source[index] !== ":") throw syntaxError("Expected ':' after JSON object key");
        index += 1;
        object[key] = parseValue();
        skipWhitespace();
        if (source[index] === "}") { index += 1; return object; }
        if (source[index] !== ",") throw syntaxError("Expected ',' in JSON object");
        index += 1;
      }
    }
    if (character === "[") {
      index += 1;
      const array = [];
      skipWhitespace();
      if (source[index] === "]") { index += 1; return array; }
      while (true) {
        array.push(parseValue());
        skipWhitespace();
        if (source[index] === "]") { index += 1; return array; }
        if (source[index] !== ",") throw syntaxError("Expected ',' in JSON array");
        index += 1;
      }
    }
    if (source.slice(index, index + 4) === "true") { index += 4; return true; }
    if (source.slice(index, index + 5) === "false") { index += 5; return false; }
    if (source.slice(index, index + 4) === "null") { index += 4; return null; }
    const start = index;
    if (source[index] === "-") index += 1;
    if (source[index] === "0") index += 1;
    else if (source[index] >= "1" && source[index] <= "9") {
      while (source[index] >= "0" && source[index] <= "9") index += 1;
    } else throw syntaxError("Invalid JSON number");
    if (source[index] === ".") {
      index += 1;
      const fractionStart = index;
      while (source[index] >= "0" && source[index] <= "9") index += 1;
      if (fractionStart === index) throw syntaxError("Invalid JSON number");
    }
    if (source[index] === "e" || source[index] === "E") {
      index += 1;
      if (source[index] === "+" || source[index] === "-") index += 1;
      const exponentStart = index;
      while (source[index] >= "0" && source[index] <= "9") index += 1;
      if (exponentStart === index) throw syntaxError("Invalid JSON number");
    }
    if (start === index) throw syntaxError("Unexpected JSON token");
    const number = Number(source.slice(start, index));
    if (number !== number) throw syntaxError("Invalid JSON number");
    return number;
  };
  const value = parseValue();
  skipWhitespace();
  if (index !== source.length) throw syntaxError("Unexpected JSON token");
  return value;
};

export const json = stream => text(stream).then(parseJson);

// ArrayBuffer is constructible in Blitz, but its current typed-array surface
// does not provide usable byte-view aliasing. Do not return an ArrayBuffer
// whose contents cannot be observed faithfully; Buffer/bytes/blob are omitted.
const consumers = { json, text };

export { consumers as default };
