const console = globalThis.console;
const noop = () => undefined;
const primitive = name => typeof console[name] === "function" ? console[name] : noop;

export const log = primitive("log");
export const info = primitive("info");
export const debug = primitive("debug");
export const warn = primitive("warn");
export const error = primitive("error");
export const dir = primitive("dir");
export const dirxml = primitive("dirxml");
export const assert = primitive("assert");
export const count = primitive("count");
export const countReset = primitive("countReset");
export const time = primitive("time");
export const timeLog = primitive("timeLog");
export const timeEnd = primitive("timeEnd");
export const group = primitive("group");
export const groupCollapsed = primitive("groupCollapsed");
export const groupEnd = primitive("groupEnd");
export const table = primitive("table");
export const trace = primitive("trace");
export const clear = primitive("clear");
export const profile = primitive("profile");
export const profileEnd = primitive("profileEnd");
export const timeStamp = primitive("timeStamp");

class Console {
  constructor(..._args) {}

  log(...args) { return log(...args); }
  info(...args) { return info(...args); }
  debug(...args) { return debug(...args); }
  warn(...args) { return warn(...args); }
  error(...args) { return error(...args); }
  dir(...args) { return dir(...args); }
  dirxml(...args) { return dirxml(...args); }
  assert(...args) { return assert(...args); }
  count(...args) { return count(...args); }
  countReset(...args) { return countReset(...args); }
  time(...args) { return time(...args); }
  timeLog(...args) { return timeLog(...args); }
  timeEnd(...args) { return timeEnd(...args); }
  group(...args) { return group(...args); }
  groupCollapsed(...args) { return groupCollapsed(...args); }
  groupEnd(...args) { return groupEnd(...args); }
  table(...args) { return table(...args); }
  trace(...args) { return trace(...args); }
  clear(...args) { return clear(...args); }
  profile(...args) { return profile(...args); }
  profileEnd(...args) { return profileEnd(...args); }
  timeStamp(...args) { return timeStamp(...args); }
}

export { Console, console as default, console };
