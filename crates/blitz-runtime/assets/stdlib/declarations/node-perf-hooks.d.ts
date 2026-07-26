export type EntryType = "mark" | "measure" | "resource" | "dns" | "function" | "gc" | "http2" | "http" | "net" | "node";

export interface PerformanceEntry {
  readonly name: string;
  readonly entryType: EntryType;
  readonly startTime: number;
  readonly duration: number;
  readonly detail?: unknown;
  toJSON(): unknown;
}

export interface PerformanceMark extends PerformanceEntry {
  readonly entryType: "mark";
  readonly duration: 0;
}

export interface PerformanceMeasure extends PerformanceEntry {
  readonly entryType: "measure";
}

export interface PerformanceResourceTiming extends PerformanceEntry {
  readonly entryType: "resource";
  readonly workerStart: number;
  readonly redirectStart: number;
  readonly redirectEnd: number;
  readonly fetchStart: number;
  readonly domainLookupStart: number;
  readonly domainLookupEnd: number;
  readonly connectStart: number;
  readonly connectEnd: number;
  readonly secureConnectionStart: number;
  readonly requestStart: number;
  readonly responseEnd: number;
  readonly transferSize: number;
  readonly encodedBodySize: number;
  readonly decodedBodySize: number;
}

export interface PerformanceMarkOptions {
  detail?: unknown;
  startTime?: number;
}

export interface PerformanceMeasureOptions {
  detail?: unknown;
  duration?: number;
  end?: number | string;
  start?: number | string;
}

export interface Performance {
  readonly timeOrigin: number;
  now(): number;
  mark(name: string, options?: PerformanceMarkOptions): PerformanceMark;
  measure(name: string, startMark?: string, endMark?: string): PerformanceMeasure;
  measure(name: string, options: PerformanceMeasureOptions): PerformanceMeasure;
  getEntries(): PerformanceEntry[];
  getEntriesByName(name: string, type?: EntryType): PerformanceEntry[];
  getEntriesByType(type: EntryType): PerformanceEntry[];
  clearMarks(name?: string): void;
  clearMeasures(name?: string): void;
  clearResourceTimings(name?: string): void;
  eventLoopUtilization(
    previous?: EventLoopUtilization,
    current?: EventLoopUtilization,
  ): EventLoopUtilization;
  toJSON(): unknown;
}

export interface PerformanceObserverEntryList {
  getEntries(): PerformanceEntry[];
  getEntriesByName(name: string, type?: EntryType): PerformanceEntry[];
  getEntriesByType(type: EntryType): PerformanceEntry[];
}

export interface PerformanceObserverOptions {
  entryTypes: readonly EntryType[];
  buffered?: boolean;
}

export interface PerformanceObserverTypeOptions {
  type: EntryType;
  buffered?: boolean;
}

export type PerformanceObserverCallback = (
  list: PerformanceObserverEntryList,
  observer: PerformanceObserver,
) => void;

export interface PerformanceObserver {
  disconnect(): void;
  observe(options: PerformanceObserverOptions | PerformanceObserverTypeOptions): void;
  takeRecords(): PerformanceEntry[];
}

export interface PerformanceEntryConstructor {
  new (...args: never[]): PerformanceEntry;
}

export interface PerformanceMarkConstructor {
  new (...args: never[]): PerformanceMark;
}

export interface PerformanceMeasureConstructor {
  new (...args: never[]): PerformanceMeasure;
}

export interface PerformanceResourceTimingConstructor {
  new (...args: never[]): PerformanceResourceTiming;
}

export interface PerformanceObserverEntryListConstructor {
  new (...args: never[]): PerformanceObserverEntryList;
}

export interface PerformanceObserverConstructor {
  new (callback: PerformanceObserverCallback): PerformanceObserver;
}

/** Undefined when the host has no global constructor with this name. */
export declare const PerformanceEntry: PerformanceEntryConstructor | undefined;
/** Undefined when the host has no global constructor with this name. */
export declare const PerformanceMark: PerformanceMarkConstructor | undefined;
/** Undefined when the host has no global constructor with this name. */
export declare const PerformanceMeasure: PerformanceMeasureConstructor | undefined;
/** Undefined when the host has no global constructor with this name. */
export declare const PerformanceObserver: PerformanceObserverConstructor | undefined;
/** Undefined when the host has no global constructor with this name. */
export declare const PerformanceObserverEntryList: PerformanceObserverEntryListConstructor | undefined;
/** Undefined when the host has no global constructor with this name. */
export declare const PerformanceResourceTiming: PerformanceResourceTimingConstructor | undefined;

export declare const performance: Performance;

export interface EventLoopUtilization {
  readonly idle: number;
  readonly active: number;
  readonly utilization: number;
}

export interface EventLoopMonitorOptions {
  resolution?: number;
}

export interface IntervalHistogram {
  readonly count: number;
  readonly max: number;
  readonly mean: number;
  readonly min: number;
  readonly stddev: number;
  enable(): boolean;
  disable(): boolean;
  reset(): void;
}

export declare function monitorEventLoopDelay(options?: EventLoopMonitorOptions): never;
/** Throws ERR_NOT_IMPLEMENTED when the host performance object has no implementation. */
export declare function eventLoopUtilization(
  previous?: EventLoopUtilization,
  current?: EventLoopUtilization,
): EventLoopUtilization;

export interface PerformanceConstants {
  readonly NODE_PERFORMANCE_GC_MAJOR: 4;
  readonly NODE_PERFORMANCE_GC_MINOR: 1;
  readonly NODE_PERFORMANCE_GC_INCREMENTAL: 8;
  readonly NODE_PERFORMANCE_GC_WEAKCB: 16;
  readonly NODE_PERFORMANCE_GC_FLAGS_NO: 0;
  readonly NODE_PERFORMANCE_GC_FLAGS_CONSTRUCT_RETAINED: 2;
  readonly NODE_PERFORMANCE_GC_FLAGS_FORCED: 4;
  readonly NODE_PERFORMANCE_GC_FLAGS_SYNCHRONOUS_PHANTOM_PROCESSING: 8;
  readonly NODE_PERFORMANCE_GC_FLAGS_ALL_AVAILABLE_GARBAGE: 16;
  readonly NODE_PERFORMANCE_GC_FLAGS_ALL_EXTERNAL_MEMORY: 32;
  readonly NODE_PERFORMANCE_GC_FLAGS_SCHEDULE_IDLE: 64;
}

export declare const constants: PerformanceConstants;

declare const perfHooks: {
  readonly constants: typeof constants;
  readonly eventLoopUtilization: typeof eventLoopUtilization;
  readonly monitorEventLoopDelay: typeof monitorEventLoopDelay;
  readonly performance: typeof performance;
  readonly PerformanceEntry?: typeof PerformanceEntry;
  readonly PerformanceMark?: typeof PerformanceMark;
  readonly PerformanceMeasure?: typeof PerformanceMeasure;
  readonly PerformanceObserver?: typeof PerformanceObserver;
  readonly PerformanceObserverEntryList?: typeof PerformanceObserverEntryList;
  readonly PerformanceResourceTiming?: typeof PerformanceResourceTiming;
};

export { perfHooks as default };
