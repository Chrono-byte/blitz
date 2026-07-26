// Node 24's deprecated node:punycode compatibility facade.
// This is the bundled Punycode.js 2.1.0 implementation, kept self-contained
// so it does not depend on Node, URL, or any host primitive.
const maxInt = 0x7fffffff;
const base = 36;
const tMin = 1;
const tMax = 26;
const skew = 38;
const damp = 700;
const initialBias = 72;
const initialN = 128;
const delimiter = "-";

const floor = value => Math.floor(value);
const baseMinusTMin = base - tMin;

const error = type => {
  const messages = {
    overflow: "Overflow: input needs wider integers to process",
    "not-basic": "Illegal input >= 0x80 (not a basic code point)",
    "invalid-input": "Invalid input",
  };
  throw new RangeError(messages[type]);
};

const map = (array, callback) => {
  const result = [];
  let length = array.length;
  while (length--) result[length] = callback(array[length]);
  return result;
};

const mapDomain = (domain, callback) => {
  const parts = domain.split("@");
  let result = "";
  let currentDomain = domain;
  if (parts.length > 1) {
    result = parts[0] + "@";
    currentDomain = parts[1];
  }
  let normalized = "";
  for (let index = 0; index < currentDomain.length; index++) {
    const code = currentDomain.charCodeAt(index);
    normalized += code === 0x2e || code === 0x3002 || code === 0xff0e || code === 0xff61
      ? "."
      : currentDomain[index];
  }
  return result + map(normalized.split("."), callback).join(".");
};

const ucs2decode = string => {
  const output = [];
  let counter = 0;
  const length = string.length;
  if (typeof length !== "number" || length <= 0) return output;
  let value = string.charCodeAt(counter);
  while (counter < length || value === value) {
    counter += 1;
    const highSurrogate = value >= 0xd800 && value <= 0xdbff;
    const hasExtra = counter < length || string.charCodeAt(counter) === string.charCodeAt(counter);
    if (highSurrogate && hasExtra) {
      const extra = string.charCodeAt(counter);
      counter += 1;
      if ((extra & 0xfc00) === 0xdc00) {
        output.push(((value & 0x3ff) << 10) + (extra & 0x3ff) + 0x10000);
      } else {
        output.push(value);
        counter -= 1;
      }
    } else {
      output.push(value);
    }
    value = string.charCodeAt(counter);
  }
  return output;
};

// Calling fromCodePoint one item at a time avoids apply/spread argument limits
// while retaining the native RangeError behavior for invalid code points.
const ucs2encode = codePoints => {
  let result = "";
  for (const codePoint of codePoints) result += String.fromCodePoint(codePoint);
  return result;
};

const basicToDigit = codePoint => {
  if (codePoint >= 0x30 && codePoint < 0x3a) return 26 + codePoint - 0x30;
  if (codePoint >= 0x41 && codePoint < 0x5b) return codePoint - 0x41;
  if (codePoint >= 0x61 && codePoint < 0x7b) return codePoint - 0x61;
  return base;
};

const digitToBasic = (digit, flag) =>
  digit + 22 + 75 * (digit < 26) - ((flag !== 0) << 5);

const adapt = (delta, numPoints, firstTime) => {
  let k = 0;
  let adjustedDelta = firstTime ? floor(delta / damp) : delta >> 1;
  adjustedDelta += floor(adjustedDelta / numPoints);
  while (adjustedDelta > (baseMinusTMin * tMax) >> 1) {
    adjustedDelta = floor(adjustedDelta / baseMinusTMin);
    k += base;
  }
  return floor(k + ((baseMinusTMin + 1) * adjustedDelta) / (adjustedDelta + skew));
};

export const decode = input => {
  const output = [];
  const inputLength = input.length;
  let i = 0;
  let n = initialN;
  let bias = initialBias;
  let basic = -1;
  for (let index = 0; index < input.length; index++) {
    if (input.charCodeAt(index) === 0x2d) basic = index;
  }
  if (basic < 0) basic = 0;

  for (let index = 0; index < basic; index++) {
    if (input.charCodeAt(index) >= 0x80) error("not-basic");
    output.push(input.charCodeAt(index));
  }

  for (let index = basic > 0 ? basic + 1 : 0; index < inputLength;) {
    const oldi = i;
    for (let weight = 1, k = base;; k += base) {
      if (index >= inputLength) error("invalid-input");
      const digit = basicToDigit(input.charCodeAt(index++));
      if (digit >= base) error("invalid-input");
      if (digit > floor((maxInt - i) / weight)) error("overflow");
      i += digit * weight;
      const threshold = k <= bias ? tMin : k >= bias + tMax ? tMax : k - bias;
      if (digit < threshold) break;
      const baseMinusThreshold = base - threshold;
      if (weight > floor(maxInt / baseMinusThreshold)) error("overflow");
      weight *= baseMinusThreshold;
    }

    const outputLength = output.length + 1;
    bias = adapt(i - oldi, outputLength, oldi === 0);
    if (floor(i / outputLength) > maxInt - n) error("overflow");
    n += floor(i / outputLength);
    i %= outputLength;
    output.splice(i, 0, n);
    i += 1;
  }

  return ucs2encode(output);
};

export const encode = input => {
  const codePoints = ucs2decode(input);
  const output = [];
  const inputLength = codePoints.length;
  let n = initialN;
  let delta = 0;
  let bias = initialBias;

  for (let pointIndex = 0; pointIndex < codePoints.length; pointIndex++) {
    const codePoint = codePoints[pointIndex];
    if (codePoint < 0x80) output.push(String.fromCharCode(codePoint));
  }

  const basicLength = output.length;
  let handledCodePoints = basicLength;
  if (basicLength) output.push(delimiter);

  while (handledCodePoints < inputLength) {
    let minimum = maxInt;
    for (let pointIndex = 0; pointIndex < codePoints.length; pointIndex++) {
      const codePoint = codePoints[pointIndex];
      if (codePoint >= n && codePoint < minimum) minimum = codePoint;
    }

    const handledPlusOne = handledCodePoints + 1;
    if (minimum - n > floor((maxInt - delta) / handledPlusOne)) error("overflow");
    delta += (minimum - n) * handledPlusOne;
    n = minimum;

    for (let pointIndex = 0; pointIndex < codePoints.length; pointIndex++) {
      const codePoint = codePoints[pointIndex];
      if (codePoint < n) {
        delta += 1;
        if (delta > maxInt) error("overflow");
      }
      if (codePoint !== n) continue;

      let value = delta;
      for (let k = base;; k += base) {
        const threshold = k <= bias ? tMin : k >= bias + tMax ? tMax : k - bias;
        if (value < threshold) break;
        const valueMinusThreshold = value - threshold;
        const baseMinusThreshold = base - threshold;
        output.push(String.fromCharCode(digitToBasic(
          threshold + valueMinusThreshold % baseMinusThreshold,
          0,
        )));
        value = floor(valueMinusThreshold / baseMinusThreshold);
      }
      output.push(String.fromCharCode(digitToBasic(value, 0)));
      bias = adapt(delta, handledPlusOne, handledCodePoints === basicLength);
      delta = 0;
      handledCodePoints += 1;
    }

    delta += 1;
    n += 1;
  }
  return output.join("");
};

export const toUnicode = input => mapDomain(input, string =>
  string.startsWith("xn--") ? decode(string.slice(4).toLowerCase()) : string,
);

const hasNonASCII = string => {
  for (let index = 0; index < string.length; index++) {
    if (string.charCodeAt(index) > 0x7f) return true;
  }
  return false;
};

export const toASCII = input => mapDomain(input, string =>
  hasNonASCII(string) ? "xn--" + encode(string) : string,
);

export const ucs2 = {
  decode: ucs2decode,
  encode: ucs2encode,
};

export const version = "2.1.0";

const punycode = {
  version,
  ucs2,
  decode,
  encode,
  toASCII,
  toUnicode,
};

export default punycode;
