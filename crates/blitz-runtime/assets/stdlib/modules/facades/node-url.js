import path from "node:path";

const URLConstructor = globalThis.URL;
const URLSearchParamsConstructor = globalThis.URLSearchParams;

export const URL = URLConstructor;
export const URLSearchParams = URLSearchParamsConstructor;

const codedError = (code, message, ErrorConstructor = TypeError) => {
  const error = new ErrorConstructor(message);
  error.code = code;
  return error;
};

const requireURL = () => {
  if (typeof URLConstructor !== "function") {
    throw codedError("ERR_NOT_IMPLEMENTED", "node:url requires the global URL constructor");
  }
  return URLConstructor;
};

const asURL = (value) => {
  const Constructor = requireURL();
  if (value !== null && typeof value === "object" && typeof value.href === "string") {
    return new Constructor(value.href);
  }
  return new Constructor(value);
};

const optionsWindows = (options) => {
  if (options === undefined) return globalThis.process?.platform === "win32";
  if (options === null || typeof options !== "object") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The options argument must be an object");
  }
  if (options.windows !== undefined && typeof options.windows !== "boolean") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The options.windows option must be a boolean");
  }
  return options.windows === undefined
    ? globalThis.process?.platform === "win32"
    : options.windows;
};

const decodeFilePath = (pathname, windows) => {
  // Encoded separators are ambiguous on both platforms and Node rejects them
  // before decoding. Check the URL spelling rather than the decoded pathname.
  const separatorPattern = windows ? /%2f|%5c/i : /%2f/i;
  if (separatorPattern.test(pathname)) {
    throw codedError(
      "ERR_INVALID_FILE_URL_PATH",
      windows
        ? "File URL path must not include encoded \\ or / characters"
        : "File URL path must not include encoded / characters",
    );
  }
  return decodeURIComponent(pathname);
};

export const fileURLToPath = (value, options) => {
  const windows = optionsWindows(options);
  const parsed = typeof value === "string" ? new URLConstructor(value) : value;
  if (parsed === null || typeof parsed !== "object" || parsed.protocol !== "file:") {
    throw codedError("ERR_INVALID_URL_SCHEME", "The URL must use the file: scheme");
  }
  const hostname = parsed.hostname || "";
  const pathname = typeof parsed.pathname === "string" ? parsed.pathname : "";
  const separatorPattern = windows ? /%2f|%5c/i : /%2f/i;
  if (separatorPattern.test(pathname)) {
    throw codedError("ERR_INVALID_FILE_URL_PATH", "File URL path must not include encoded separators");
  }
  const decodedPath = decodeURIComponent(pathname);
  if (!windows) {
    if (hostname !== "" && hostname.toLowerCase() !== "localhost") {
      throw codedError("ERR_INVALID_FILE_URL_HOST", "File URL host must be localhost or empty");
    }
    return decodedPath || "/";
  }
  const windowsPath = decodedPath.split("/").join("\\");
  if (hostname !== "" && hostname.toLowerCase() !== "localhost") {
    return `\\\\${hostname}${windowsPath || "\\"}`;
  }
  if (!/^\/[A-Za-z]:\//.test(decodedPath)) {
    throw codedError("ERR_INVALID_FILE_URL_PATH", "File URL path must be absolute");
  }
  return windowsPath.slice(1);
};

const encodePathSegment = (segment) => encodeURIComponent(segment)
  .replace(/%24|%26|%2B|%2C|%3B|%3D|%3A/gi, match => ({
    "%24": "$",
    "%26": "&",
    "%2B": "+",
    "%2C": ",",
    "%3B": ";",
    "%3D": "=",
    "%3A": ":",
  })[match.toUpperCase()]);

const encodeFilePath = (value) => value.split("/").map(encodePathSegment).join("/");

export const pathToFileURL = (value, options) => {
  if (typeof value !== "string") {
    throw codedError("ERR_INVALID_ARG_TYPE", "The path argument must be a string");
  }
  const windows = optionsWindows(options);
  const hostWindows = globalThis.process?.platform === "win32";
  const isWindowsAbsolute = /^(?:[A-Za-z]:[\\/]|\\\\|\/\/)/.test(value);

  // When the Node 24 windows override is used on a POSIX host, relative paths
  // still resolve against the host cwd. The win32 path parser is used for
  // already-absolute drive and UNC paths.
  const resolver = windows && !hostWindows && !isWindowsAbsolute
    ? path.posix
    : windows
      ? path.win32
      : path.posix;
  if (windows && !hostWindows && value.startsWith("/") && !/^\/[A-Za-z]:\//.test(value) && !value.startsWith("//")) {
    throw codedError("ERR_INVALID_FILE_URL_PATH", "Windows file paths must be absolute");
  }
  const resolved = resolver.resolve(value);
  let pathname = windows ? resolved.split("\\").join("/") : resolved;
  let hostname = "";

  if (windows && pathname.startsWith("//")) {
    const separator = pathname.indexOf("/", 2);
    if (separator === -1) {
      hostname = pathname.slice(2);
      pathname = "/";
    } else {
      hostname = pathname.slice(2, separator);
      pathname = pathname.slice(separator);
    }
  } else if (/^[A-Za-z]:\//.test(pathname)) {
    pathname = `/${pathname}`;
  } else if (!pathname.startsWith("/")) {
    pathname = `/${pathname}`;
  }

  const href = `file://${hostname}${encodeFilePath(pathname)}`;
  return new (requireURL())(href);
};

const codePointArray = (value) => {
  const points = [];
  for (let index = 0; index < value.length; index++) {
    const point = value.codePointAt(index);
    points.push(point);
    if (point > 0xffff) index++;
  }
  return points;
};

const punycodeDigit = value => value < 26 ? String.fromCharCode(97 + value) : String.fromCharCode(48 + value - 26);
const punycodeValue = value => value >= 48 && value <= 57 ? value - 22 : value >= 65 && value <= 90 ? value - 65 : value >= 97 && value <= 122 ? value - 97 : -1;

const adaptBias = (delta, length, first) => {
  delta = first ? Math.floor(delta / 700) : Math.floor(delta / 2);
  delta += Math.floor(delta / length);
  let k = 0;
  while (delta > 455) {
    delta = Math.floor(delta / 35);
    k += 36;
  }
  return k + Math.floor((36 * delta) / (delta + 38));
};

const encodePunycode = (value) => {
  const points = codePointArray(value);
  const basic = points.filter(point => point < 0x80);
  let output = basic.map(point => String.fromCharCode(point).toLowerCase()).join("");
  const basicCount = basic.length;
  let handled = basicCount;
  if (handled !== 0) output += "-";
  let n = 128;
  let delta = 0;
  let bias = 72;
  while (handled < points.length) {
    let next = Infinity;
    for (const point of points) if (point >= n && point < next) next = point;
    if (next === Infinity) return "";
    delta += (next - n) * (handled + 1);
    n = next;
    for (const point of points) {
      if (point < n) delta++;
      if (point !== n) continue;
      let q = delta;
      for (let k = 36;; k += 36) {
        const threshold = k <= bias ? 1 : k >= bias + 26 ? 26 : k - bias;
        if (q < threshold) break;
        output += punycodeDigit(threshold + ((q - threshold) % (36 - threshold)));
        q = Math.floor((q - threshold) / (36 - threshold));
      }
      output += punycodeDigit(q);
      bias = adaptBias(delta, handled + 1, handled === basicCount);
      delta = 0;
      handled++;
    }
    delta++;
    n++;
  }
  return output;
};

const decodePunycode = (value) => {
  const basicEnd = value.lastIndexOf("-");
  const output = basicEnd < 0 ? [] : codePointArray(value.slice(0, basicEnd));
  let inputIndex = basicEnd < 0 ? 0 : basicEnd + 1;
  let insertionIndex = 0;
  let n = 128;
  let bias = 72;
  while (inputIndex < value.length) {
    const oldIndex = insertionIndex;
    let weight = 1;
    let digit;
    for (let k = 36;; k += 36) {
      if (inputIndex >= value.length || (digit = punycodeValue(value.charCodeAt(inputIndex++))) < 0) return "";
      insertionIndex += digit * weight;
      const threshold = k <= bias ? 1 : k >= bias + 26 ? 26 : k - bias;
      if (digit < threshold) {
        const outputLength = output.length + 1;
        bias = adaptBias(insertionIndex - oldIndex, outputLength, oldIndex === 0);
        n += Math.floor(insertionIndex / outputLength);
        insertionIndex %= outputLength;
        output.splice(insertionIndex, 0, n);
        insertionIndex++;
        n++;
        break;
      }
      weight *= 36 - threshold;
      if (weight > 0x7fffffff) return "";
    }
  }
  return output.map(point => String.fromCodePoint(point)).join("");
};

const asciiDomain = (value) => {
  if (typeof value !== "string" || value.length === 0 || value !== value.trim()) return "";
  if (value.startsWith("[") && value[value.length - 1] === "]") return value.toLowerCase();
  if (value.indexOf(":") !== -1) return "";
  const Constructor = requireURL();
  let hostname;
  try {
    hostname = new Constructor(`http://${value}`).hostname;
  } catch {
    return "";
  }
  if (typeof hostname !== "string" || hostname.length === 0) return "";
  return hostname.split(".").map(label => {
    if (/^[\x00-\x7f]*$/.test(label)) return label.toLowerCase();
    return `xn--${encodePunycode(label)}`;
  }).join(".");
};

export const domainToASCII = asciiDomain;

export const domainToUnicode = (value) => {
  const ascii = asciiDomain(value);
  if (ascii === "") return "";
  return ascii.split(".").map(label => label.toLowerCase().startsWith("xn--")
    ? decodePunycode(label.slice(4))
    : label).join(".");
};

const decodeAuth = value => {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
};

const parseQuery = (value) => {
  const result = Object.create(null);
  if (value === "") return result;
  for (const field of value.split("&")) {
    const separator = field.indexOf("=");
    const key = separator < 0 ? field : field.slice(0, separator);
    const item = separator < 0 ? "" : field.slice(separator + 1);
    const decode = part => decodeAuth(part.split("+").join(" "));
    const decodedKey = decode(key);
    const decodedValue = decode(item);
    if (result[decodedKey] === undefined) result[decodedKey] = decodedValue;
    else if (Array.isArray(result[decodedKey])) result[decodedKey].push(decodedValue);
    else result[decodedKey] = [result[decodedKey], decodedValue];
  }
  return result;
};

const legacyParts = (input, parseQueryString) => {
  let value = input;
  let protocol = null;
  const protocolMatch = /^([A-Za-z][A-Za-z\d+.-]*:)/.exec(value);
  if (protocolMatch) {
    protocol = protocolMatch[1].toLowerCase();
    value = value.slice(protocolMatch[1].length);
  }
  const slashes = value.startsWith("//");
  if (slashes) value = value.slice(2);
  let authorityEnd = -1;
  for (let index = 0; index < value.length; index++) {
    if (value[index] === "/" || value[index] === "?" || value[index] === "#") {
      authorityEnd = index;
      break;
    }
  }
  const authority = slashes || protocol === "http:" || protocol === "https:" || protocol === "ftp:"
    ? authorityEnd < 0 ? value : value.slice(0, authorityEnd)
    : "";
  if (authority) value = value.slice(authority.length);
  const at = authority.lastIndexOf("@");
  const auth = at < 0 ? null : decodeAuth(authority.slice(0, at));
  let host = at < 0 ? authority : authority.slice(at + 1);
  let hostname = host;
  let port = null;
  if (hostname.startsWith("[") && hostname.indexOf("]") !== -1) {
    const end = hostname.indexOf("]");
    const rest = hostname.slice(end + 1);
    hostname = hostname.slice(0, end + 1);
    if (rest.startsWith(":")) port = rest.slice(1) || null;
  } else {
    const colon = hostname.lastIndexOf(":");
    if (colon >= 0 && /^\d*$/.test(hostname.slice(colon + 1))) {
      port = hostname.slice(colon + 1) || null;
      hostname = hostname.slice(0, colon);
    }
  }
  if (host === "") host = null;
  const hashIndex = value.indexOf("#");
  const hash = hashIndex < 0 ? null : value.slice(hashIndex);
  if (hashIndex >= 0) value = value.slice(0, hashIndex);
  const queryIndex = value.indexOf("?");
  const search = queryIndex < 0 ? null : value.slice(queryIndex);
  const rawQuery = queryIndex < 0 ? "" : value.slice(queryIndex + 1);
  const pathname = queryIndex < 0 ? value : value.slice(0, queryIndex);
  const query = parseQueryString ? parseQuery(rawQuery) : search === null ? null : rawQuery;
  const pathValue = pathname || search === null ? pathname || null : search;
  return {
    protocol,
    slashes: slashes ? true : null,
    auth,
    host,
    port,
    hostname: hostname || null,
    hash,
    search,
    query,
    pathname: pathname || null,
    path: pathValue,
    href: input,
  };
};

export const parse = (value, parseQueryString = false) => {
  if (typeof value !== "string") throw codedError("ERR_INVALID_ARG_TYPE", "The url argument must be a string");
  const parts = legacyParts(value, parseQueryString);
  const Constructor = URLConstructor;
  if (typeof Constructor === "function") {
    try {
      const parsed = new Constructor(value);
      parts.protocol = parsed.protocol || parts.protocol;
      parts.auth = parsed.username || parsed.password ? `${decodeAuth(parsed.username)}${parsed.password ? `:${decodeAuth(parsed.password)}` : ""}` : parts.auth;
      parts.hostname = parsed.hostname || parts.hostname;
      parts.port = parsed.port || parts.port;
      parts.host = parsed.host || parts.host;
      parts.hash = parsed.hash || parts.hash;
      parts.search = parsed.search || parts.search;
      parts.query = parseQueryString ? parseQuery(parsed.search ? parsed.search.slice(1) : "") : parsed.search ? parsed.search.slice(1) : parts.query;
      parts.pathname = parsed.pathname || parts.pathname;
      parts.path = `${parts.pathname || ""}${parts.search || ""}` || null;
      parts.href = parsed.href;
      parts.slashes = parts.slashes || parsed.host !== "" ? true : parts.slashes;
    } catch {
      // Legacy url.parse accepts relative and otherwise non-WHATWG forms.
    }
  }
  return parts;
};

export const format = (value) => {
  if (typeof value === "string") return value;
  if (value === null || typeof value !== "object") throw codedError("ERR_INVALID_ARG_TYPE", "The url argument must be a string or object");
  if (typeof value.href === "string" && value.protocol === undefined) return value.href;
  let protocol = value.protocol || "";
  if (protocol && protocol[protocol.length - 1] !== ":") protocol += ":";
  let auth = value.auth || "";
  if (auth) auth = encodeURIComponent(auth).replace(/%3A/gi, ":");
  let host = value.host || value.hostname || "";
  if (value.hostname && value.host === undefined && value.hostname.indexOf(":") !== -1 && !value.hostname.startsWith("[")) host = `[${value.hostname}]`;
  if (value.port && value.host === undefined) host += `:${value.port}`;
  const slashes = value.slashes || host !== "" || protocol === "http:" || protocol === "https:" || protocol === "ftp:";
  const pathname = value.pathname || "";
  let search = value.search || "";
  if (!search && typeof value.query === "string" && value.query !== "") search = `?${value.query}`;
  if (search && !search.startsWith("?")) search = `?${search}`;
  const hash = value.hash || "";
  return `${protocol}${slashes ? "//" : ""}${auth ? `${auth}@` : ""}${host}${pathname}${search}${hash}`;
};

export const resolve = (from, to) => {
  if (typeof from !== "string" || typeof to !== "string") throw codedError("ERR_INVALID_ARG_TYPE", "The from and to arguments must be strings");
  const Constructor = requireURL();
  return new Constructor(to, from).href;
};

export const resolveObject = (from, to) => parse(resolve(from, to));

export const urlToHttpOptions = (value) => {
  const parsed = asURL(value);
  const options = Object.create(null);
  options.protocol = parsed.protocol;
  options.hostname = (parsed.hostname || "").replace(/^\[|\]$/g, "");
  options.hash = parsed.hash || "";
  options.search = parsed.search || "";
  options.pathname = parsed.pathname || "";
  options.path = `${options.pathname}${options.search}`;
  options.href = parsed.href;
  if (parsed.port) options.port = Number(parsed.port);
  if (parsed.username || parsed.password) options.auth = `${decodeAuth(parsed.username || "")}${parsed.password ? `:${decodeAuth(parsed.password)}` : ""}`;
  return options;
};

const url = Object.freeze({
  URL,
  URLSearchParams,
  domainToASCII,
  domainToUnicode,
  fileURLToPath,
  pathToFileURL,
  urlToHttpOptions,
  format,
  parse,
  resolve,
  resolveObject,
});

export default url;
