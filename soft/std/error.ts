/**
 * Standard library: Error primitives.
 * Written in TypeScript, runs on the Rust VM.
 */

declare var Error: ErrorConstructor;
declare var TypeError: TypeErrorConstructor;
declare var RangeError: RangeErrorConstructor;
declare var ReferenceError: ReferenceErrorConstructor;
declare var SyntaxError: SyntaxErrorConstructor;
declare var EvalError: EvalErrorConstructor;
declare var URIError: URIErrorConstructor;

interface ErrorConstructor {
  new (message?: string): Error;
  (message?: string): Error;
  readonly prototype: Error;
}

interface Error {
  name: string;
  message: string;
  stack?: string;
  toString(): string;
}

interface TypeErrorConstructor extends ErrorConstructor {
  new (message?: string): TypeError;
  (message?: string): TypeError;
  readonly prototype: TypeError;
}

interface TypeError extends Error {}

interface RangeErrorConstructor extends ErrorConstructor {
  new (message?: string): RangeError;
  (message?: string): RangeError;
  readonly prototype: RangeError;
}

interface RangeError extends Error {}

interface ReferenceErrorConstructor extends ErrorConstructor {
  new (message?: string): ReferenceError;
  (message?: string): ReferenceError;
  readonly prototype: ReferenceError;
}

interface ReferenceError extends Error {}

interface SyntaxErrorConstructor extends ErrorConstructor {
  new (message?: string): SyntaxError;
  (message?: string): SyntaxError;
  readonly prototype: SyntaxError;
}

interface SyntaxError extends Error {}

interface EvalErrorConstructor extends ErrorConstructor {
  new (message?: string): EvalError;
  (message?: string): EvalError;
  readonly prototype: EvalError;
}

interface EvalError extends Error {}

interface URIErrorConstructor extends ErrorConstructor {
  new (message?: string): URIError;
  (message?: string): URIError;
  readonly prototype: URIError;
}

interface URIError extends Error {}

function createErrorClass(name: string, ParentConstructor: any): any {
  function ErrorCtor(this: any, message?: string): void {
    if (this instanceof ErrorCtor === false) {
      return new ErrorCtor(message);
    }
    this.name = name;
    this.message = message !== undefined ? String(message) : "";
    if (Error.captureStackTrace) {
      Error.captureStackTrace(this, ErrorCtor);
    }
    return this;
  }
  ErrorCtor.prototype = Object.create(ParentConstructor.prototype);
  ErrorCtor.prototype.name = name;
  ErrorCtor.prototype.constructor = ErrorCtor;
  return ErrorCtor;
}

const ErrorImpl = createErrorClass("Error", Error);

const TypeErrorImpl = createErrorClass("TypeError", TypeError || Error);
const RangeErrorImpl = createErrorClass("RangeError", RangeError || Error);
const ReferenceErrorImpl = createErrorClass("ReferenceError", ReferenceError || Error);
const SyntaxErrorImpl = createErrorClass("SyntaxError", SyntaxError || Error);
const EvalErrorImpl = createErrorClass("EvalError", EvalError || Error);
const URIErrorImpl = createErrorClass("URIError", URIError || Error);

if (Error.prototype && !Error.prototype.toString) {
  Error.prototype.toString = function (): string {
    return this.name + ": " + this.message;
  };
}

(Error as any) = ErrorImpl;
(TypeError as any) = TypeErrorImpl;
(RangeError as any) = RangeErrorImpl;
(ReferenceError as any) = ReferenceErrorImpl;
(SyntaxError as any) = SyntaxErrorImpl;
(EvalError as any) = EvalErrorImpl;
(URIError as any) = URIErrorImpl;
