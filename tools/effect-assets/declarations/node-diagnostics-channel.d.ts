export type ChannelName = string | symbol;
export type ChannelListener = (message: unknown, name: ChannelName) => void;

export interface StoreLike<StoreType> {
  run<Result>(store: StoreType, callback: () => Result): Result;
}

export interface TracingChannelSubscribers<ContextType extends object> {
  start: (message: ContextType) => void;
  end: (message: ContextType & { error?: unknown; result?: unknown }) => void;
  asyncStart: (message: ContextType & { error?: unknown; result?: unknown }) => void;
  asyncEnd: (message: ContextType & { error?: unknown; result?: unknown }) => void;
  error: (message: ContextType & { error: unknown }) => void;
}

export interface TracingChannelCollection<StoreType = unknown, ContextType = StoreType> {
  start: Channel<StoreType, ContextType>;
  end: Channel<StoreType, ContextType>;
  asyncStart: Channel<StoreType, ContextType>;
  asyncEnd: Channel<StoreType, ContextType>;
  error: Channel<StoreType, ContextType>;
}

export declare class Channel<StoreType = unknown, ContextType = StoreType> {
  readonly name: ChannelName;
  readonly hasSubscribers: boolean;

  constructor(name: ChannelName);
  publish(message: unknown): void;
  subscribe(onMessage: ChannelListener): void;
  unsubscribe(onMessage: ChannelListener): boolean;
  bindStore(store: StoreLike<StoreType>, transform?: (context: ContextType) => StoreType): void;
  unbindStore(store: StoreLike<StoreType>): boolean;
  runStores<ThisArg = any, Args extends any[] = any[], Result = any>(
    context: ContextType,
    fn: (this: ThisArg, ...args: Args) => Result,
    thisArg?: ThisArg,
    ...args: Args
  ): Result;
}

export interface TracingChannel<StoreType = unknown, ContextType extends object = {}>
  extends TracingChannelCollection<StoreType, ContextType> {
  readonly start: Channel<StoreType, ContextType>;
  readonly end: Channel<StoreType, ContextType>;
  readonly asyncStart: Channel<StoreType, ContextType>;
  readonly asyncEnd: Channel<StoreType, ContextType>;
  readonly error: Channel<StoreType, ContextType>;
  readonly hasSubscribers: boolean;

  subscribe(subscribers: TracingChannelSubscribers<ContextType>): void;
  unsubscribe(subscribers: TracingChannelSubscribers<ContextType>): boolean;
  traceSync<ThisArg = any, Args extends any[] = any[], Result = any>(
    fn: (this: ThisArg, ...args: Args) => Result,
    context?: ContextType,
    thisArg?: ThisArg,
    ...args: Args
  ): Result;
  tracePromise<ThisArg = any, Args extends any[] = any[], Result = any>(
    fn: (this: ThisArg, ...args: Args) => Promise<Result>,
    context?: ContextType,
    thisArg?: ThisArg,
    ...args: Args
  ): Promise<Result>;
  traceCallback<ThisArg = any, Args extends any[] = any[], Result = any>(
    fn: (this: ThisArg, ...args: Args) => Result,
    position?: number,
    context?: ContextType,
    thisArg?: ThisArg,
    ...args: Args
  ): Result;
}

export declare function channel(name: ChannelName): Channel;
export declare function hasSubscribers(name: ChannelName): boolean;
export declare function subscribe(name: ChannelName, onMessage: ChannelListener): void;
export declare function unsubscribe(name: ChannelName, onMessage: ChannelListener): boolean;
export declare function tracingChannel<
  StoreType = unknown,
  ContextType extends object = StoreType extends object ? StoreType : object,
>(nameOrChannels: string | TracingChannelCollection<StoreType, ContextType>): TracingChannel<StoreType, ContextType>;

export interface DiagnosticsChannelModule {
  readonly channel: typeof channel;
  readonly hasSubscribers: typeof hasSubscribers;
  readonly subscribe: typeof subscribe;
  readonly tracingChannel: typeof tracingChannel;
  readonly unsubscribe: typeof unsubscribe;
  readonly Channel: typeof Channel;
}

declare const diagnosticsChannel: DiagnosticsChannelModule;
export { diagnosticsChannel as default };
