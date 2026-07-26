export interface Tracing {
  readonly categories: string;
  readonly enabled: false;
  enable(): void;
  disable(): void;
}

export declare function createTracing(options: { readonly categories: readonly string[] }): Tracing;
export declare function getEnabledCategories(): "";

export interface TraceEventsModule {
  readonly createTracing: typeof createTracing;
  readonly getEnabledCategories: typeof getEnabledCategories;
}

declare const traceEvents: TraceEventsModule;
export { traceEvents as default };
