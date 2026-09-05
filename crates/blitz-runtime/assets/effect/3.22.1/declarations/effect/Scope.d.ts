import type * as Context from "effect/Context";
import type * as Effect from "effect/Effect";
import type * as Exit from "effect/Exit";

interface Pipeable<Self> {
  readonly pipe: {
    <A>(ab: (self: Self) => A): A;
    <A, B>(ab: (self: Self) => A, bc: (value: A) => B): B;
    <A, B, C>(ab: (self: Self) => A, bc: (value: A) => B, cd: (value: B) => C): C;
  };
}

interface SequentialExecutionStrategy {
  readonly _tag: "Sequential";
}
interface ParallelExecutionStrategy {
  readonly _tag: "Parallel";
}
interface ParallelNExecutionStrategy {
  readonly _tag: "ParallelN";
  readonly parallelism: number;
}
type ExecutionStrategy = SequentialExecutionStrategy | ParallelExecutionStrategy | ParallelNExecutionStrategy;

export interface Scope extends Pipeable<Scope> {
  readonly strategy: ExecutionStrategy;
}

export interface CloseableScope extends Scope {}

export declare const Scope: Context.Tag<Scope, Scope>;

export declare namespace Scope {
  type Finalizer = (exit: Exit.Exit<unknown, unknown>) => Effect.Effect<void>;
  type Closeable = CloseableScope;
}

export declare const addFinalizer: (scope: Scope, finalizer: Effect.Effect<unknown>) => Effect.Effect<void>;
export declare const addFinalizerExit: (scope: Scope, finalizer: Scope.Finalizer) => Effect.Effect<void>;
export declare const make: (executionStrategy?: ExecutionStrategy) => Effect.Effect<CloseableScope>;
export declare const close: (scope: CloseableScope, exit: Exit.Exit<unknown, unknown>) => Effect.Effect<void>;
