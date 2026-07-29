import * as Chunk from "effect/Chunk";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Stream from "effect/Stream";

interface Clock {
  readonly now: number;
}
interface Logger {
  readonly read: () => Effect.Effect<number>;
}
interface BuildError {
  readonly _tag: "BuildError";
}

const Clock = Context.GenericTag<"fixture/Clock", Clock>("fixture/Clock");
const Logger = Context.GenericTag<"fixture/Logger", Logger>("fixture/Logger");
const clockLayer = Layer.succeed(Clock, { now: 1 });
const loggerEffect =
  null as unknown as Effect.Effect<Logger, BuildError, "fixture/Clock">;
const loggerLayer = Layer.effect(Logger, loggerEffect);
const complete: Layer.Layer<"fixture/Logger", BuildError> =
  Layer.provide(loggerLayer, clockLayer);
const completeAndMerged: Layer.Layer<
  "fixture/Clock" | "fixture/Logger",
  BuildError
> = Layer.provideMerge(loggerLayer, clockLayer);

const accessed =
  null as unknown as Effect.Effect<number, never, "fixture/Clock">;
const closed: Effect.Effect<number> =
  Effect.provideService(accessed, Clock, { now: 1 });
const resource = Effect.acquireRelease(
  Effect.succeed("resource"),
  (_value, _exit: Exit.Exit<unknown, unknown>) => Effect.succeed(undefined),
);
const scoped: Effect.Effect<string> = Effect.scoped(resource);

const fiber = null as unknown as Fiber.Fiber<number, BuildError>;
const awaited: Effect.Effect<Exit.Exit<number, BuildError>> = Fiber.await(fiber);
const collected: Effect.Effect<Chunk.Chunk<number>> =
  Stream.runCollect(Stream.scoped(Effect.succeed(1)));

void [complete, completeAndMerged, closed, scoped, awaited, collected];
