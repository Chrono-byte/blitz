import type * as Context from "effect/Context";
import type * as Effect from "effect/Effect";
import type * as Layer from "effect/Layer";
import type * as Runtime from "blitz:runtime";

interface Clock { readonly now: number }
const clock = Context.GenericTag<"Clock", Clock>("Clock");
const wrongTag: Context.Tag<"Other", Clock> = clock;
const wrongLayer: Layer.Layer<"Other"> = Layer.succeed<"Clock", Clock>(clock, { now: 1 });
const wrongMember = Layer.missing;
const open = null as unknown as Effect.Effect<number, never, "Clock">;
Runtime.runMain(open);
