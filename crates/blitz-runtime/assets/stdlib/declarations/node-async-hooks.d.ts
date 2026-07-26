export interface HookCallbacks {
  init?(asyncId: number, type: string, triggerAsyncId: number, resource: object): void;
  before?(asyncId: number): void;
  after?(asyncId: number): void;
  promiseResolve?(asyncId: number): void;
  destroy?(asyncId: number): void;
}

/**
 * Lifecycle hooks are not exposed by this initial facade because the VM does
 * not yet provide truthful async resource init/before/after/destroy events.
 */
export declare function createHook(callbacks: HookCallbacks): never;

export interface AsyncResourceOptions {
  triggerAsyncId?: number;
  requireManualDestroy?: boolean;
}

export declare class AsyncResource {
  constructor(type: string, triggerAsyncId?: number | AsyncResourceOptions);

  static bind<Func extends (this: ThisArg, ...args: any[]) => any, ThisArg>(
    fn: Func,
    type?: string,
    thisArg?: ThisArg,
  ): Func;

  bind<Func extends (...args: any[]) => any>(fn: Func): Func;
  runInAsyncScope<This, Result>(
    fn: (this: This, ...args: any[]) => Result,
    thisArg?: This,
    ...args: any[]
  ): Result;
  emitDestroy(): this;
  asyncId(): number;
  triggerAsyncId(): number;
}

export interface AsyncLocalStorageOptions<T = unknown> {
  defaultValue?: T;
  name?: string;
}

export declare class AsyncLocalStorage<T = unknown> {
  constructor(options?: AsyncLocalStorageOptions<T>);

  static bind<Func extends (...args: any[]) => any>(fn: Func): Func;
  static snapshot(): <Result, Args extends any[]>(fn: (...args: Args) => Result, ...args: Args) => Result;

  disable(): void;
  getStore(): T | undefined;
  readonly name: string;
  run<Result>(store: T, callback: () => Result): Result;
  run<Result, Args extends any[]>(store: T, callback: (...args: Args) => Result, ...args: Args): Result;
  exit<Result, Args extends any[]>(callback: (...args: Args) => Result, ...args: Args): Result;
  enterWith(store: T): void;
}

export declare function executionAsyncId(): number;
export declare function triggerAsyncId(): number;
export declare function executionAsyncResource(): object;

declare const asyncHooks: {
  readonly AsyncLocalStorage: typeof AsyncLocalStorage;
  readonly AsyncResource: typeof AsyncResource;
  readonly createHook: typeof createHook;
  readonly executionAsyncId: typeof executionAsyncId;
  readonly executionAsyncResource: typeof executionAsyncResource;
  readonly triggerAsyncId: typeof triggerAsyncId;
};

export { asyncHooks as default };
