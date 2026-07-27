// SPDX-License-Identifier: MPL-2.0
import type * as Context from "effect/Context";
import type * as Effect from "effect/Effect";
import type * as Layer from "effect/Layer";
import type * as Fiber from "effect/Fiber";
import type * as Runtime from "blitz:runtime";

interface Clock { readonly now: number }
const clock = Context.GenericTag<"Clock", Clock>("Clock");
const live: Layer.Layer<"Clock"> = Layer.succeed<"Clock", Clock>(clock, { now: 1 });
const effectLayer: Layer.Layer<"Clock"> = Layer.effect<"Clock", Clock, never, never>(clock, resource);
const fiber = null as unknown as Fiber.Fiber<number, string>;
const observed = null as unknown as Effect.Effect<Fiber.Exit<number, string>>;
Runtime.runMain(program);
const program: Effect.Effect<number> = Effect.succeed(1);
const resource: Effect.Effect<Clock> = Effect.succeed({ now: 1 });
const acquired: Effect.Effect<Clock> = Effect.acquireRelease(
  resource,
  (value: Clock, exit: Effect.Exit<unknown, unknown>) => Effect.succeed(undefined),
);
const overloadNumber: number = Effect.overloaded("value");
const overloadString: string = Effect.overloaded(1);

type WithoutError = Exclude<"ok" | "bad", "bad">;
type Table = Record<"ok" | "bad", number>;
type Values = ReadonlyArray<number>;
type Copy<T> = { [K in keyof T]: T[K] };
interface Options { readonly name: string; readonly debug?: boolean }
const options: Options = { name: "foundation" };
const result: WithoutError = "ok";
const table: Table = { ok: 1, bad: 2 };
const values: Values = [1, 2];
interface Pair { readonly name: string }
const copied: Copy<Pair> = { name: "foundation" };
