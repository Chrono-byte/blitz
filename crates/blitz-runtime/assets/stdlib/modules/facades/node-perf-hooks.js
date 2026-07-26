// Prefer the runtime's performance singleton. In Blitz this may be absent, so
// the fallback below provides a deliberately small, bounded User Timing slice.
const hostPerformance = globalThis.performance;
const hasHostPerformance = hostPerformance !== undefined && hostPerformance !== null
  && (typeof hostPerformance === "object" || typeof hostPerformance === "function")
  && typeof hostPerformance.now === "function"
  && typeof hostPerformance.mark === "function"
  && typeof hostPerformance.measure === "function"
  && typeof hostPerformance.getEntries === "function";

const hostConstructor = name => {
  const value = globalThis[name];
  return typeof value === "function" ? value : undefined;
};

// These are globals in Node 24 and in web-compatible hosts. Do not manufacture
// constructors when the host does not provide them: instanceof and prototype
// identity would otherwise be misleading.
const PerformanceObserver = hostConstructor("PerformanceObserver");
const PerformanceObserverEntryList = hostConstructor("PerformanceObserverEntryList");
const PerformanceEntry = hostConstructor("PerformanceEntry");
const PerformanceMark = hostConstructor("PerformanceMark");
const PerformanceMeasure = hostConstructor("PerformanceMeasure");
const PerformanceResourceTiming = hostConstructor("PerformanceResourceTiming");

const unsupported = operation => {
  const error = new Error(`node:perf_hooks.${operation} is not implemented by Blitz`);
  error.code = "ERR_NOT_IMPLEMENTED";
  throw error;
};

const isFiniteNumber = value => typeof value === "number" && Number.isFinite(value);

const createLocalPerformance = () => {
  const timeOrigin = Date.now();
  const entries = [];
  const maxEntries = 1024;
  let lastNow = 0;

  const now = () => {
    const current = Math.max(0, Date.now() - timeOrigin);
    lastNow = Math.max(lastNow, current);
    return lastNow;
  };

  const addEntry = entry => {
    if (entries.length === maxEntries) entries.shift();
    entries.push(entry);
    return entry;
  };

  const latestMark = name => {
    for (let index = entries.length - 1; index >= 0; index -= 1) {
      const entry = entries[index];
      if (entry.entryType === "mark" && entry.name === name) return entry;
    }
    return undefined;
  };

  const missingMark = name => {
    const error = new Error(`The mark '${name}' does not exist`);
    error.code = "ERR_INVALID_ARG_VALUE";
    return error;
  };

  const resolveTime = (value, fallback) => {
    if (value === undefined) return fallback;
    if (isFiniteNumber(value)) return value;
    if (typeof value === "string") {
      const mark = latestMark(value);
      if (mark !== undefined) return mark.startTime;
      throw missingMark(value);
    }
    return Number(value);
  };

  const entry = (name, entryType, startTime, duration, detail) => {
    const value = {
      name,
      entryType,
      startTime,
      duration,
      detail,
      toJSON() {
        return {
          name: this.name,
          entryType: this.entryType,
          startTime: this.startTime,
          duration: this.duration,
          detail: this.detail,
        };
      },
    };
    return Object.freeze(value);
  };

  const mark = (name, options) => {
    const settings = options !== null && typeof options === "object" ? options : {};
    const startTime = settings.startTime === undefined ? now() : Number(settings.startTime);
    if (!isFiniteNumber(startTime)) unsupported("performance.mark");
    return addEntry(entry(String(name), "mark", startTime, 0, settings.detail ?? null));
  };

  const measure = (name, startOrOptions, endMark) => {
    let start = startOrOptions;
    let end = endMark;
    let detail = null;
    let duration;

    if (startOrOptions !== null && typeof startOrOptions === "object") {
      start = startOrOptions.start;
      end = startOrOptions.end;
      detail = startOrOptions.detail ?? null;
      duration = startOrOptions.duration;
    }

    const startTime = resolveTime(start, 0);
    const endTime = resolveTime(end, now());
    if (duration !== undefined) {
      if (!isFiniteNumber(Number(duration))) unsupported("performance.measure");
      return addEntry(entry(String(name), "measure", startTime, Number(duration), detail));
    }
    return addEntry(entry(String(name), "measure", startTime, endTime - startTime, detail));
  };

  const getEntries = () => entries.slice().sort((left, right) => left.startTime - right.startTime);
  const getEntriesByName = (name, type) => getEntries().filter(entryValue => (
    entryValue.name === String(name) && (type === undefined || entryValue.entryType === type)
  ));
  const getEntriesByType = type => getEntries().filter(entryValue => entryValue.entryType === type);
  const clear = type => name => {
    for (let index = entries.length - 1; index >= 0; index -= 1) {
      if (entries[index].entryType === type && (name === undefined || entries[index].name === String(name))) {
        entries.splice(index, 1);
      }
    }
  };

  const localPerformance = {
    clearMarks: clear("mark"),
    clearMeasures: clear("measure"),
    clearResourceTimings: clear("resource"),
    eventLoopUtilization: () => unsupported("performance.eventLoopUtilization"),
    getEntries,
    getEntriesByName,
    getEntriesByType,
    mark,
    measure,
    now,
    timeOrigin,
    toJSON: () => ({ timeOrigin }),
  };
  return Object.freeze(localPerformance);
};

export const performance = hasHostPerformance ? hostPerformance : createLocalPerformance();

// Node's constants are stable values, but there is no host capability behind
// them in Blitz. They describe entry metadata; they do not claim GC support.
export const constants = Object.freeze({
  NODE_PERFORMANCE_GC_MAJOR: 4,
  NODE_PERFORMANCE_GC_MINOR: 1,
  NODE_PERFORMANCE_GC_INCREMENTAL: 8,
  NODE_PERFORMANCE_GC_WEAKCB: 16,
  NODE_PERFORMANCE_GC_FLAGS_NO: 0,
  NODE_PERFORMANCE_GC_FLAGS_CONSTRUCT_RETAINED: 2,
  NODE_PERFORMANCE_GC_FLAGS_FORCED: 4,
  NODE_PERFORMANCE_GC_FLAGS_SYNCHRONOUS_PHANTOM_PROCESSING: 8,
  NODE_PERFORMANCE_GC_FLAGS_ALL_AVAILABLE_GARBAGE: 16,
  NODE_PERFORMANCE_GC_FLAGS_ALL_EXTERNAL_MEMORY: 32,
  NODE_PERFORMANCE_GC_FLAGS_SCHEDULE_IDLE: 64,
});

export const monitorEventLoopDelay = () => unsupported("monitorEventLoopDelay");

export const eventLoopUtilization = (...args) => {
  const method = performance.eventLoopUtilization;
  if (typeof method !== "function") return unsupported("eventLoopUtilization");
  return Function.prototype.apply.call(method, performance, args);
};

const perfHooks = Object.freeze({
  constants,
  eventLoopUtilization,
  monitorEventLoopDelay,
  performance,
  ...(PerformanceObserver === undefined ? {} : { PerformanceObserver }),
  ...(PerformanceObserverEntryList === undefined ? {} : { PerformanceObserverEntryList }),
  ...(PerformanceEntry === undefined ? {} : { PerformanceEntry }),
  ...(PerformanceMark === undefined ? {} : { PerformanceMark }),
  ...(PerformanceMeasure === undefined ? {} : { PerformanceMeasure }),
  ...(PerformanceResourceTiming === undefined ? {} : { PerformanceResourceTiming }),
});

export {
  PerformanceEntry,
  PerformanceMark,
  PerformanceMeasure,
  PerformanceObserver,
  PerformanceObserverEntryList,
  PerformanceResourceTiming,
};
export default perfHooks;
