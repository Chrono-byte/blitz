import type * as Cause from "effect/Cause";
import type * as Context from "effect/Context";
import type * as Duration from "effect/Duration";
import type * as Exit from "effect/Exit";
import type * as Fiber from "effect/Fiber";
import type * as FiberId from "effect/FiberId";
import type * as HashSet from "effect/HashSet";
import type * as Option from "effect/Option";
import type * as Scope from "effect/Scope";

export interface Effect<out Success, out Error = never, out Requirements = never> {
  readonly _Success: (_: never) => Success;
  readonly _Error: (_: never) => Error;
  readonly _Requirements: (_: never) => Requirements;
  readonly pipe: {
    <A>(ab: (self: Effect<Success, Error, Requirements>) => A): A;
    <A, B>(ab: (self: Effect<Success, Error, Requirements>) => A, bc: (value: A) => B): B;
    <A, B, C>(ab: (self: Effect<Success, Error, Requirements>) => A, bc: (value: A) => B, cd: (value: B) => C): C;
  };
}

export declare const succeed: <Success>(value: Success) => Effect<Success>;
export declare const fail: <Error>(error: Error) => Effect<never, Error>;
export declare const sync: <Success>(evaluate: () => Success) => Effect<Success>;
declare const try_: {
  <Success, Error>(options: { readonly try: () => Success; readonly catch: (error: unknown) => Error }): Effect<Success, Error>;
  <Success>(evaluate: () => Success): Effect<Success, Cause.UnknownException>;
};
export { try_ as try };
export declare const suspend: <Success, Error, Requirements>(evaluate: () => Effect<Success, Error, Requirements>) => Effect<Success, Error, Requirements>;
export declare const async: <Success, Error = never, Requirements = never>(resume: (callback: (effect: Effect<Success, Error, Requirements>) => void, signal: AbortSignal) => void | Effect<void, never, Requirements>, blockingOn?: FiberId.FiberId) => Effect<Success, Error, Requirements>;
export declare const never: Effect<never>;
export declare const sleep: (duration: Duration.DurationInput) => Effect<void>;

export declare const map: {
  <Success, Output>(f: (value: Success) => Output): <Error, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Output, Error, Requirements>;
  <Success, Error, Requirements, Output>(self: Effect<Success, Error, Requirements>, f: (value: Success) => Output): Effect<Output, Error, Requirements>;
};

export declare const flatMap: {
  <Success, Output, Error2, Requirements2>(f: (value: Success) => Effect<Output, Error2, Requirements2>): <Error, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Output, Error | Error2, Requirements | Requirements2>;
  <Success, Error, Requirements, Output, Error2, Requirements2>(self: Effect<Success, Error, Requirements>, f: (value: Success) => Effect<Output, Error2, Requirements2>): Effect<Output, Error | Error2, Requirements | Requirements2>;
};

export declare const catchAll: {
  <Error, Output, Error2, Requirements2>(f: (error: Error) => Effect<Output, Error2, Requirements2>): <Success, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Success | Output, Error2, Requirements | Requirements2>;
  <Success, Error, Requirements, Output, Error2, Requirements2>(self: Effect<Success, Error, Requirements>, f: (error: Error) => Effect<Output, Error2, Requirements2>): Effect<Success | Output, Error2, Requirements | Requirements2>;
};
export declare const catchTag: {
  <Error extends { readonly _tag: string }, Tag extends Error["_tag"], Output, Error2, Requirements2>(tag: Tag, f: (error: Extract<Error, { readonly _tag: Tag }>) => Effect<Output, Error2, Requirements2>): <Success, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Success | Output, Exclude<Error, { readonly _tag: Tag }> | Error2, Requirements | Requirements2>;
  <Success, Error extends { readonly _tag: string }, Requirements, Tag extends Error["_tag"], Output, Error2, Requirements2>(self: Effect<Success, Error, Requirements>, tag: Tag, f: (error: Extract<Error, { readonly _tag: Tag }>) => Effect<Output, Error2, Requirements2>): Effect<Success | Output, Exclude<Error, { readonly _tag: Tag }> | Error2, Requirements | Requirements2>;
};
export declare const mapError: {
  <Error, Output>(f: (error: Error) => Output): <Success, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Success, Output, Requirements>;
  <Success, Error, Requirements, Output>(self: Effect<Success, Error, Requirements>, f: (error: Error) => Output): Effect<Success, Output, Requirements>;
};
export declare const tap: {
  <Success, Output, Error2, Requirements2>(f: (value: Success) => Effect<Output, Error2, Requirements2>): <Error, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Success, Error | Error2, Requirements | Requirements2>;
  <Success, Error, Requirements, Output, Error2, Requirements2>(self: Effect<Success, Error, Requirements>, f: (value: Success) => Effect<Output, Error2, Requirements2>): Effect<Success, Error | Error2, Requirements | Requirements2>;
};
export declare const as: {
  <Output>(value: Output): <Success, Error, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Output, Error, Requirements>;
  <Success, Error, Requirements, Output>(self: Effect<Success, Error, Requirements>, value: Output): Effect<Output, Error, Requirements>;
};
export declare const asVoid: <Success, Error, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<void, Error, Requirements>;
export declare const zipRight: {
  <Output, Error2, Requirements2>(that: Effect<Output, Error2, Requirements2>): <Success, Error, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Output, Error | Error2, Requirements | Requirements2>;
  <Success, Error, Requirements, Output, Error2, Requirements2>(self: Effect<Success, Error, Requirements>, that: Effect<Output, Error2, Requirements2>): Effect<Output, Error | Error2, Requirements | Requirements2>;
};
export declare const ensuring: {
  <FinalizerResult, FinalizerRequirements>(finalizer: Effect<FinalizerResult, never, FinalizerRequirements>): <Success, Error, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Success, Error, Requirements | FinalizerRequirements>;
  <Success, Error, Requirements, FinalizerResult, FinalizerRequirements>(self: Effect<Success, Error, Requirements>, finalizer: Effect<FinalizerResult, never, FinalizerRequirements>): Effect<Success, Error, Requirements | FinalizerRequirements>;
};
export declare const onInterrupt: {
  <FinalizerResult, FinalizerRequirements>(cleanup: (interruptors: HashSet.HashSet<FiberId.FiberId>) => Effect<FinalizerResult, never, FinalizerRequirements>): <Success, Error, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Success, Error, Requirements | FinalizerRequirements>;
  <Success, Error, Requirements, FinalizerResult, FinalizerRequirements>(self: Effect<Success, Error, Requirements>, cleanup: (interruptors: HashSet.HashSet<FiberId.FiberId>) => Effect<FinalizerResult, never, FinalizerRequirements>): Effect<Success, Error, Requirements | FinalizerRequirements>;
};

export declare const acquireRelease: {
  <Resource, FinalizerResult, FinalizerRequirements>(release: (resource: Resource, exit: Exit.Exit<unknown, unknown>) => Effect<FinalizerResult, never, FinalizerRequirements>): <AcquireError, AcquireRequirements>(acquire: Effect<Resource, AcquireError, AcquireRequirements>) => Effect<Resource, AcquireError, Scope.Scope | AcquireRequirements | FinalizerRequirements>;
  <Resource, AcquireError, AcquireRequirements, FinalizerResult, FinalizerRequirements>(acquire: Effect<Resource, AcquireError, AcquireRequirements>, release: (resource: Resource, exit: Exit.Exit<unknown, unknown>) => Effect<FinalizerResult, never, FinalizerRequirements>): Effect<Resource, AcquireError, Scope.Scope | AcquireRequirements | FinalizerRequirements>;
};
export declare const scoped: <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements | Scope.Scope>) => Effect<Success, Error, Exclude<Requirements, Scope.Scope>>;

export declare const fork: <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements>) => Effect<Fiber.RuntimeFiber<Success, Error>, never, Requirements>;
export declare const forkDaemon: <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements>) => Effect<Fiber.RuntimeFiber<Success, Error>, never, Requirements>;
export declare const forkIn: <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements>, scope: Scope.Scope) => Effect<Fiber.RuntimeFiber<Success, Error>, never, Requirements>;
export declare const forkScoped: <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements>) => Effect<Fiber.RuntimeFiber<Success, Error>, never, Scope.Scope | Requirements>;
export declare const interrupt: Effect<never>;
export declare const runFork: <Success, Error>(effect: Effect<Success, Error, never>) => Fiber.RuntimeFiber<Success, Error>;
export declare const runSync: <Success, Error>(effect: Effect<Success, Error, never>) => Success;
export declare const runSyncExit: <Success, Error>(effect: Effect<Success, Error, never>) => Exit.Exit<Success, Error>;
export declare const runPromise: <Success, Error>(effect: Effect<Success, Error, never>) => Promise<Success>;
export declare const fromFiber: <Success, Error>(fiber: Fiber.Fiber<Success, Error>) => Effect<Success, Error>;
export declare const exit: <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements>) => Effect<Exit.Exit<Success, Error>, never, Requirements>;

interface RunCallbackOptions<Success, Error = never> {
  readonly onExit?: ((exit: Exit.Exit<Success, Error>) => void) | undefined;
}
interface Cancel<Success, Error = never> {
  (fiberId?: FiberId.FiberId, options?: RunCallbackOptions<Success, Error> | undefined): void;
}
export declare const runCallback: <Success, Error>(effect: Effect<Success, Error>, options?: RunCallbackOptions<Success, Error>) => Cancel<Success, Error>;

type SuccessOf<EffectValue> = EffectValue extends Effect<infer Success, infer _Error, infer _Requirements> ? Success : never;
type ErrorOf<EffectValue> = EffectValue extends Effect<infer _Success, infer Error, infer _Requirements> ? Error : never;
type RequirementsOf<EffectValue> = EffectValue extends Effect<infer _Success, infer _Error, infer Requirements> ? Requirements : never;
type EffectCollection = Iterable<Effect<unknown, unknown, unknown>>;

export declare const all: <const Effects extends EffectCollection>(effects: Effects) => Effect<
  [Effects] extends [ReadonlyArray<infer Item>]
    ? number extends Effects["length"]
      ? Array<SuccessOf<Item>>
      : { -readonly [Key in keyof Effects]: SuccessOf<Effects[Key]> }
    : Array<SuccessOf<Effects extends Iterable<infer Item> ? Item : never>>,
  ErrorOf<Effects extends Iterable<infer Item> ? Item : never>,
  RequirementsOf<Effects extends Iterable<infer Item> ? Item : never>
>;
export declare const matchEffect: {
  <Error, Output, Error2, Requirements2, Success, Output2, Error3, Requirements3>(options: { readonly onFailure: (error: Error) => Effect<Output, Error2, Requirements2>; readonly onSuccess: (value: Success) => Effect<Output2, Error3, Requirements3> }): <Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Output | Output2, Error2 | Error3, Requirements | Requirements2 | Requirements3>;
  <Success, Error, Requirements, Output, Error2, Requirements2, Output2, Error3, Requirements3>(effect: Effect<Success, Error, Requirements>, options: { readonly onFailure: (error: Error) => Effect<Output, Error2, Requirements2>; readonly onSuccess: (value: Success) => Effect<Output2, Error3, Requirements3> }): Effect<Output | Output2, Error2 | Error3, Requirements | Requirements2 | Requirements3>;
};
export declare const matchCauseEffect: {
  <Error, Output, Error2, Requirements2, Success, Output2, Error3, Requirements3>(options: { readonly onFailure: (cause: Cause.Cause<Error>) => Effect<Output, Error2, Requirements2>; readonly onSuccess: (value: Success) => Effect<Output2, Error3, Requirements3> }): <Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Output | Output2, Error2 | Error3, Requirements | Requirements2 | Requirements3>;
  <Success, Error, Requirements, Output, Error2, Requirements2, Output2, Error3, Requirements3>(effect: Effect<Success, Error, Requirements>, options: { readonly onFailure: (cause: Cause.Cause<Error>) => Effect<Output, Error2, Requirements2>; readonly onSuccess: (value: Success) => Effect<Output2, Error3, Requirements3> }): Effect<Output | Output2, Error2 | Error3, Requirements | Requirements2 | Requirements3>;
};

export declare const provide: {
  <Provided>(context: Context.Context<Provided>): <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements>) => Effect<Success, Error, Exclude<Requirements, Provided>>;
  <Success, Error, Requirements, Provided>(effect: Effect<Success, Error, Requirements>, context: Context.Context<Provided>): Effect<Success, Error, Exclude<Requirements, Provided>>;
};
export declare const provideService: {
  <Identifier, Service>(tag: Context.Tag<Identifier, Service>, service: Service): <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements>) => Effect<Success, Error, Exclude<Requirements, Identifier>>;
  <Success, Error, Requirements, Identifier, Service>(effect: Effect<Success, Error, Requirements>, tag: Context.Tag<Identifier, Service>, service: Service): Effect<Success, Error, Exclude<Requirements, Identifier>>;
};
export declare const provideServiceEffect: {
  <Identifier, Service, ServiceError, ServiceRequirements>(tag: Context.Tag<Identifier, Service>, service: Effect<Service, ServiceError, ServiceRequirements>): <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements>) => Effect<Success, Error | ServiceError, Exclude<Requirements, Identifier> | ServiceRequirements>;
  <Success, Error, Requirements, Identifier, Service, ServiceError, ServiceRequirements>(effect: Effect<Success, Error, Requirements>, tag: Context.Tag<Identifier, Service>, service: Effect<Service, ServiceError, ServiceRequirements>): Effect<Success, Error | ServiceError, Exclude<Requirements, Identifier> | ServiceRequirements>;
};
export declare const contextWith: <Requirements, Success>(f: (context: Context.Context<Requirements>) => Success) => Effect<Success, never, Requirements>;
export declare const contextWithEffect: <ContextRequirements, Success, Error, Requirements>(f: (context: Context.Context<ContextRequirements>) => Effect<Success, Error, Requirements>) => Effect<Success, Error, ContextRequirements | Requirements>;
export declare const serviceFunctionEffect: <Identifier, Service, Arguments extends ReadonlyArray<unknown>, Success, Error, Requirements>(tag: Context.Tag<Identifier, Service>, f: (service: Service) => (...arguments_: Arguments) => Effect<Success, Error, Requirements>) => (...arguments_: Arguments) => Effect<Success, Error, Requirements | Identifier>;
export declare const serviceOption: <Identifier, Service>(tag: Context.Tag<Identifier, Service>) => Effect<Option.Option<Service>>;
export declare const serviceOptional: <Identifier, Service>(tag: Context.Tag<Identifier, Service>) => Effect<Service, Cause.NoSuchElementException>;
