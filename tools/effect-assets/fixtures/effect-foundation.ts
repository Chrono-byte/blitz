import * as Chunk from "effect/Chunk";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";

interface Clock {
  readonly now: number;
}
interface Logger {
  readonly log: (message: string) => Effect.Effect<void>;
}
interface BuildError {
  readonly _tag: "BuildError";
}

type ClockId = "fixture/Clock";
type LoggerId = "fixture/Logger";

const Clock = Context.GenericTag<ClockId, Clock>("fixture/Clock");
const Logger = Context.GenericTag<LoggerId, Logger>("fixture/Logger");

const clockLayer = Layer.succeed(Clock, { now: 1 });
const loggerLayer = Layer.effect(
  Logger,
  Effect.contextWith<ClockId, Logger>((context) => ({
    log: () => Effect.sync(() => {
      Context.get(context, Clock).now;
    }),
  })) as Effect.Effect<Logger, BuildError, ClockId>,
);

const merged: Layer.Layer<ClockId | LoggerId, BuildError, ClockId> =
  Layer.mergeAll(clockLayer, loggerLayer);
const provided: Layer.Layer<LoggerId, BuildError> =
  Layer.provide(loggerLayer, clockLayer);
const providedTuple: Layer.Layer<LoggerId, BuildError> =
  Layer.provide(loggerLayer, [clockLayer]);
const providedMerge: Layer.Layer<ClockId | LoggerId, BuildError> =
  Layer.provideMerge(loggerLayer, clockLayer);
const providedMergeCurried: Layer.Layer<ClockId | LoggerId, BuildError> =
  loggerLayer.pipe(Layer.provideMerge(clockLayer));

const accessed: Effect.Effect<number, never, ClockId> =
  Effect.contextWith((context: Context.Context<ClockId>) => Context.get(context, Clock).now);
const serviceMethod = Effect.serviceFunctionEffect(
  Logger,
  (service) => (message: string) => service.log(message),
);
const logged: Effect.Effect<void, never, LoggerId> = serviceMethod("hello");
const optional: Effect.Effect<import("effect/Option").Option<Clock>> =
  Effect.serviceOption(Clock);
const required: Effect.Effect<Clock, import("effect/Cause").NoSuchElementException> =
  Effect.serviceOptional(Clock);
const closed: Effect.Effect<number> = Effect.provideService(accessed, Clock, { now: 1 });

const resource = Effect.acquireRelease(
  Effect.succeed({ close: () => undefined }),
  (value, exit: Exit.Exit<unknown, unknown>) =>
    Effect.sync(() => {
      value.close();
      Exit.isExit(exit);
    }),
);
const scoped: Effect.Effect<{ readonly close: () => undefined }> = Effect.scoped(resource);
const explicitlyScoped: Effect.Effect<number> = Effect.scoped(
  null as unknown as Effect.Effect<number, never, Scope.Scope>,
);

const fiber = null as unknown as Fiber.RuntimeFiber<number, BuildError>;
const awaited: Effect.Effect<Exit.Exit<number, BuildError>> = Fiber.await(fiber);
const joined: Effect.Effect<number, BuildError> = Fiber.join(fiber);
const interrupted: Effect.Effect<Exit.Exit<number, BuildError>> = Fiber.interrupt(fiber);

const stream = Stream.scoped(resource);
const collected: Effect.Effect<Chunk.Chunk<{ readonly close: () => undefined }>> =
  Stream.runCollect(stream);
const acquiredStream: Stream.Stream<string> = Stream.acquireRelease(
  Effect.succeed("resource"),
  (_value, _exit) => Effect.succeed(undefined),
);
const scopedWith: Stream.Stream<number> = Stream.scopedWith(() => Effect.succeed(1));

const tuple: Effect.Effect<[number, string]> =
  Effect.all([Effect.succeed(1), Effect.succeed("two")] as const);
const covariant: Effect.Effect<number, string, string> =
  null as unknown as Effect.Effect<1, "error", ClockId>;

// @ts-expect-error tags are invariant in their identifier
const wrongTag: Context.Tag<"fixture/Other", Clock> = Clock;
// @ts-expect-error open requirements cannot be observed by a root runner
Effect.runSync(accessed);
// @ts-expect-error release effects cannot introduce typed finalizer errors
Effect.acquireRelease(Effect.succeed("resource"), () => Effect.fail("release failed"));
// @ts-expect-error Fiber.await observes an Exit, not the success value directly
const wrongAwait: Effect.Effect<number, BuildError> = Fiber.await(fiber);
// @ts-expect-error runCollect returns Chunk rather than ReadonlyArray
const wrongCollection: Effect.Effect<ReadonlyArray<number>> =
  Stream.runCollect(Stream.make(1, 2));
// @ts-expect-error Layer.provide removes only the identifiers produced by its provider
const stillOpen: Layer.Layer<LoggerId, BuildError> = Layer.provide(loggerLayer, Layer.empty);

void [
  merged,
  provided,
  providedTuple,
  providedMerge,
  providedMergeCurried,
  logged,
  optional,
  required,
  closed,
  scoped,
  explicitlyScoped,
  awaited,
  joined,
  interrupted,
  collected,
  acquiredStream,
  scopedWith,
  tuple,
  covariant,
];
