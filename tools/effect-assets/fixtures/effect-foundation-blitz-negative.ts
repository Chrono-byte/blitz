import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Stream from "effect/Stream";

interface Clock {
  readonly now: number;
}

const Clock = Context.GenericTag<"fixture/Clock", Clock>("fixture/Clock");
const wrongTag: Context.Tag<"fixture/Other", Clock> = Clock;
const open =
  null as unknown as Effect.Effect<number, never, "fixture/Clock">;
Effect.runSync(open);
const badFinalizer = Effect.acquireRelease(
  Effect.succeed("resource"),
  () => Effect.fail("release failed"),
);
const fiber = null as unknown as Fiber.Fiber<number, string>;
const wrongAwait: Effect.Effect<number, string> = Fiber.await(fiber);
const wrongCollection: Effect.Effect<ReadonlyArray<number>> =
  Stream.runCollect(Stream.make(1, 2));
const wrongLayer: Layer.Layer<"fixture/Other"> =
  Layer.succeed(Clock, { now: 1 });

void [wrongTag, badFinalizer, wrongAwait, wrongCollection, wrongLayer];
