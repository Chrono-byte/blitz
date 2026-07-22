import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Transport from "blitz-internal:app-transport";

const buildLayers = (layers) => layers.length === 0
  ? Effect.succeed(undefined)
  : Layer.build(Layer.mergeAll(...layers));

const start = (app, options) => Effect.flatMap(
  buildLayers(app.layers),
  () => Transport.openAppServer({
    ...options,
    rpc: app.rpc,
    layers: app.layers
  })
);

const makeDescription = (rpc, layers) => {
  const description = {
    rpc,
    layers,
    provide(layer) {
      return makeDescription(this.rpc, Object.freeze(this.layers.concat([layer])));
    },
    serve(options = {}) {
      const transportOptions = Object.freeze({
        hostname: options.hostname === undefined ? "127.0.0.1" : options.hostname,
        port: options.port === undefined ? 8000 : options.port,
        rpcPath: options.rpcPath === undefined ? "/rpc" : options.rpcPath
      });
      return Effect.scoped(
        Effect.acquireRelease(
          start(this, transportOptions),
          (server) => Transport.closeAppServer(server)
        ).pipe(Effect.flatMap((server) => Transport.awaitShutdown(server)))
      );
    }
  };
  return Object.freeze(description);
};

export const App = Object.freeze({
  make({ rpc }) {
    return makeDescription(rpc, Object.freeze([]));
  },
  provide(layer) {
    return (app) => app.provide(layer);
  }
});
