import EventEmitter from "node:events";

// Domains are a deprecated, synchronous compatibility boundary. Blitz does
// not have Node's async-resource or process exception machinery, so only
// explicit enter/run/bind/intercept scopes and explicitly added emitters are
// tracked. A callback resumed later is not implicitly in its old domain.
const stack = [];
const activeExport = { value: null };

const isObjectLike = value => value !== null && (typeof value === "object" || typeof value === "function");

const setActive = value => {
  activeExport.value = value;
};

function Domain() {
  if (!(this instanceof Domain)) return new Domain();
  EventEmitter.call(this);
  this.members = [];
  Object.defineProperty(this, "domain", {
    configurable: true,
    enumerable: false,
    value: null,
    writable: true,
  });
}

Domain.prototype = Object.create(EventEmitter.prototype);
Object.defineProperty(Domain.prototype, "constructor", {
  configurable: true,
  value: Domain,
  writable: true,
});
Domain.prototype.members = [];

Domain.prototype._errorHandler = function(error, emitter) {
  if (error !== null && (typeof error === "object" || typeof error === "function")) {
    try {
      error.domainEmitter = emitter;
      error.domainThrown = false;
    } catch {
      // Error-like host objects can be non-extensible; routing still applies.
    }
  }
  return this.emit("error", error);
};

Domain.prototype.enter = function() {
  stack.push(this);
  setActive(this);
};

Domain.prototype.exit = function() {
  const index = stack.lastIndexOf(this);
  if (index === -1) return;
  stack.length = index;
  setActive(stack.length === 0 ? undefined : stack[stack.length - 1]);
};

Domain.prototype.add = function(emitter) {
  if (!isObjectLike(emitter)) {
    // Let defineProperty provide the same native TypeError family as Node.
    Object.defineProperty(emitter, "domain", { value: this });
  }
  const previous = emitter.domain;
  if (previous === this) return undefined;
  if (previous !== null && previous !== undefined && typeof previous.remove === "function") {
    previous.remove(emitter);
  }
  Object.defineProperty(emitter, "domain", {
    configurable: true,
    enumerable: false,
    value: this,
    writable: true,
  });
  if (!this.members.includes(emitter)) this.members.push(emitter);
  return undefined;
};

Domain.prototype.remove = function(emitter) {
  const index = this.members.indexOf(emitter);
  if (index !== -1) this.members.splice(index, 1);
  emitter.domain = null;
  return undefined;
};

Domain.prototype.run = function(callback, ...args) {
  this.enter();
  try {
    return callback(...args);
  } finally {
    this.exit();
  }
};

Domain.prototype.bind = function(callback) {
  const domain = this;
  const bound = function(...args) {
    return domain.run(() => callback.apply(this, args));
  };
  return bound;
};

Domain.prototype.intercept = function(callback) {
  const domain = this;
  const intercepted = function(first, ...args) {
    if (first instanceof Error) {
      domain._errorHandler(first);
      return undefined;
    }
    return domain.run(() => callback.apply(this, args));
  };
  return intercepted;
};

Domain.prototype.dispose = function() {
  for (const member of this.members.slice()) this.remove(member);
  this.exit();
  this.removeAllListeners();
};

const createDomain = () => new Domain();
const create = createDomain;
const active = null;
const domain = {
  _stack: stack,
  Domain,
  createDomain,
  create,
  active: activeExport.value,
};

// Keep the CommonJS-shaped default object live in the same intentionally
// imperfect way as Node: its active property changes, while the named ESM
// `active` binding is the initial null snapshot.
const syncDefaultActive = () => {
  domain.active = activeExport.value;
};
const originalEnter = Domain.prototype.enter;
Domain.prototype.enter = function() {
  originalEnter.call(this);
  syncDefaultActive();
};
const originalExit = Domain.prototype.exit;
Domain.prototype.exit = function() {
  originalExit.call(this);
  syncDefaultActive();
};

export { Domain, stack as _stack, active, create, createDomain };
export default domain;
