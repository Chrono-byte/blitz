import type * as Context from "effect/Context";
import type * as Duration from "effect/Duration";
import type * as Effect from "effect/Effect";
import type * as Fiber from "effect/Fiber";
import type * as Layer from "effect/Layer";
import type * as Scope from "effect/Scope";

export interface HttpServerError {
  readonly _tag: "HttpServerError";
  readonly reason:
    | "InvalidOptions"
    | "Bind"
    | "Transport"
    | "Shutdown"
    | "Handler"
    | "Body"
    | "Limit";
  readonly operation: "listen" | "serve" | "shutdown";
  readonly message: string;
}

export interface HttpBodyError {
  readonly _tag: "HttpBodyError";
  readonly reason: "Limit" | "Disconnect" | "Transport";
  readonly message: string;
}

export interface RequestContext {
  readonly requestId: string;
  readonly remoteAddress: {
    readonly hostname: string;
    readonly port: number;
  };
  readonly fork: <Success, Error, Requirements>(
    effect: Effect.Effect<Success, Error, Requirements>,
  ) => Effect.Effect<Fiber.RuntimeFiber<Success, Error>, never, Requirements>;
}

export interface HttpServerResource {
  readonly localAddress: string;
  readonly shutdown: Effect.Effect<void, never>;
}

export type Handler<Error = never, Requirements = never> = (
  request: Request,
  context: RequestContext,
) => Effect.Effect<Response, Error, Requirements>;

export interface ServeOptions<Error, Requirements, MapperRequirements = never> {
  readonly hostname?: string;
  readonly port?: number;
  readonly handler: Handler<Error, Requirements>;
  readonly onHandlerError?: (
    error: Error,
    request: Request,
    context: RequestContext,
  ) => Effect.Effect<Response, never, MapperRequirements>;
  readonly requestTimeout?: Duration.DurationInput;
  readonly maxRequestBodyBytes?: number;
  readonly maxResponseBodyBytes?: number;
  readonly queueCapacity?: number;
}

export interface HttpServerService {
  readonly listen: <Error, Requirements, MapperRequirements = never>(
    options: ServeOptions<Error, Requirements, MapperRequirements>,
  ) => Effect.Effect<
    HttpServerResource,
    HttpServerError,
    Scope.Scope | Requirements | MapperRequirements
  >;
  readonly serve: <Error, Requirements, MapperRequirements = never>(
    options: ServeOptions<Error, Requirements, MapperRequirements>,
  ) => Effect.Effect<
    never,
    HttpServerError,
    Requirements | MapperRequirements
  >;
}

export type HttpServer = "blitz:http/HttpServer";

export declare const HttpServer: Context.Tag<HttpServer, HttpServerService>;

export declare const layerLive: Layer.Layer<HttpServer>;

export declare const listen: <Error, Requirements, MapperRequirements = never>(
  options: ServeOptions<Error, Requirements, MapperRequirements>,
) => Effect.Effect<
  HttpServerResource,
  HttpServerError,
  HttpServer | Scope.Scope | Requirements | MapperRequirements
>;

export declare const serve: <Error, Requirements, MapperRequirements = never>(
  options: ServeOptions<Error, Requirements, MapperRequirements>,
) => Effect.Effect<
  never,
  HttpServerError,
  HttpServer | Requirements | MapperRequirements
>;
