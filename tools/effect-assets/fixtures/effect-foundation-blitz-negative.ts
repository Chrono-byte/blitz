import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Stream from "effect/Stream";

interface Clock {
  readonly now: number;
}

const Clock = Context.GenericTag<"fixture/Clock", Clock>("fixture/Clock");
declare const open: Effect.Effect<number, never, "fixture/Clock">;
declare const fiber: Fiber.Fiber<number, string>;

// @case tag-identity
const wrongTag: Context.Tag<"fixture/Other", Clock> = Clock;

// @case open-requirements
Effect.runSync(open);

// @case failing-finalizer
const badFinalizer = Effect.acquireRelease(
  Effect.succeed("resource"),
  () => Effect.fail("release failed"),
);

// @case fiber-await-exit
const wrongAwait: Effect.Effect<number, string> = Fiber.await(fiber);

// @case stream-collection
const wrongCollection: Effect.Effect<ReadonlyArray<number>> =
  Stream.runCollect(Stream.make(1, 2));

// @case layer-identity
const wrongLayer: Layer.Layer<"fixture/Other"> =
  Layer.succeed(Clock, { now: 1 });
