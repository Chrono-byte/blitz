import type * as HashSet from "effect/HashSet";
import type * as Option from "effect/Option";

export type FiberId = None | Runtime | Composite;
export interface None { readonly _tag: "None" }
export interface Runtime { readonly _tag: "Runtime"; readonly id: number; readonly startTimeMillis: number }
export interface Composite { readonly _tag: "Composite"; readonly left: FiberId; readonly right: FiberId }

export declare const none: None;
export declare const runtime: (id: number, startTimeMillis: number) => Runtime;
export declare const composite: (left: FiberId, right: FiberId) => Composite;
export declare const isFiberId: (value: unknown) => value is FiberId;
export declare const isNone: (value: FiberId) => value is None;
export declare const isRuntime: (value: FiberId) => value is Runtime;
export declare const isComposite: (value: FiberId) => value is Composite;
export declare const combine: {
  (right: FiberId): (left: FiberId) => FiberId;
  (left: FiberId, right: FiberId): FiberId;
};
export declare const ids: (fiberId: FiberId) => HashSet.HashSet<number>;
export declare const toOption: (fiberId: FiberId) => Option.Option<FiberId>;
export declare const unsafeMake: (_: void) => Runtime;
