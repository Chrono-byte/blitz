import * as Context from "effect/Context";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Scope from "effect/Scope";
import * as Transport from "blitz-internal:http";

const hostOptions = (options, scope) => {
  const onHandlerError = options.onHandlerError;
  const handler = (request, hostContext) => {
    const context = Object.freeze({
      requestId: hostContext.requestId,
      remoteAddress: hostContext.remoteAddress,
      fork: (effect) => Effect.forkIn(effect, scope)
    });
    const effect = options.handler(request, context);
    const handled = onHandlerError === undefined
      ? effect
      : effect.pipe(Effect.catchAll((error) => onHandlerError(error, request, context)));
    return Effect.runSync(handled);
  };
  return Object.freeze({
    hostname: options.hostname,
    port: options.port,
    handler,
    onHandlerError: options.onHandlerError,
    requestTimeout: options.requestTimeout,
    maxRequestBodyBytes: options.maxRequestBodyBytes,
    maxResponseBodyBytes: options.maxResponseBodyBytes,
    queueCapacity: options.queueCapacity
  });
};

const acquireServer = (options) => Effect.contextWithEffect((context) => {
  const scope = Context.get(context, Scope.Scope);
  return Effect.acquireRelease(
    Effect.suspend(() => Transport.openServer(hostOptions(options, scope))),
    (server) => Effect.suspend(() => Transport.closeServer(server))
  );
});

const makeResource = (server) => Object.freeze({
  localAddress: server.localAddress,
  shutdown: Effect.suspend(() => Transport.closeServer(server))
});

const live = Object.freeze({
  listen(options) {
    return Effect.suspend(() => acquireServer(options).pipe(Effect.map(makeResource)));
  },
  serve(options) {
    return Effect.suspend(() => acquireServer(options).pipe(
      Effect.flatMap((server) => Transport.awaitShutdown(server)),
      Effect.scoped
    ));
  }
});

export const HttpServer = Context.GenericTag("blitz:http/HttpServer");

export const layerLive = Layer.succeed(HttpServer, live);

export const listen = Effect.serviceFunctionEffect(
  HttpServer,
  (service) => service.listen
);

export const serve = Effect.serviceFunctionEffect(
  HttpServer,
  (service) => service.serve
);
