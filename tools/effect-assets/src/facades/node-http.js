import * as Effect from "effect/Effect";
import { EventEmitter } from "node:events";
import { Buffer } from "node:buffer";
import * as Transport from "blitz-internal:http";

const nextTurn = callback => Promise.resolve().then(callback);
const lowerName = name => String(name).toLowerCase();

const copyHeaders = headers => {
  const result = {};
  for (const name in headers) {
    const value = headers[name];
    if (typeof value !== "function") result[lowerName(name)] = value;
  }
  return result;
};

const normalizeHeaderValue = value => {
  if (Array.isArray(value)) return value.map(String).join(", ");
  return String(value);
};

export function IncomingMessage(request, context) {
  EventEmitter.call(this);
  this.method = request.method;
  this.url = request.path + (request.query === null ? "" : "?" + request.query);
  this.headers = copyHeaders(request.headers);
  this.headersDistinct = {};
  this.rawHeaders = [];
  for (const name in this.headers) {
    this.headersDistinct[name] = [this.headers[name]];
    this.rawHeaders.push(name, this.headers[name]);
  }
  this.httpVersion = "1.1";
  this.httpVersionMajor = 1;
  this.httpVersionMinor = 1;
  this.complete = false;
  this.destroyed = false;
  this.readable = true;
  this.socket = {
    remoteAddress: context.remoteAddress.hostname,
    remotePort: context.remoteAddress.port,
    remoteFamily: context.remoteAddress.hostname.indexOf(":") === -1 ? "IPv4" : "IPv6",
    encrypted: false
  };
  this.connection = this.socket;
  this._body = request.body;
}
IncomingMessage.prototype = Object.create(EventEmitter.prototype);
IncomingMessage.prototype.constructor = IncomingMessage;
IncomingMessage.prototype.setTimeout = function(_milliseconds, callback) {
  if (typeof callback === "function") this.once("timeout", callback);
  return this;
};
IncomingMessage.prototype.destroy = function(error) {
  if (this.destroyed) return this;
  this.destroyed = true;
  this.readable = false;
  if (error !== undefined) this.emit("error", error);
  this.emit("close");
  return this;
};
IncomingMessage.prototype._deliverBody = function() {
  if (this.destroyed || this.complete) return;
  if (this._body.length !== 0) this.emit("data", Buffer.from(this._body));
  this.complete = true;
  this.readable = false;
  this.emit("end");
};

export function ServerResponse(resolve) {
  EventEmitter.call(this);
  this.statusCode = 200;
  this.statusMessage = undefined;
  this.headersSent = false;
  this.finished = false;
  this.writableEnded = false;
  this.writableFinished = false;
  this.destroyed = false;
  this.sendDate = true;
  this._headers = {};
  this._chunks = [];
  this._resolve = resolve;
}
ServerResponse.prototype = Object.create(EventEmitter.prototype);
ServerResponse.prototype.constructor = ServerResponse;
ServerResponse.prototype.setHeader = function(name, value) {
  if (this.headersSent) throw new Error("Cannot set headers after they are sent to the client");
  this._headers[lowerName(name)] = normalizeHeaderValue(value);
  return this;
};
ServerResponse.prototype.getHeader = function(name) {
  return this._headers[lowerName(name)];
};
ServerResponse.prototype.getHeaders = function() {
  return { ...this._headers };
};
ServerResponse.prototype.getHeaderNames = function() {
  return Object.keys(this._headers);
};
ServerResponse.prototype.hasHeader = function(name) {
  return Object.prototype.hasOwnProperty.call(this._headers, lowerName(name));
};
ServerResponse.prototype.removeHeader = function(name) {
  if (this.headersSent) throw new Error("Cannot remove headers after they are sent to the client");
  delete this._headers[lowerName(name)];
};
ServerResponse.prototype.writeHead = function(statusCode, statusMessage, headers) {
  this.statusCode = Number(statusCode);
  if (typeof statusMessage === "string") {
    this.statusMessage = statusMessage;
  } else {
    headers = statusMessage;
  }
  if (headers !== undefined) {
    if (Array.isArray(headers)) {
      for (let index = 0; index < headers.length; index += 2) {
        this.setHeader(headers[index], headers[index + 1]);
      }
    } else {
      for (const name of Object.keys(headers)) this.setHeader(name, headers[name]);
    }
  }
  this.headersSent = true;
  return this;
};
ServerResponse.prototype.flushHeaders = function() {
  this.headersSent = true;
};
ServerResponse.prototype.write = function(chunk, encoding, callback) {
  if (this.writableEnded) throw new Error("write after end");
  if (typeof encoding === "function") callback = encoding;
  this.headersSent = true;
  this._chunks.push(typeof chunk === "string" ? chunk : Buffer.from(chunk).toString());
  if (typeof callback === "function") callback();
  return true;
};
ServerResponse.prototype.end = function(chunk, encoding, callback) {
  if (typeof chunk === "function") {
    callback = chunk;
    chunk = undefined;
  } else if (typeof encoding === "function") {
    callback = encoding;
  }
  if (chunk !== undefined && chunk !== null) this.write(chunk);
  if (this.writableEnded) return this;
  this.headersSent = true;
  this.finished = true;
  this.writableEnded = true;
  this.writableFinished = true;
  const response = new Response(this._chunks.join(""), {
    status: this.statusCode,
    headers: this._headers
  });
  this._resolve(response);
  if (typeof callback === "function") callback();
  this.emit("finish");
  return this;
};
ServerResponse.prototype.setTimeout = function(_milliseconds, callback) {
  if (typeof callback === "function") this.once("timeout", callback);
  return this;
};
ServerResponse.prototype.destroy = function(error) {
  if (this.destroyed) return this;
  this.destroyed = true;
  if (error !== undefined) this.emit("error", error);
  this.emit("close");
  return this;
};

const listenOptions = args => {
  if (args[0] !== null && typeof args[0] === "object") {
    return {
      port: args[0].port === undefined ? 0 : Number(args[0].port),
      hostname: args[0].host === undefined ? (args[0].hostname === undefined ? "127.0.0.1" : String(args[0].hostname)) : String(args[0].host)
    };
  }
  return {
    port: Number(args[0]),
    hostname: typeof args[1] === "string" ? args[1] : "127.0.0.1"
  };
};

export function Server(options, requestListener) {
  if (!(this instanceof Server)) return new Server(options, requestListener);
  EventEmitter.call(this);
  if (typeof options === "function") {
    requestListener = options;
    options = undefined;
  }
  this.listening = false;
  this.maxHeadersCount = null;
  this.requestTimeout = options === undefined || options.requestTimeout === undefined ? 300000 : Number(options.requestTimeout);
  this.headersTimeout = options === undefined || options.headersTimeout === undefined ? 60000 : Number(options.headersTimeout);
  this.keepAliveTimeout = options === undefined || options.keepAliveTimeout === undefined ? 5000 : Number(options.keepAliveTimeout);
  this._handle = undefined;
  if (typeof requestListener === "function") this.on("request", requestListener);
}
Server.prototype = Object.create(EventEmitter.prototype);
Server.prototype.constructor = Server;
Server.prototype.listen = function(...args) {
  if (this.listening) throw new Error("Listen method has been called more than once without closing");
  const callback = args.find(value => typeof value === "function");
  const options = listenOptions(args);
  options.handler = (request, context) => new Promise((resolve, reject) => {
    let stage = "incoming request";
    try {
      const incoming = new IncomingMessage(request, context);
      stage = "server response";
      const response = new ServerResponse(resolve);
      stage = "request listener";
      this.emit("request", incoming, response);
      stage = "request body scheduling";
      nextTurn(() => incoming._deliverBody());
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      reject(stage + ": " + message);
    }
  });
  try {
    this._handle = Effect.runSync(Transport.openServer(options));
    this.listening = true;
    nextTurn(() => {
      this.emit("listening");
      if (typeof callback === "function") callback();
    });
  } catch (error) {
    nextTurn(() => this.emit("error", error));
  }
  return this;
};
Server.prototype.close = function(callback) {
  if (!this.listening || this._handle === undefined) {
    const error = new Error("Server is not running");
    error.code = "ERR_SERVER_NOT_RUNNING";
    if (typeof callback === "function") nextTurn(() => callback(error));
    return this;
  }
  Effect.runSync(Transport.closeServer(this._handle));
  this.listening = false;
  this._handle = undefined;
  nextTurn(() => {
    this.emit("close");
    if (typeof callback === "function") callback();
  });
  return this;
};
Server.prototype.address = function() {
  if (!this.listening || this._handle === undefined) return null;
  return {
    address: this._handle.hostname,
    family: this._handle.hostname.includes(":") ? "IPv6" : "IPv4",
    port: this._handle.port
  };
};
Server.prototype.ref = function() {
  if (this._handle !== undefined && typeof this._handle.ref === "function") this._handle.ref();
  return this;
};
Server.prototype.unref = function() {
  if (this._handle !== undefined && typeof this._handle.unref === "function") this._handle.unref();
  return this;
};
Server.prototype.getConnections = function(callback) {
  callback(null, 0);
};

export const createServer = (options, requestListener) => new Server(options, requestListener);

const unsupportedClient = () => {
  const error = new Error("node:http client requests are not implemented");
  error.code = "ERR_NOT_IMPLEMENTED";
  throw error;
};

export const request = unsupportedClient;
export const get = unsupportedClient;
export const METHODS = Object.freeze([
  "ACL", "BIND", "CHECKOUT", "CONNECT", "COPY", "DELETE", "GET", "HEAD", "LINK",
  "LOCK", "M-SEARCH", "MERGE", "MKACTIVITY", "MKCALENDAR", "MKCOL", "MOVE",
  "NOTIFY", "OPTIONS", "PATCH", "POST", "PROPFIND", "PROPPATCH", "PURGE", "PUT",
  "REBIND", "REPORT", "SEARCH", "SOURCE", "SUBSCRIBE", "TRACE", "UNBIND", "UNLINK",
  "UNLOCK", "UNSUBSCRIBE"
]);
export const STATUS_CODES = Object.freeze({
  100: "Continue", 101: "Switching Protocols", 200: "OK", 201: "Created",
  202: "Accepted", 204: "No Content", 301: "Moved Permanently", 302: "Found",
  304: "Not Modified", 400: "Bad Request", 401: "Unauthorized", 403: "Forbidden",
  404: "Not Found", 405: "Method Not Allowed", 408: "Request Timeout",
  409: "Conflict", 413: "Payload Too Large", 429: "Too Many Requests",
  500: "Internal Server Error", 501: "Not Implemented", 502: "Bad Gateway",
  503: "Service Unavailable", 504: "Gateway Timeout"
});
export const maxHeaderSize = 16384;
export const globalAgent = Object.freeze({ protocol: "http:" });

const http = {
  METHODS,
  STATUS_CODES,
  IncomingMessage,
  Server,
  ServerResponse,
  createServer,
  get,
  globalAgent,
  maxHeaderSize,
  request
};
export default http;
