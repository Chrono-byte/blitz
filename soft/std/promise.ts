/**
 * Standard library: Promise primitive.
 * Written in TypeScript, runs on the Rust VM.
 */

declare var Promise: PromiseConstructor;

interface PromiseConstructor {
  new <T>(
    executor: (
      resolve: (value: T | PromiseLike<T>) => void,
      reject: (reason?: any) => void,
    ) => void,
  ): Promise<T>;
  <T>(
    executor: (
      resolve: (value: T | PromiseLike<T>) => void,
      reject: (reason?: any) => void,
    ) => void,
  ): Promise<T>;
  readonly prototype: Promise<any>;
  resolve<T>(value: T | PromiseLike<T>): Promise<T>;
  reject<T = never>(reason?: any): Promise<T>;
  all<T>(values: readonly (T | PromiseLike<T>)[]): Promise<T[]>;
  race<T>(values: readonly PromiseLike<T>[]): Promise<T>;
  allSettled<T>(values: readonly (T | PromiseLike<T>)[]): Promise<PromiseSettledResult<T>[]>;
  any<T>(values: readonly PromiseLike<T>[]): Promise<T>;
}

interface PromiseLike<T> {
  then<TResult1 = T, TResult2 = never>(
    onfulfilled?: ((value: T) => TResult1 | PromiseLike<TResult1>) | null,
    onrejected?: ((reason: any) => TResult2 | PromiseLike<TResult2>) | null,
  ): PromiseLike<TResult1 | TResult2>;
}

interface Promise<T> {
  then<TResult1 = T, TResult2 = never>(
    onfulfilled?: ((value: T) => TResult1 | PromiseLike<TResult1>) | null,
    onrejected?: ((reason: any) => TResult2 | PromiseLike<TResult2>) | null,
  ): Promise<TResult1 | TResult2>;
  catch<TResult = never>(
    onrejected?: ((reason: any) => TResult | PromiseLike<TResult>) | null,
  ): Promise<T | TResult>;
  finally(onfinally?: (() => void) | null): Promise<T>;
  readonly [Symbol.toStringTag]: string;
}

type PromiseStatus = "pending" | "fulfilled" | "rejected";

interface PromiseSettledResult<T> {
  status: "fulfilled" | "rejected";
  value?: T;
  reason?: any;
}

const pendingPromise = { status: "pending" as PromiseStatus };

const PromiseImpl: PromiseConstructor = function <T>(
  executor: (resolve: (value: T | PromiseLike<T>) => void, reject: (reason?: any) => void) => void,
): Promise<T> {
  if (this instanceof PromiseImpl === false) {
    return new PromiseImpl(executor);
  }
  if (typeof executor !== "function") {
    throw new TypeError("Promise resolver undefined is not a function");
  }
  const resolve = (value: T | PromiseLike<T>) => {};
  const reject = (reason?: any) => {};
  try {
    executor(resolve, reject);
  } catch (error) {
    reject(error);
  }
  return this;
} as any;

PromiseImpl.resolve = function <T>(value: T | PromiseLike<T>): Promise<T> {
  if (value instanceof PromiseImpl) {
    return value;
  }
  return new PromiseImpl((resolve) => {
    resolve(value);
  });
};

PromiseImpl.reject = function <T = never>(reason?: any): Promise<T> {
  return new PromiseImpl((_, reject) => {
    reject(reason);
  });
};

PromiseImpl.all = function <T>(values: readonly (T | PromiseLike<T>)[]): Promise<T[]> {
  return new PromiseImpl((resolve, reject) => {
    const results: T[] = [];
    let pending = values.length;
    if (pending === 0) {
      resolve(results);
      return;
    }
    let i = 0;
    while (i < values.length) {
      const value = values[i];
      PromiseImpl.resolve(value).then(
        (result: T) => {
          results[i] = result;
          pending = pending - 1;
          if (pending === 0) {
            resolve(results);
          }
        },
        (error) => {
          reject(error);
        },
      );
      i = i + 1;
    }
  });
};

PromiseImpl.race = function <T>(values: readonly PromiseLike<T>[]): Promise<T> {
  return new PromiseImpl((resolve, reject) => {
    let i = 0;
    while (i < values.length) {
      PromiseImpl.resolve(values[i]).then(
        (value: T) => {
          resolve(value);
        },
        (error) => {
          reject(error);
        },
      );
      i = i + 1;
    }
  });
};

PromiseImpl.allSettled = function <T>(
  values: readonly (T | PromiseLike<T>)[],
): Promise<PromiseSettledResult<T>[]> {
  return new PromiseImpl((resolve) => {
    const results: PromiseSettledResult<T>[] = [];
    let pending = values.length;
    if (pending === 0) {
      resolve(results);
      return;
    }
    let i = 0;
    while (i < values.length) {
      const value = values[i];
      PromiseImpl.resolve(value).then(
        (result: T) => {
          results[i] = { status: "fulfilled", value: result };
          pending = pending - 1;
          if (pending === 0) {
            resolve(results);
          }
        },
        (error) => {
          results[i] = { status: "rejected", reason: error };
          pending = pending - 1;
          if (pending === 0) {
            resolve(results);
          }
        },
      );
      i = i + 1;
    }
  });
};

PromiseImpl.any = function <T>(values: readonly PromiseLike<T>[]): Promise<T> {
  return new PromiseImpl((resolve, reject) => {
    const errors: any[] = [];
    let pending = values.length;
    if (pending === 0) {
      reject(new AggregateError([], "All promises were rejected"));
      return;
    }
    let i = 0;
    while (i < values.length) {
      PromiseImpl.resolve(values[i]).then(
        (value: T) => {
          resolve(value);
        },
        (error) => {
          errors.push(error);
          pending = pending - 1;
          if (pending === 0) {
            reject(new AggregateError(errors, "All promises were rejected"));
          }
        },
      );
      i = i + 1;
    }
  });
};

if (typeof Promise !== "undefined") {
  const prototype = Promise.prototype;
  if (prototype) {
    if (!prototype.then) {
      prototype.then = function <T, TResult1, TResult2>(
        this: Promise<T>,
        onfulfilled?: ((value: T) => TResult1 | PromiseLike<TResult1>) | null,
        onrejected?: ((reason: any) => TResult2 | PromiseLike<TResult2>) | null,
      ): Promise<TResult1 | TResult2> {
        // Relax typing: our simple shim returns a permissive Promise via resolve.
        // Cast to any to avoid complex generic incompatibilities with PromiseLike.
        return (PromiseImpl.resolve(onfulfilled ? (onfulfilled as any)(this) : (this as any)) as any) as Promise<
          TResult1 | TResult2
        >;
      };
    }
    if (!prototype.catch) {
      prototype.catch = function <T, TResult>(
        this: Promise<T>,
        onrejected?: ((reason: any) => TResult | PromiseLike<TResult>) | null,
      ): Promise<T | TResult> {
        return this.then(undefined, onrejected);
      };
    }
    if (!prototype.finally) {
      prototype.finally = function <T>(
        this: Promise<T>,
        onfinally?: (() => void) | null,
      ): Promise<T> {
        return this.then(
          (value: T) => {
            if (typeof onfinally === "function") {
              onfinally();
            }
            return value;
          },
          (reason) => {
            if (typeof onfinally === "function") {
              onfinally();
            }
            throw reason;
          },
        );
      };
    }
  }
}

(Promise as any) = PromiseImpl;
